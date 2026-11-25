//! Provider 的 OAuth 客户端、PKCE 及 Google 身份校验

use std::{fmt, time::Duration};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use gateway_admin::ports::provider::{ProviderAdminError, ProviderAdminErrorKind};
use gateway_core::account::OutboundProxy;
use reqwest::{Client, RequestBuilder};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use url::Url;

use crate::ConfigError;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OAuthRegistration {
    client_id: String,
    client_secret: String,
    redirect_uri: String,
    resource_scopes: Vec<String>,
    resource_origin: Url,
}

impl fmt::Debug for OAuthRegistration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OAuthRegistration([REDACTED])")
    }
}

impl OAuthRegistration {
    pub(super) fn validate(&self) -> Result<(), ConfigError> {
        let url = Url::parse(&self.redirect_uri).map_err(|_| ConfigError)?;
        let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        let origin = &self.resource_origin;
        let local = matches!(origin.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        if origin.host().is_none()
            || !(origin.scheme() == "https" || local && origin.scheme() == "http")
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
        {
            return Err(ConfigError);
        }
        if !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
            || url.host().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || url.query().is_some()
            || !valid_token(&self.client_id)
            || !valid_token(&self.client_secret)
            || self.resource_scopes.is_empty()
            || self.resource_scopes.iter().any(|scope| {
                !valid_token(scope)
                    || matches!(
                        scope.as_str(),
                        "openid"
                            | "email"
                            | "profile"
                            | "https://www.googleapis.com/auth/userinfo.email"
                            | "https://www.googleapis.com/auth/userinfo.profile"
                    )
            })
        {
            return Err(ConfigError);
        }
        Ok(())
    }
}

pub(crate) fn error(kind: ProviderAdminErrorKind) -> ProviderAdminError {
    ProviderAdminError::new(kind)
}

fn invalid() -> ProviderAdminError {
    error(ProviderAdminErrorKind::Invalid)
}
fn unavailable() -> ProviderAdminError {
    error(ProviderAdminErrorKind::Unavailable)
}
pub(super) fn valid_token(s: &str) -> bool {
    !s.is_empty() && s.len() <= 16384 && s.bytes().all(|b| b.is_ascii_graphic())
}

pub(super) fn random_value() -> Result<String, ProviderAdminError> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| unavailable())?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

pub(crate) struct GoogleOAuth {
    config: OAuthRegistration,
    token_endpoint: String,
    identity_endpoint: String,
    token_info_endpoint: String,
}

enum GrantSource<'a> {
    Authorization,
    Refresh(&'a Material),
    Import(&'a str),
}

#[derive(Deserialize)]
pub(super) struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    token_type: String,
    expires_in: u32,
    scope: Option<String>,
}

#[derive(Deserialize)]
struct UserInfo {
    id: String,
    email: Option<String>,
    #[serde(default)]
    verified_email: bool,
}

