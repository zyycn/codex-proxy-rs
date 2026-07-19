//! OpenAI token 续期 Reqwest 适配器。

use async_trait::async_trait;
use reqwest::{Client, StatusCode, redirect::Policy};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use std::fmt;
use std::time::Duration;

const MAX_TOKEN_LIFETIME_SECONDS: u64 = 366 * 24 * 60 * 60;
const MAX_OAUTH_RESPONSE_BYTES: usize = 64 * 1024;
const TOKEN_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const TOKEN_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Codex Desktop 使用的官方 OAuth public client。
pub const OFFICIAL_CODEX_OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Codex Desktop 使用的官方 token endpoint。
pub const OFFICIAL_CODEX_TOKEN_ENDPOINT: &str = "https://auth.openai.com/oauth/token";
/// Codex Desktop loopback callback；管理员复制完整回调 URL 交回固定 complete API。
pub const OFFICIAL_CODEX_REDIRECT_URI: &str = "http://localhost:1455/auth/callback";

/// Token 刷新成功后得到的认证材料。
#[derive(Clone)]
pub struct TokenPair {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Duration,
}

impl fmt::Debug for TokenPair {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TokenPair")
            .field("access_token", &"[REDACTED]")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

/// Codex token 刷新的稳定失败分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RefreshFailure {
    #[error("refresh token is invalid or expired")]
    InvalidGrant,
    #[error("account is banned")]
    Banned,
    #[error("refresh transport failed before server processing")]
    RetryableTransport,
    #[error("refresh transport failed after possible server processing")]
    Transport,
}

/// Codex token 刷新端口。
#[async_trait]
pub trait TokenRefresher: Send + Sync + 'static {
    async fn refresh(&self, refresh_token: &str) -> Result<TokenPair, RefreshFailure>;
}

/// Authorization Code + PKCE 的一次性 grant。
pub struct AuthorizationCodeGrant {
    pub code: SecretString,
    pub code_verifier: SecretString,
}

impl fmt::Debug for AuthorizationCodeGrant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthorizationCodeGrant")
            .field("code", &"[REDACTED]")
            .field("code_verifier", &"[REDACTED]")
            .finish()
    }
}

/// 官方 token endpoint 返回、尚待 OIDC/JWKS 绑定校验的 token set。
pub struct AuthorizationTokenSet {
    pub secret: crate::credential::CodexOAuthSecret,
    pub id_token: SecretString,
}

impl fmt::Debug for AuthorizationTokenSet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthorizationTokenSet")
            .field("secret", &"[REDACTED]")
            .field("id_token", &"[REDACTED]")
            .finish()
    }
}

/// Authorization Code exchange 的低基数失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AuthorizationCodeExchangeError {
    #[error("authorization code was rejected")]
    Rejected,
    #[error("authorization code exchange is unavailable")]
    Unavailable,
    #[error("authorization code exchange send state is ambiguous")]
    Ambiguous,
}

#[async_trait]
pub trait AuthorizationCodeExchanger: Send + Sync + 'static {
    async fn exchange_authorization_code(
        &self,
        grant: AuthorizationCodeGrant,
    ) -> Result<AuthorizationTokenSet, AuthorizationCodeExchangeError>;
}

/// OpenAI token 续期客户端配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenClientConfig {
    /// OpenAI 客户端 ID。
    pub client_id: String,
    /// Token 交换入口。
    pub token_endpoint: String,
}

/// OpenAI token 续期客户端。
#[derive(Clone)]
pub struct OpenAiTokenClient {
    client: Client,
    config: TokenClientConfig,
}

/// 官方 Codex token client 无法安全构建。
#[derive(Debug, thiserror::Error)]
#[error("official Codex token client could not be built")]
pub struct TokenClientBuildError;

impl OpenAiTokenClient {
    /// 使用 Reqwest 客户端和静态配置构造 token 续期客户端。
    pub fn new(client: Client, config: TokenClientConfig) -> Self {
        Self { client, config }
    }
}

/// 构建固定官方 endpoint、禁止 redirect 且无自动重试的 Codex token client。
///
/// # Errors
///
/// 本地 TLS/HTTP client 初始化失败时返回脱敏错误。
pub fn official_openai_token_client() -> Result<OpenAiTokenClient, TokenClientBuildError> {
    let client = Client::builder()
        .redirect(Policy::none())
        .connect_timeout(TOKEN_CONNECT_TIMEOUT)
        .timeout(TOKEN_REQUEST_TIMEOUT)
        .build()
        .map_err(|_| TokenClientBuildError)?;
    Ok(OpenAiTokenClient::new(
        client,
        TokenClientConfig {
            client_id: OFFICIAL_CODEX_OAUTH_CLIENT_ID.to_owned(),
            token_endpoint: OFFICIAL_CODEX_TOKEN_ENDPOINT.to_owned(),
        },
    ))
}

#[derive(Deserialize)]
struct RefreshTokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
}

#[derive(Deserialize)]
struct AuthorizationCodeResponse {
    access_token: String,
    refresh_token: Option<String>,
    id_token: String,
    token_type: String,
    expires_in: u64,
}

