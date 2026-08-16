//! OpenAI token 续期 Reqwest 适配器。

use async_trait::async_trait;
use reqwest::{Client, StatusCode, redirect::Policy};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use std::fmt;
use std::time::Duration;

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
    /// 官方刷新响应省略时由持久化调用方保留当前 access token。
    pub access_token: Option<String>,
    /// 官方刷新响应省略时由持久化调用方保留当前 refresh token。
    pub refresh_token: Option<String>,
    /// 官方刷新响应省略时由持久化调用方保留当前 ID token。
    pub id_token: Option<String>,
    /// OAuth endpoint 可以省略或返回零 expiry；这不影响已经成功的 token
    /// exchange，只意味着调用方不能据此安排下一次刷新。
    pub expires_in: Option<Duration>,
}

impl fmt::Debug for TokenPair {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TokenPair")
            .field(
                "access_token",
                &self.access_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("id_token", &self.id_token.as_ref().map(|_| "[REDACTED]"))
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

/// Codex token 刷新的稳定失败分类。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RefreshFailure {
    #[error("refresh token is invalid or expired")]
    InvalidGrant { message: Option<String> },
    #[error("account is banned")]
    Banned { message: Option<String> },
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

/// 官方 token endpoint 返回的 token set。
///
/// 与官方首次 authorization-code exchange 一致：`id_token`、`access_token` 与
/// `refresh_token` 都是响应的必填字段。这里不检查 token 内容、签名或 claims。
pub struct AuthorizationTokenSet {
    pub secret: crate::credential::CodexOAuthSecret,
    pub id_token: SecretString,
    pub expires_in: Option<Duration>,
}

impl fmt::Debug for AuthorizationTokenSet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthorizationTokenSet")
            .field("secret", &"[REDACTED]")
            .field("id_token", &"[REDACTED]")
            .field("expires_in", &self.expires_in)
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

/// 构建禁止 redirect 且无自动重试的 Codex token client。
///
/// # Errors
///
/// 本地 TLS/HTTP client 初始化失败时返回脱敏错误。
pub fn openai_token_client(
    config: TokenClientConfig,
) -> Result<OpenAiTokenClient, TokenClientBuildError> {
    let client = Client::builder()
        .redirect(Policy::none())
        .connect_timeout(TOKEN_CONNECT_TIMEOUT)
        .timeout(TOKEN_REQUEST_TIMEOUT)
        .build()
        .map_err(|_| TokenClientBuildError)?;
    Ok(OpenAiTokenClient::new(client, config))
}

/// 构建固定官方 endpoint 的 Codex token client。
pub fn official_openai_token_client() -> Result<OpenAiTokenClient, TokenClientBuildError> {
    openai_token_client(TokenClientConfig {
        client_id: OFFICIAL_CODEX_OAUTH_CLIENT_ID.to_owned(),
        token_endpoint: OFFICIAL_CODEX_TOKEN_ENDPOINT.to_owned(),
    })
}

#[derive(Deserialize)]
struct RefreshTokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_in: Option<u64>,
}

#[derive(Deserialize)]
struct RefreshErrorResponse {
    error: Option<RefreshError>,
    code: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RefreshError {
    Details(RefreshErrorDetails),
    Code(String),
}

#[derive(Deserialize)]
struct RefreshErrorDetails {
    code: Option<String>,
    message: Option<String>,
}

impl RefreshErrorResponse {
    fn code(&self) -> Option<&str> {
        match self.error.as_ref() {
            Some(RefreshError::Details(error)) => error.code.as_deref(),
            Some(RefreshError::Code(code)) => Some(code.as_str()),
            None => None,
        }
        .or(self.code.as_deref())
    }

    fn message(&self) -> Option<String> {
        match self.error.as_ref() {
            Some(RefreshError::Details(error)) => error.message.as_deref(),
            Some(RefreshError::Code(_)) | None => None,
        }
        .filter(|message| !message.trim().is_empty())
        .map(ToOwned::to_owned)
    }
}

#[derive(Deserialize)]
struct AuthorizationCodeResponse {
    access_token: String,
    refresh_token: String,
    id_token: String,
    expires_in: Option<u64>,
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
        let (status, body) = read_bounded_response(response)
            .await
            .map_err(|_| AuthorizationCodeExchangeError::Ambiguous)?;
        if !status.is_success() {
            return Err(match status.as_u16() {
                429 | 500..=599 => AuthorizationCodeExchangeError::Unavailable,
                _ => AuthorizationCodeExchangeError::Rejected,
            });
        }
        let tokens = serde_json::from_slice::<AuthorizationCodeResponse>(&body)
            .map_err(|_| AuthorizationCodeExchangeError::Rejected)?;
        let id_token = SecretString::from(tokens.id_token);
        Ok(AuthorizationTokenSet {
            secret: crate::credential::CodexOAuthSecret {
                access_token: SecretString::from(tokens.access_token),
                refresh_token: Some(SecretString::from(tokens.refresh_token)),
                id_token: None,
            },
            id_token,
            expires_in: optional_expiry(tokens.expires_in),
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
    if tokens
        .id_token
        .as_deref()
        .is_some_and(|token| super::types::parse_chatgpt_jwt_claims(token).is_err())
    {
        return Err(());
    }
    Ok(TokenPair {
        access_token: tokens.access_token,
        // OAuth 刷新响应可能省略未变更的 RT；缺失时由调用方保留当前值。
        refresh_token: tokens.refresh_token,
        // 官方刷新响应允许省略 ID token；缺失时由调用方保留当前值。
        id_token: tokens.id_token,
        expires_in: optional_expiry(tokens.expires_in),
    })
}

fn optional_expiry(value: Option<u64>) -> Option<Duration> {
    value
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
}

fn classify_refresh_failure(status: StatusCode, body: &[u8]) -> RefreshFailure {
    // 5xx 与 429 按状态码判为瞬态：其正文（CDN/网关页等）不是权威 OAuth 错误响应，
    // 不据此终态失效账号。
    if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
        return RefreshFailure::Transport;
    }
    // 官方刷新错误的消息与错误码分别位于 `error.message`、`error.code`；
    // `error` 字符串与顶层 `code` 仅用于兼容官方客户端自身的错误码提取契约。
    let Ok(error) = serde_json::from_slice::<RefreshErrorResponse>(body) else {
        return RefreshFailure::Transport;
    };
    let message = error.message();
    if message.as_deref().is_some_and(|message| {
        message
            .to_ascii_lowercase()
            .contains("account has been deactivated")
    }) {
        return RefreshFailure::Banned { message };
    }
    let normalized_code = error.code().map(str::to_ascii_lowercase);
    if matches!(
        normalized_code.as_deref(),
        Some(
            "invalid_grant"
                | "invalid_token"
                | "access_denied"
                | "refresh_token_expired"
                | "refresh_token_reused"
                | "refresh_token_invalidated"
        )
    ) {
        return RefreshFailure::InvalidGrant { message };
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
