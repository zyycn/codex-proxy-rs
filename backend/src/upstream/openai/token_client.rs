//! OpenAI token 续期 Reqwest 适配器。

use async_trait::async_trait;
use reqwest::{Client, StatusCode};
use serde::Deserialize;

use crate::fleet::refresh::{RefreshFailure, TokenPair, TokenRefresher};

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

impl OpenAiTokenClient {
    /// 使用 Reqwest 客户端和静态配置构造 token 续期客户端。
    pub fn new(client: Client, config: TokenClientConfig) -> Self {
        Self { client, config }
    }
}

/// 使用默认 Reqwest 客户端构造 OpenAI token 续期客户端。
pub fn default_openai_token_client(config: TokenClientConfig) -> OpenAiTokenClient {
    OpenAiTokenClient::new(Client::new(), config)
}

#[derive(Deserialize)]
struct RefreshTokenResponse {
    access_token: String,
    refresh_token: Option<String>,
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
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|_| RefreshFailure::Transport)?;
        if !status.is_success() {
            return Err(classify_refresh_failure(status, &body));
        }
        parse_token_pair(&body).map_err(|()| RefreshFailure::Transport)
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

fn classify_refresh_failure(_status: StatusCode, body: &str) -> RefreshFailure {
    let lower = body.to_ascii_lowercase();
    if lower.contains("account has been deactivated") || lower.contains("refresh_token_reused") {
        return RefreshFailure::Banned;
    }
    if lower.contains("invalid_grant")
        || lower.contains("invalid_token")
        || lower.contains("access_denied")
        || lower.contains("refresh_token_expired")
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
