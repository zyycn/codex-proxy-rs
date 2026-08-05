use std::sync::Arc;

use crate::credential::token::{parse_oauth_error, parse_refresh_success, parse_token_success};
use crate::{
    AllowedRedirectUri, AuthorizationCodeGrant, DiscoveryDocument, FormField, GrokOAuthConfig,
    HttpHeader, OAuthError, OAuthHttpRequest, OAuthHttpResponse, OAuthHttpTransport,
    OAuthOperation, OAuthPrincipal, PendingAuthorization, RefreshTokenGrant, RefreshedTokenSet,
    TokenCandidate, TokenVerificationContext, TokenVerifier, VerificationFailure, VerificationFlow,
    VerificationMethod, VerifiedTokenSet,
};

/// 与 transport 无关的官方 Grok Build OAuth 协议客户端。
#[derive(Clone)]
pub struct GrokOAuthClient {
    config: GrokOAuthConfig,
    wire_profile: crate::XaiWireProfileState,
    transport: Arc<dyn OAuthHttpTransport>,
    verifier: Arc<dyn TokenVerifier>,
}

impl GrokOAuthClient {
    /// 创建客户端，显式注入 HTTP 与 token 验证两个信任端口。
    #[must_use]
    pub fn new(
        config: GrokOAuthConfig,
        wire_profile: crate::XaiWireProfileState,
        transport: Arc<dyn OAuthHttpTransport>,
        verifier: Arc<dyn TokenVerifier>,
    ) -> Self {
        Self {
            config,
            wire_profile,
            transport,
            verifier,
        }
    }

    /// 返回不可变的官方 provider 配置。
    #[must_use]
    pub const fn config(&self) -> &GrokOAuthConfig {
        &self.config
    }

    /// 拉取并校验官方同源 OIDC 发现文档。
    ///
    /// # Errors
    ///
    /// transport 失败、非成功状态、JSON 非法、issuer 不匹配、端点跨 origin、
    /// 缺少 JWKS 或算法不安全时返回错误。
    pub async fn discover(&self) -> Result<DiscoveryDocument, OAuthError> {
        let response = self
            .execute(
                OAuthOperation::Discovery,
                OAuthHttpRequest::get(self.config.discovery_url()),
            )
            .await?;
        if !is_success(&response) {
            return Err(OAuthError::HttpStatus {
                operation: OAuthOperation::Discovery,
                status: response.status(),
            });
        }
        DiscoveryDocument::parse(&self.config, response.body())
    }

    /// 启动 Authorization Code + PKCE，不产生网络 I/O。
    ///
    /// # Errors
    ///
    /// 安全随机 state、nonce 或 verifier 生成失败时返回熵不可用错误。
    pub fn start_authorization_code(
        &self,
        discovery: &DiscoveryDocument,
        redirect_uri: AllowedRedirectUri,
        principal: Option<&OAuthPrincipal>,
    ) -> Result<PendingAuthorization, OAuthError> {
        PendingAuthorization::start(&self.config, discovery, redirect_uri, principal)
    }

    /// 对已校验 state 的授权 grant 只做一次交换，返回凭据前强制通过
    /// nonce 绑定的 ID token 验证。
    ///
    /// # Errors
    ///
    /// transport/协议错误、缺少 ID token、验证失败或任何非成功 OAuth 响应时
    /// 返回错误。不做自动重试。
    pub async fn exchange_authorization_code(
        &self,
        discovery: &DiscoveryDocument,
        grant: AuthorizationCodeGrant,
    ) -> Result<VerifiedTokenSet, OAuthError> {
        let (code, redirect_uri, code_verifier, nonce) = grant.into_parts();
        let request = OAuthHttpRequest::post(
            discovery.token_endpoint().clone(),
            vec![self.version_header()],
            vec![
                FormField::public("grant_type", "authorization_code"),
                FormField::secret("code", code),
                FormField::public("redirect_uri", redirect_uri.as_url().to_string()),
                FormField::public("client_id", self.config.client_id()),
                FormField::secret("code_verifier", code_verifier),
            ],
        );
        let response = self
            .execute(OAuthOperation::AuthorizationCodeToken, request)
            .await?;
        if !is_success(&response) {
            return Err(parse_oauth_error(
                &response,
                OAuthOperation::AuthorizationCodeToken,
            ));
        }

        let tokens = parse_token_success(&response, OAuthOperation::AuthorizationCodeToken)?;
        if tokens.id_token.is_none() {
            return Err(VerificationFailure::MissingIdToken.into());
        }
        let context = TokenVerificationContext::new(
            VerificationFlow::AuthorizationCode,
            discovery.issuer(),
            self.config.client_id(),
            discovery.jwks_uri(),
            discovery.userinfo_endpoint(),
            discovery.signing_algorithms(),
            Some(&nonce),
        );
        let candidate = TokenCandidate::new(
            &tokens.access_token,
            tokens.id_token.as_ref(),
            tokens.expires_in,
        );
        let evidence = self.verifier.verify(context, candidate).await?;
        if evidence.method() != VerificationMethod::IdToken {
            return Err(VerificationFailure::WrongEvidence.into());
        }

        Ok(VerifiedTokenSet::new(
            tokens,
            evidence,
            self.config.scope_string(),
        ))
    }

