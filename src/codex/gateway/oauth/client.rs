use std::collections::BTreeMap;

use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Duration, Utc};
use rand::Rng;
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::config::AuthConfig;

use super::{
    refresh::{RefreshFailure, TokenRefresher},
    token::TokenPair,
};

const OPENAI_OAUTH_DEVICE_CODE_ENDPOINT: &str = "https://auth.openai.com/oauth/device/code";
const OAUTH_CALLBACK_PORT: u16 = 1455;
const OAUTH_CALLBACK_PATH: &str = "/auth/callback";

#[derive(Debug, Clone)]
pub struct OAuthConfig {
    pub client_id: String,
    pub auth_endpoint: String,
    pub device_code_endpoint: String,
    pub token_endpoint: String,
}

impl OAuthConfig {
    pub fn from_auth_config(config: &AuthConfig) -> Self {
        Self {
            client_id: config.oauth_client_id.clone(),
            auth_endpoint: config.oauth_auth_endpoint.clone(),
            device_code_endpoint: OPENAI_OAUTH_DEVICE_CODE_ENDPOINT.to_string(),
            token_endpoint: config.oauth_token_endpoint.clone(),
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
    async fn exchange_code(
        &self,
        code: &str,
        code_verifier: &str,
        redirect_uri: &str,
    ) -> Result<TokenPair, OAuthError>;

    async fn request_device_code(&self) -> Result<DeviceCode, OAuthError>;

    async fn poll_device_token(&self, device_code: &str) -> Result<TokenPair, OAuthError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PkceLogin {
    pub auth_url: String,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PkceSession {
    pub code_verifier: String,
    pub redirect_uri: String,
    pub return_host: String,
}

#[derive(Debug, Clone)]
struct PendingPkceSession {
    session: PkceSession,
    created_at: DateTime<Utc>,
    exchanging: bool,
}

#[derive(Debug)]
pub struct PkceSessionStore {
    pending: BTreeMap<String, PendingPkceSession>,
    completed: BTreeMap<String, DateTime<Utc>>,
    ttl: Duration,
}

impl Default for PkceSessionStore {
    fn default() -> Self {
        Self {
            pending: BTreeMap::new(),
            completed: BTreeMap::new(),
            ttl: Duration::minutes(5),
        }
    }
}

impl PkceSessionStore {
    pub fn start_login(&mut self, return_host: &str, config: &OAuthConfig) -> PkceLogin {
        self.cleanup(Utc::now());
        let (code_verifier, code_challenge) = generate_pkce_pair();
        let state = random_state();
        let redirect_uri = format!("http://localhost:{OAUTH_CALLBACK_PORT}{OAUTH_CALLBACK_PATH}");
        let auth_url = build_auth_url(config, &redirect_uri, &state, &code_challenge);
        self.pending.insert(
            state.clone(),
            PendingPkceSession {
                session: PkceSession {
                    code_verifier,
                    redirect_uri,
                    return_host: return_host.to_string(),
                },
                created_at: Utc::now(),
                exchanging: false,
            },
        );
        PkceLogin { auth_url, state }
    }

    pub fn try_acquire(&mut self, state: &str) -> Option<PkceSession> {
        self.cleanup(Utc::now());
        let pending = self.pending.get_mut(state)?;
        if pending.exchanging {
            return None;
        }
        pending.exchanging = true;
        Some(pending.session.clone())
    }

    pub fn release(&mut self, state: &str) {
        if let Some(pending) = self.pending.get_mut(state) {
            pending.exchanging = false;
        }
    }

    pub fn complete(&mut self, state: &str) {
        self.pending.remove(state);
        self.completed.insert(state.to_string(), Utc::now());
    }

    pub fn is_completed_or_exchanging(&mut self, state: &str) -> bool {
        self.cleanup(Utc::now());
        self.completed.contains_key(state)
            || self
                .pending
                .get(state)
                .is_some_and(|pending| pending.exchanging)
    }

    fn cleanup(&mut self, now: DateTime<Utc>) {
        self.pending
            .retain(|_, pending| now - pending.created_at <= self.ttl);
        self.completed
            .retain(|_, completed_at| now - *completed_at <= self.ttl);
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
impl TokenRefresher for OpenAiOAuthRefresher {
    async fn refresh(&self, refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        // 🔒 一次性刷新令牌保护：
        // OAuth2 refresh tokens 可能是一次性的（用完即失效）。
        // 如果请求中途失败（网络错误、超时等），我们不能自动重试，
        // 因为令牌可能已被服务器消耗。
        //
        // 调用者应该：
        // 1. 在调用前保存原始 refresh_token
        // 2. 只在 is_safe_to_retry_refresh(&error) 返回 true 时重试
        // 3. 任何其他错误都应标记为 permanent failure
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

fn generate_pkce_pair() -> (String, String) {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    let code_verifier = URL_SAFE_NO_PAD.encode(bytes);
    let digest = Sha256::digest(code_verifier.as_bytes());
    let code_challenge = URL_SAFE_NO_PAD.encode(digest);
    (code_verifier, code_challenge)
}

fn random_state() -> String {
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn build_auth_url(
    config: &OAuthConfig,
    redirect_uri: &str,
    state: &str,
    code_challenge: &str,
) -> String {
    let params = [
        ("response_type", "code"),
        ("client_id", config.client_id.as_str()),
        ("redirect_uri", redirect_uri),
        ("scope", "openid profile email offline_access"),
        ("code_challenge", code_challenge),
        ("code_challenge_method", "S256"),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("state", state),
        ("originator", "codex_cli_rs"),
    ];
    let query = params
        .iter()
        .map(|(key, value)| format!("{}={}", percent_encode(key), percent_encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{}?{query}", config.auth_endpoint)
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(char::from(byte));
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
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
