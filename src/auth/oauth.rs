use async_trait::async_trait;
use reqwest::{Client, StatusCode};
use serde::Deserialize;

use crate::auth::{
    refresh::{RefreshFailure, TokenRefresher},
    token::TokenPair,
};

const CODEX_OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OPENAI_OAUTH_AUTHORIZE_ENDPOINT: &str = "https://auth.openai.com/oauth/authorize";
const OPENAI_OAUTH_TOKEN_ENDPOINT: &str = "https://auth.openai.com/oauth/token";

#[derive(Debug, Clone)]
pub struct OAuthConfig {
    pub client_id: String,
    pub auth_endpoint: String,
    pub token_endpoint: String,
}

impl OAuthConfig {
    pub fn codex_default() -> Self {
        Self {
            client_id: CODEX_OAUTH_CLIENT_ID.to_string(),
            auth_endpoint: OPENAI_OAUTH_AUTHORIZE_ENDPOINT.to_string(),
            token_endpoint: OPENAI_OAUTH_TOKEN_ENDPOINT.to_string(),
        }
    }
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
        let tokens = serde_json::from_str::<RefreshTokenResponse>(&body)
            .map_err(|_| RefreshFailure::Transport)?;
        if tokens.access_token.trim().is_empty() {
            return Err(RefreshFailure::Transport);
        }
        Ok(TokenPair {
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
        })
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