#[derive(Deserialize)]
struct TokenInfo {
    scope: String,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Material {
    pub(super) registration: String,
    pub(super) subject: String,
    pub(crate) access_token: String,
    pub(super) refresh_token: String,
    pub(super) scopes: Vec<String>,
}

pub(super) struct VerifiedGrant {
    pub(super) material: Material,
    pub(super) email: Option<String>,
    pub(super) expires_at: DateTime<Utc>,
}

impl TokenResponse {
    /// 首次授权须明确获授业务范围；刷新只允许从同一注册已验证的凭据继承省略项
    pub(super) fn resolve_grant(
        &mut self,
        required_scopes: &[String],
        previous: Option<&Material>,
    ) -> Result<(Vec<String>, String), ProviderAdminError> {
        if !self.token_type.eq_ignore_ascii_case("Bearer")
            || !valid_token(&self.access_token)
            || self.expires_in == 0
        {
            return Err(invalid());
        }
        // Google 可在刷新响应中省略原范围和 refresh token，显式收窄的范围始终优先
        let scopes = self
            .scope
            .as_deref()
            .map(|s| {
                s.split_ascii_whitespace()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .or_else(|| previous.map(|p| p.scopes.clone()))
            .ok_or_else(invalid)?;
        if !required_scopes.iter().all(|s| scopes.contains(s)) {
            return Err(error(if previous.is_some() {
                ProviderAdminErrorKind::CredentialRefreshRequired
            } else {
                ProviderAdminErrorKind::Invalid
            })
            .with_public_message("Google 未授予该客户端配置的权限范围"));
        }
        let refresh_token = self
            .refresh_token
            .take()
            .or_else(|| previous.map(|p| p.refresh_token.clone()))
            .filter(|s| valid_token(s))
            .ok_or_else(|| invalid().with_public_message("未取得可续期凭据，请重新授权"))?;
        Ok((scopes, refresh_token))
    }
}

impl GoogleOAuth {
    pub(crate) fn for_provider() -> Result<Self, ConfigError> {
        // 注册参数来自 Antigravity Desktop 2.34.0 的个人账号客户端
        // client secret 随桌面制品公开分发，不是用户账号凭据
        let registration = OAuthRegistration {
            client_id: "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com"
                .into(),
            client_secret: "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".into(),
            redirect_uri: "http://localhost:8085/auth/callback".into(),
            resource_scopes: [
                "https://www.googleapis.com/auth/cloud-platform",
                "https://www.googleapis.com/auth/cclog",
                "https://www.googleapis.com/auth/experimentsandconfigs",
                "https://www.googleapis.com/auth/aicode",
            ]
            .map(str::to_owned)
            .into(),
            resource_origin: Url::parse("https://daily-cloudcode-pa.googleapis.com")
                .map_err(|_| ConfigError)?,
        };
        registration.validate()?;
        Ok(Self::new(registration))
    }

    pub(super) fn new(config: OAuthRegistration) -> Self {
        Self::with_endpoints(
            config,
            "https://oauth2.googleapis.com/token",
            "https://www.googleapis.com/oauth2/v2/userinfo",
            "https://www.googleapis.com/oauth2/v3/tokeninfo",
        )
    }

    pub(super) fn with_endpoints(
        config: OAuthRegistration,
        token_endpoint: &str,
        identity_endpoint: &str,
        token_info_endpoint: &str,
    ) -> Self {
        Self {
            config,
            token_endpoint: token_endpoint.into(),
            identity_endpoint: identity_endpoint.into(),
            token_info_endpoint: token_info_endpoint.into(),
        }
    }

    pub(crate) fn resource_origin(&self) -> &Url {
        &self.config.resource_origin
    }

    pub(crate) fn registration(&self) -> String {
        // 密钥轮换不改变注册身份；更换客户端、回调或申请范围需要重新授权
        let mut digest = Sha256::new();
        let mut scopes: Vec<_> = self
            .config
            .resource_scopes
            .iter()
            .map(String::as_str)
            .collect();
        scopes.sort_unstable();
        scopes.dedup();
        for part in [
            self.config.client_id.as_str(),
            self.config.redirect_uri.as_str(),
            self.config.resource_origin.as_str(),
        ]
        .into_iter()
        .chain(scopes)
        {
            digest.update(part.as_bytes());
            digest.update([0]);
        }
        URL_SAFE_NO_PAD.encode(digest.finalize())
    }

    pub(super) fn authorize_url(
        &self,
        state: &str,
        verifier: &str,
    ) -> Result<String, ProviderAdminError> {
        let mut url =
            Url::parse("https://accounts.google.com/o/oauth2/auth").map_err(|_| unavailable())?;
        let scopes = [
            "https://www.googleapis.com/auth/userinfo.email",
            "https://www.googleapis.com/auth/userinfo.profile",
        ]
        .into_iter()
        .chain(self.config.resource_scopes.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ");
        url.query_pairs_mut().extend_pairs([
            ("response_type", "code"),
            ("client_id", &self.config.client_id),
            ("redirect_uri", &self.config.redirect_uri),
            ("scope", &scopes),
            ("state", state),
            (
                "code_challenge",
                &URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
            ),
            ("code_challenge_method", "S256"),
            ("access_type", "offline"),
            ("prompt", "consent"),
        ]);
        Ok(url.into())
    }

    pub(super) fn callback_code(
        &self,
        callback: &str,
        state: &str,
    ) -> Result<String, ProviderAdminError> {
        let mut url = Url::parse(callback).map_err(|_| invalid())?;
        if url.fragment().is_some() {
            return Err(invalid());
        }
        let mut code = None;
        let mut returned_state = None;
        for (name, value) in url.query_pairs() {
            match name.as_ref() {
                "error" => return Err(invalid()),
                "state" if returned_state.is_none() => returned_state = Some(value.into_owned()),
                "code" if code.is_none() && valid_token(&value) => code = Some(value.into_owned()),
                "state" | "code" => return Err(invalid()),
                _ => {}
            }
        }
        url.set_query(None);
        if returned_state.as_deref() != Some(state)
            || url != Url::parse(&self.config.redirect_uri).map_err(|_| invalid())?
        {
            return Err(invalid());
        }
        code.ok_or_else(invalid)
    }

    pub(super) async fn exchange(
        &self,
        code: &str,
        verifier: &str,
        proxy: Option<&OutboundProxy>,
        profile: &crate::profile::ClientProfile,
    ) -> Result<VerifiedGrant, ProviderAdminError> {
        self.request(
            &[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("code_verifier", verifier),
                ("redirect_uri", &self.config.redirect_uri),
            ],
            GrantSource::Authorization,
            proxy,
            profile,
        )
        .await
    }

    pub(super) async fn refresh(
        &self,
        previous: &Material,
        proxy: Option<&OutboundProxy>,
        profile: &crate::profile::ClientProfile,
    ) -> Result<VerifiedGrant, ProviderAdminError> {
        if self.registration() != previous.registration || !valid_token(&previous.refresh_token) {
            return Err(error(ProviderAdminErrorKind::CredentialRefreshRequired));
        }
        self.request(
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", &previous.refresh_token),
            ],
            GrantSource::Refresh(previous),
            proxy,
            profile,
        )
        .await
    }

    pub(super) async fn verify_import(
        &self,
        refresh_token: &str,
        proxy: Option<&OutboundProxy>,
        profile: &crate::profile::ClientProfile,
    ) -> Result<VerifiedGrant, ProviderAdminError> {
        if !valid_token(refresh_token) {
            return Err(invalid());
        }
        self.request(
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
            ],
            GrantSource::Import(refresh_token),
            proxy,
            profile,
        )
        .await
    }

    async fn request(
        &self,
        fields: &[(&str, &str)],
        source: GrantSource<'_>,
        proxy: Option<&OutboundProxy>,
        profile: &crate::profile::ClientProfile,
    ) -> Result<VerifiedGrant, ProviderAdminError> {
        let client: Client = gateway_egress::http_client(proxy)
            .map_err(|_| invalid())?
            .use_rustls_tls()
            // OAuth 整轮请求冻结系统 AGY 画像，沿用 Desktop 的 HTTP/1.1 传输
            .http1_only()
            .http1_title_case_headers()
            .user_agent(profile.user_agent())
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|_| unavailable())?;
        let mut form = fields.to_vec();
        form.extend([
            ("client_id", self.config.client_id.as_str()),
            ("client_secret", self.config.client_secret.as_str()),
        ]);
        let mut token: TokenResponse = read_json(
            client.post(&self.token_endpoint).form(&form),
            !matches!(source, GrantSource::Authorization),
        )
        .await?;
        let previous = match source {
            GrantSource::Refresh(previous) => Some(previous),
            GrantSource::Authorization => None,
            GrantSource::Import(refresh_token) => {
                // Google 已用当前注册验证 RT，但文件中的身份和权限不可作为继承来源
                token
                    .refresh_token
                    .get_or_insert_with(|| refresh_token.to_owned());
                None
            }
        };
        // Desktop 通过 tokeninfo 查询实际获授范围；新凭据不能从导入文件继承权限
        if previous.is_none() && token.scope.is_none() {
            let info: TokenInfo = read_json(
                client
                    .get(&self.token_info_endpoint)
                    .query(&[("access_token", &token.access_token)]),
                false,
            )
            .await?;
            token.scope = Some(info.scope);
        }
        let (scopes, refresh_token) =
            token.resolve_grant(&self.config.resource_scopes, previous)?;
        // 刷新凭据属于已验证的注册与 subject。RT 可能发生轮换，拿到新 RT 后直接提交，
        // 避免后续资料查询失败令新旧凭据同时丢失。新的授权码及文件导入必须重新查验身份
        let (subject, email) = if let Some(previous) = previous {
            (previous.subject.clone(), None)
        } else {
            let user: UserInfo = read_json(
                client
                    .get(&self.identity_endpoint)
                    .bearer_auth(&token.access_token),
                false,
            )
            .await?;
            if user.id.len() > 255 || !valid_token(&user.id) {
                return Err(invalid());
            }
            (
                user.id,
                user.email.filter(|s| {
                    user.verified_email
                        && !s.is_empty()
                        && s.len() <= 320
                        && !s.chars().any(char::is_control)
                }),
            )
        };
        Ok(VerifiedGrant {
            email,
            expires_at: Utc::now() + chrono::Duration::seconds(i64::from(token.expires_in)),
            material: Material {
                registration: self.registration(),
                subject,
                access_token: token.access_token,
                refresh_token,
                scopes,
            },
        })
    }
}

async fn read_json<T: DeserializeOwned>(
    request: RequestBuilder,
    refresh: bool,
) -> Result<T, ProviderAdminError> {
    let mut response = request
        .send()
        .await
        .map_err(|e| unavailable().with_source(e.without_url()))?;
    let status = response.status();
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
        if data.len().saturating_add(chunk.len()) > 65536 {
            return Err(invalid());
        }
        data.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        #[derive(Deserialize)]
        struct OAuthFailure {
            error: String,
        }
        if refresh
            && status == reqwest::StatusCode::BAD_REQUEST
            && serde_json::from_slice::<OAuthFailure>(&data)
                .is_ok_and(|body| body.error == "invalid_grant")
        {
            return Err(error(ProviderAdminErrorKind::CredentialRefreshRequired)
                .with_public_message("Google 已撤销刷新授权，请重新授权"));
        }
        return Err(if status.is_server_error() || status.as_u16() == 429 {
            unavailable()
        } else {
            invalid()
        });
    }
    serde_json::from_slice(&data).map_err(|_| invalid())
}
