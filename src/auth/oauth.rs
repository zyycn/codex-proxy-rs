use async_trait::async_trait;
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use thiserror::Error;

use crate::auth::{
    refresh::{RefreshFailure, TokenRefresher},
    token::TokenPair,
};

const CODEX_OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OPENAI_OAUTH_AUTHORIZE_ENDPOINT: &str = "https://auth.openai.com/oauth/authorize";
const OPENAI_OAUTH_DEVICE_CODE_ENDPOINT: &str = "https://auth.openai.com/oauth/device/code";
const OPENAI_OAUTH_TOKEN_ENDPOINT: &str = "https://auth.openai.com/oauth/token";

#[derive(Debug, Clone)]
pub struct OAuthConfig {
    pub client_id: String,
    pub auth_endpoint: String,
    pub device_code_endpoint: String,
    pub token_endpoint: String,
}

impl OAuthConfig {
    pub fn codex_default() -> Self {
        Self {
            client_id: CODEX_OAUTH_CLIENT_ID.to_string(),
            auth_endpoint: OPENAI_OAUTH_AUTHORIZE_ENDPOINT.to_string(),
            device_code_endpoint: OPENAI_OAUTH_DEVICE_CODE_ENDPOINT.to_string(),
            token_endpoint: OPENAI_OAUTH_TOKEN_ENDPOINT.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OAuthError {
    #[error("authorization pending")]
    AuthorizationPending,
    #[error("slow down")]
    SlowDown,
    #[error("OAuth request rejected: {0}")]
    Rejected(String),
    #[error("OAuth transport failed")]
    Transport,
}

impl OAuthError {
    pub fn pending_code(&self) -> Option<&'static str> {
        match self {
            Self::AuthorizationPending => Some("authorization_pending"),
            Self::SlowDown => Some("slow_down"),
            Self::Rejected(_) | Self::Transport => None,
        }
    }
}

#[async_trait]
pub trait OAuthClient: Send + Sync + 'static {
    async fn request_device_code(&self) -> Result<DeviceCode, OAuthError>;

    async fn poll_device_token(&self, device_code: &str) -> Result<TokenPair, OAuthError>;
}

#[derive(Clone)]
pub struct OpenAiOAuthRefresher {
    client: Client,
    config: OAuthConfig,
}

impl OpenAiOAuthRefresher {
    pub fn new(client: Client, config: OAuthConfig) -> Self {
        Self { client, config }
    }

    pub fn codex_default(client: Client) -> Self {
        Self::new(client, OAuthConfig::codex_default())
    }
}

#[derive(Deserialize)]
struct RefreshTokenResponse {
    access_token: String,
    refresh_token: Option<String>,
}

#[derive(Deserialize)]
struct OAuthErrorResponse {
    error: Option<String>,
    error_description: Option<String>,
}

#[async_trait]
impl OAuthClient for OpenAiOAuthRefresher {
    async fn request_device_code(&self) -> Result<DeviceCode, OAuthError> {
        let response = self
            .client
            .post(&self.config.device_code_endpoint)
            .form(&[
                ("client_id", self.config.client_id.as_str()),
                ("scope", "openid profile email offline_access"),
            ])
            .send()
            .await
            .map_err(|_| OAuthError::Transport)?;
        let status = response.status();
        let body = response.text().await.map_err(|_| OAuthError::Transport)?;
        if !status.is_success() {
            return Err(OAuthError::Rejected(format!(
                "Device code request failed ({status}): {body}"
            )));
        }
        serde_json::from_str::<DeviceCode>(&body).map_err(|_| OAuthError::Transport)
    }

    async fn poll_device_token(&self, device_code: &str) -> Result<TokenPair, OAuthError> {
        let response = self
            .client
            .post(&self.config.token_endpoint)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", device_code),
                ("client_id", self.config.client_id.as_str()),
            ])
            .send()
            .await
            .map_err(|_| OAuthError::Transport)?;
        let status = response.status();
        let body = response.text().await.map_err(|_| OAuthError::Transport)?;
        if !status.is_success() {
            return Err(classify_device_poll_failure(status, &body));
        }
        parse_token_pair(&body).map_err(|_| OAuthError::Transport)
    }
}

#[async_trait]
impl TokenRefresher for OpenAiOAuthRefresher {
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
            .map_err(|_| RefreshFailure::Transport)?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|_| RefreshFailure::Transport)?;
        if !status.is_success() {
            return Err(classify_refresh_failure(status, &body));
        }
        parse_token_pair(&body).map_err(|_| RefreshFailure::Transport)
    }
}

fn parse_token_pair(body: &str) -> Result<TokenPair, ()> {
    let tokens = serde_json::from_str::<RefreshTokenResponse>(body).map_err(|_| ())?;
    if tokens.access_token.trim().is_empty() {
        return Err(());
    }
    Ok(TokenPair {
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
    })
}

fn classify_device_poll_failure(status: StatusCode, body: &str) -> OAuthError {
    let parsed = serde_json::from_str::<OAuthErrorResponse>(body).ok();
    match parsed.as_ref().and_then(|data| data.error.as_deref()) {
        Some("authorization_pending") => OAuthError::AuthorizationPending,
        Some("slow_down") => OAuthError::SlowDown,
        Some(code) => OAuthError::Rejected(
            parsed
                .as_ref()
                .and_then(|data| data.error_description.clone())
                .unwrap_or_else(|| code.to_string()),
        ),
        None => OAuthError::Rejected(format!("Device token poll failed ({status}): {body}")),
    }
}

fn classify_refresh_failure(status: StatusCode, body: &str) -> RefreshFailure {
    let lower = body.to_ascii_lowercase();
    if lower.contains("quota") {
        return RefreshFailure::QuotaExhausted;
    }
    if lower.contains("account has been deactivated")
        || lower.contains("refresh_token_reused")
        || lower.contains("banned")
    {
        return RefreshFailure::Banned;
    }
    if status == StatusCode::BAD_REQUEST
        || status == StatusCode::UNAUTHORIZED
        || lower.contains("invalid_grant")
        || lower.contains("invalid_token")
        || lower.contains("access_denied")
        || lower.contains("refresh_token_expired")
        || lower.contains("token_revoked")
    {
        return RefreshFailure::InvalidGrant;
    }
    RefreshFailure::Transport
}
