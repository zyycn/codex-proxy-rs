use std::sync::Arc;

use crate::credential::token::{parse_oauth_error, parse_refresh_success, parse_token_success};
use crate::{
    AllowedRedirectUri, AuthorizationCodeGrant, DiscoveryDocument, FormField, GrokOAuthConfig,
    HttpHeader, OAuthError, OAuthHttpRequest, OAuthHttpResponse, OAuthHttpTransport,
    OAuthOperation, OAuthPrincipal, PendingAuthorization, RefreshTokenGrant, RefreshedTokenSet,
    TokenCandidate, TokenVerificationContext, TokenVerifier, VerificationFailure, VerificationFlow,
    VerificationMethod, VerifiedTokenSet,
};

/// Transport-agnostic official Grok Build OAuth protocol client.
#[derive(Clone)]
pub struct GrokOAuthClient {
    config: GrokOAuthConfig,
    transport: Arc<dyn OAuthHttpTransport>,
    verifier: Arc<dyn TokenVerifier>,
}

impl GrokOAuthClient {
    /// Creates a client with explicit HTTP and token verification trust ports.
    #[must_use]
    pub fn new(
        config: GrokOAuthConfig,
        transport: Arc<dyn OAuthHttpTransport>,
        verifier: Arc<dyn TokenVerifier>,
    ) -> Self {
        Self {
            config,
            transport,
            verifier,
        }
    }

    /// Returns the immutable official provider configuration.
    #[must_use]
    pub const fn config(&self) -> &GrokOAuthConfig {
        &self.config
    }

    /// Fetches and validates official same-origin OIDC discovery.
    ///
    /// # Errors
    ///
    /// Fails on transport errors, non-success status, malformed JSON, issuer
    /// mismatch, cross-origin endpoints, missing JWKS, or insecure algorithms.
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

    /// Starts Authorization Code + PKCE without performing network I/O.
    ///
    /// # Errors
    ///
    /// Returns an entropy error if secure random state, nonce, or verifier
    /// generation fails.
    pub fn start_authorization_code(
        &self,
        discovery: &DiscoveryDocument,
        redirect_uri: AllowedRedirectUri,
        principal: Option<&OAuthPrincipal>,
    ) -> Result<PendingAuthorization, OAuthError> {
        PendingAuthorization::start(&self.config, discovery, redirect_uri, principal)
    }

    /// Exchanges a state-validated authorization grant exactly once, then
    /// requires nonce-bound ID-token verification before returning credentials.
    ///
    /// # Errors
    ///
    /// Fails on transport/protocol errors, missing ID token, failed verification,
    /// or any non-success OAuth response. No automatic retry is performed.
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

    /// Exchanges a refresh token once. The caller must serialize refreshes and
    /// persist rotated tokens through credential-revision CAS.
    ///
    /// # Errors
    ///
    /// Returns a classified OAuth error. Ambiguous transport failures must not
    /// be retried automatically because the refresh token may have rotated.
    pub async fn refresh(
        &self,
        discovery: &DiscoveryDocument,
        grant: &RefreshTokenGrant,
    ) -> Result<RefreshedTokenSet, OAuthError> {
        let mut form = vec![
            FormField::public("grant_type", "refresh_token"),
            FormField::secret("refresh_token", grant.refresh_token().clone()),
            FormField::public("client_id", self.config.client_id()),
        ];
        if let Some(principal) = grant.principal() {
            form.push(FormField::public(
                "principal_type",
                principal.principal_type(),
            ));
            form.push(FormField::secret(
                "principal_id",
                crate::SecretValue::new(principal.principal_id().to_owned()),
            ));
        }
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
        HttpHeader::new("x-grok-client-version", self.config.client_version())
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