#[async_trait]
impl TokenRefresher for OpenAiTokenClient {
    async fn refresh(&self, refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        let response = self
            .client
            .post(&self.config.token_endpoint)
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", self.config.client_id.as_str()),
                ("refresh_token", refresh_token),
            ])
            .send()
            .await
            .map_err(|error| refresh_transport_failure(&error))?;
        let (status, body) = read_bounded_response(response).await?;
        if !status.is_success() {
            return Err(classify_refresh_failure(status, &body));
        }
        parse_token_pair(&body).map_err(|()| RefreshFailure::Transport)
    }
}

#[async_trait]
impl AuthorizationCodeExchanger for OpenAiTokenClient {
    async fn exchange_authorization_code(
        &self,
        grant: AuthorizationCodeGrant,
    ) -> Result<AuthorizationTokenSet, AuthorizationCodeExchangeError> {
        let response = self
            .client
            .post(&self.config.token_endpoint)
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", self.config.client_id.as_str()),
                ("code", grant.code.expose_secret()),
                ("redirect_uri", OFFICIAL_CODEX_REDIRECT_URI),
                ("code_verifier", grant.code_verifier.expose_secret()),
            ])
            .send()
            .await
            .map_err(|error| {
                if is_safe_to_retry_refresh_transport(&error) {
                    AuthorizationCodeExchangeError::Unavailable
                } else {
                    AuthorizationCodeExchangeError::Ambiguous
                }
            })?;
        let success_is_json = response.status().is_success()
            && response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.split(';').next())
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"));
        let (status, body) = read_bounded_response(response)
            .await
            .map_err(|_| AuthorizationCodeExchangeError::Ambiguous)?;
        if !status.is_success() {
            return Err(match status.as_u16() {
                429 | 500..=599 => AuthorizationCodeExchangeError::Unavailable,
                _ => AuthorizationCodeExchangeError::Rejected,
            });
        }
        if !success_is_json {
            return Err(AuthorizationCodeExchangeError::Rejected);
        }
        let tokens = serde_json::from_slice::<AuthorizationCodeResponse>(&body)
            .map_err(|_| AuthorizationCodeExchangeError::Rejected)?;
        if tokens.access_token.is_empty()
            || tokens.id_token.is_empty()
            || !tokens.token_type.eq_ignore_ascii_case("bearer")
            || tokens.expires_in == 0
            || tokens.expires_in > MAX_TOKEN_LIFETIME_SECONDS
            || tokens
                .refresh_token
                .as_deref()
                .is_some_and(|token| token.is_empty())
        {
            return Err(AuthorizationCodeExchangeError::Rejected);
        }
        Ok(AuthorizationTokenSet {
            secret: crate::credential::CodexOAuthSecret {
                access_token: SecretString::from(tokens.access_token),
                refresh_token: tokens.refresh_token.map(SecretString::from),
                id_token: None,
            },
            id_token: SecretString::from(tokens.id_token),
        })
    }
}

async fn read_bounded_response(
    mut response: reqwest::Response,
) -> Result<(StatusCode, Vec<u8>), RefreshFailure> {
    let status = response.status();
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| RefreshFailure::Transport)?
    {
        let next_len = body
            .len()
            .checked_add(chunk.len())
            .filter(|length| *length <= MAX_OAUTH_RESPONSE_BYTES)
            .ok_or(RefreshFailure::Transport)?;
        body.reserve(next_len.saturating_sub(body.len()));
        body.extend_from_slice(&chunk);
    }
    Ok((status, body))
}

fn parse_token_pair(body: &[u8]) -> Result<TokenPair, ()> {
    let tokens = serde_json::from_slice::<RefreshTokenResponse>(body).map_err(|_| ())?;
    if tokens.access_token.trim().is_empty() {
        return Err(());
    }
    if tokens.expires_in == 0 || tokens.expires_in > MAX_TOKEN_LIFETIME_SECONDS {
        return Err(());
    }
    Ok(TokenPair {
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        expires_in: Duration::from_secs(tokens.expires_in),
    })
}

fn classify_refresh_failure(_status: StatusCode, body: &[u8]) -> RefreshFailure {
    let lower = String::from_utf8_lossy(body).to_ascii_lowercase();
    if lower.contains("account has been deactivated") {
        return RefreshFailure::Banned;
    }
    if lower.contains("invalid_grant")
        || lower.contains("invalid_token")
        || lower.contains("access_denied")
        || lower.contains("refresh_token_expired")
        || lower.contains("refresh_token_reused")
    {
        return RefreshFailure::InvalidGrant;
    }
    RefreshFailure::Transport
}

fn refresh_transport_failure(error: &reqwest::Error) -> RefreshFailure {
    if is_safe_to_retry_refresh_transport(error) {
        RefreshFailure::RetryableTransport
    } else {
        RefreshFailure::Transport
    }
}

fn is_safe_to_retry_refresh_transport(error: &reqwest::Error) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("econnrefused")
        || message.contains("could not resolve proxy")
        || message.contains("could not resolve host")
        || message.contains("curl exited with code 5")
        || message.contains("curl exited with code 6")
        || message.contains("curl exited with code 7")
        || message.contains("curl exited with code 35")
        || message.contains("dns error")
        || message.contains("connection refused")
        || message.contains("network is unreachable")
        || message.contains("tls handshake")
}
