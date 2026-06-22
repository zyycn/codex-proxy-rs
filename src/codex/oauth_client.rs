//! OpenAI OAuth Reqwest 适配器。

use async_trait::async_trait;
use reqwest::{Client, StatusCode};
use serde::Deserialize;

use crate::accounts::oauth::{DeviceCode, OAuthClient, OAuthConfig, OAuthError, TokenPair};
use crate::accounts::token_refresh::{RefreshFailure, TokenRefresher};

/// OpenAI OAuth 上游客户端。
#[derive(Clone)]
pub struct OpenAiOAuthClient {
    client: Client,
    config: OAuthConfig,
}

impl OpenAiOAuthClient {
    /// 使用 Reqwest 客户端和静态配置构造 OAuth 客户端。
    pub fn new(client: Client, config: OAuthConfig) -> Self {
        Self { client, config }
    }
}

/// 使用默认 Reqwest 客户端构造 OpenAI OAuth 客户端。
pub fn default_openai_oauth_client(config: OAuthConfig) -> OpenAiOAuthClient {
    OpenAiOAuthClient::new(Client::new(), config)
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
impl OAuthClient for OpenAiOAuthClient {
    async fn exchange_code(
        &self,
        code: &str,
        code_verifier: &str,
        redirect_uri: &str,
    ) -> Result<TokenPair, OAuthError> {
        let response = self
            .client
            .post(&self.config.token_endpoint)
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", self.config.client_id.as_str()),
                ("code", code),
                ("redirect_uri", redirect_uri),
                ("code_verifier", code_verifier),
            ])
            .send()
            .await
            .map_err(|_| OAuthError::Transport)?;
        let status = response.status();
        let body = response.text().await.map_err(|_| OAuthError::Transport)?;
        if !status.is_success() {
            return Err(OAuthError::Rejected(format!(
                "Token exchange failed ({status}): {body}"
            )));
        }
        parse_token_pair(&body).map_err(|_| OAuthError::Transport)
    }

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
impl TokenRefresher for OpenAiOAuthClient {
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