    /// 验证已归一化的已有 OAuth credential。有效 AT 由官方 user-info 确认；
    /// 过期 AT 先用 RT 换取新 AT，再由同一官方端点确认。
    ///
    /// # Errors
    ///
    /// 导入 metadata、token wire、刷新、OIDC claim 或 user-info 任一失败时拒绝。
    pub async fn verify_imported_credential(
        &self,
        discovery: &DiscoveryDocument,
        candidate: crate::GrokOAuthImportCandidate,
    ) -> Result<VerifiedTokenSet, crate::GrokOAuthImportError> {
        let validated = candidate.validate(&self.config, chrono::Utc::now())?;
        let scope = validated.scope;
        let mut tokens = validated.tokens;
        let flow = if validated.requires_refresh {
            let refresh_token = tokens
                .refresh_token
                .take()
                .ok_or(crate::GrokOAuthImportError::InvalidField("refresh_token"))?;
            let refreshed = self
                .refresh(discovery, &RefreshTokenGrant::new(refresh_token.clone()))
                .await?;
            tokens = crate::credential::token::UnverifiedTokenSet {
                access_token: refreshed.access_token().clone(),
                refresh_token: Some(
                    refreshed
                        .rotated_refresh_token()
                        .cloned()
                        .unwrap_or(refresh_token),
                ),
                id_token: None,
                expires_in: refreshed.expires_in(),
            };
            VerificationFlow::CredentialImportRefreshed
        } else {
            VerificationFlow::CredentialImport
        };
        let context = TokenVerificationContext::new(
            flow,
            discovery.issuer(),
            self.config.client_id(),
            discovery.jwks_uri(),
            discovery.userinfo_endpoint(),
            discovery.signing_algorithms(),
            None,
        );
        let verification_candidate = TokenCandidate::new(
            &tokens.access_token,
            tokens.id_token.as_ref(),
            tokens.expires_in,
        );
        let evidence = self
            .verifier
            .verify(context, verification_candidate)
            .await
            .map_err(OAuthError::from)?;
        let expected_method = match flow {
            VerificationFlow::CredentialImport | VerificationFlow::CredentialImportRefreshed => {
                VerificationMethod::UserInfo
            }
            VerificationFlow::AuthorizationCode => {
                return Err(crate::GrokOAuthImportError::OAuth(
                    VerificationFailure::WrongEvidence.into(),
                ));
            }
        };
        if evidence.method() != expected_method {
            return Err(crate::GrokOAuthImportError::OAuth(
                VerificationFailure::WrongEvidence.into(),
            ));
        }
        Ok(VerifiedTokenSet::new(tokens, evidence, scope))
    }

    /// 执行一次 refresh token 交换。调用方须串行化刷新，并通过 credential
    /// revision CAS 持久化轮换后的 token。
    ///
    /// # Errors
    ///
    /// 返回已分类的 OAuth 错误。refresh token 可能已轮换，Ambiguous 类
    /// transport 失败不在本次 exchange 内重试，后续交由 refresh scheduler 退避协调。
    pub async fn refresh(
        &self,
        discovery: &DiscoveryDocument,
        grant: &RefreshTokenGrant,
    ) -> Result<RefreshedTokenSet, OAuthError> {
        let form = vec![
            FormField::public("grant_type", "refresh_token"),
            FormField::secret("refresh_token", grant.refresh_token().clone()),
            FormField::public("client_id", self.config.client_id()),
        ];
        let response = self
            .execute(
                OAuthOperation::RefreshToken,
                OAuthHttpRequest::post(
                    discovery.token_endpoint().clone(),
                    vec![self.version_header()],
                    form,
                ),
            )
            .await?;
        if !is_success(&response) {
            return Err(parse_oauth_error(&response, OAuthOperation::RefreshToken));
        }
        parse_refresh_success(&response)
    }

    async fn execute(
        &self,
        operation: OAuthOperation,
        request: OAuthHttpRequest,
    ) -> Result<OAuthHttpResponse, OAuthError> {
        self.transport
            .execute(request)
            .await
            .map_err(|failure| OAuthError::transport(operation, failure))
    }

    fn version_header(&self) -> HttpHeader {
        HttpHeader::new("x-grok-client-version", self.wire_profile.client_version())
    }
}

impl std::fmt::Debug for GrokOAuthClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GrokOAuthClient")
            .field("config", &self.config)
            .field("transport", &"dyn OAuthHttpTransport")
            .field("verifier", &"dyn TokenVerifier")
            .finish()
    }
}

const fn is_success(response: &OAuthHttpResponse) -> bool {
    matches!(response.status(), 200..=299)
}
