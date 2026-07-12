use std::{collections::HashMap, fmt::Write as _, sync::Arc};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration, Utc};
use rand::Rng;
use reqwest::{Client, StatusCode, Url};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use super::{
    AccountManageService,
    types::{AccountManageError, OAuthAuthorizeResult, OAuthExchangeInput},
};

const OPENAI_OAUTH_AUTHORIZE_ENDPOINT: &str = "https://auth.openai.com/oauth/authorize";
const OPENAI_OAUTH_REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const OPENAI_OAUTH_SCOPE: &str = "openid profile email offline_access";
const OAUTH_SESSION_TTL_MINUTES: i64 = 30;

#[derive(Clone)]
pub struct AccountOAuthService {
    client: Client,
    client_id: String,
    token_endpoint: String,
    sessions: Arc<Mutex<HashMap<String, AccountOAuthSession>>>,
}

impl AccountOAuthService {
    pub fn new(client: Client, client_id: String, token_endpoint: String) -> Self {
        Self {
            client,
            client_id,
            token_endpoint,
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn authorize(&self) -> Result<OAuthAuthorizeResult, AccountManageError> {
        let now = Utc::now();
        let expires_at = now + Duration::minutes(OAUTH_SESSION_TTL_MINUTES);
        let session_id = random_url_token(32);
        let state = random_url_token(32);
        let code_verifier = random_hex_token(64);
        let code_challenge = code_challenge(&code_verifier);
        let session = AccountOAuthSession {
            state: state.clone(),
            code_verifier,
            expires_at,
        };

        let mut sessions = self.sessions.lock().await;
        sessions.retain(|_, session| session.expires_at > now);
        sessions.insert(session_id.clone(), session);
        drop(sessions);

        let mut auth_url =
            Url::parse(OPENAI_OAUTH_AUTHORIZE_ENDPOINT).map_err(|_| AccountManageError::Import)?;
        auth_url
            .query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.client_id)
            .append_pair("redirect_uri", OPENAI_OAUTH_REDIRECT_URI)
            .append_pair("scope", OPENAI_OAUTH_SCOPE)
            .append_pair("state", &state)
            .append_pair("code_challenge", &code_challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("id_token_add_organizations", "true")
            .append_pair("codex_cli_simplified_flow", "true");

        Ok(OAuthAuthorizeResult {
            session_id,
            auth_url: auth_url.to_string(),
            expires_at,
        })
    }

    async fn exchange(
        &self,
        input: OAuthExchangeInput,
    ) -> Result<AccountOAuthTokens, AccountManageError> {
        let (code, state) = oauth_callback_parts(&input)?;
        let session = self.session(&input.session_id).await?;
        if session.state != state {
            return Err(AccountManageError::OAuthStateMismatch);
        }

        let response = self
            .client
            .post(&self.token_endpoint)
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", self.client_id.as_str()),
                ("code", code.as_str()),
                ("redirect_uri", OPENAI_OAUTH_REDIRECT_URI),
                ("code_verifier", session.code_verifier.as_str()),
            ])
            .send()
            .await
            .map_err(|error| AccountManageError::OAuthCodeExchange(error.to_string()))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| AccountManageError::OAuthCodeExchange(error.to_string()))?;
        if !status.is_success() {
            return Err(AccountManageError::OAuthCodeExchange(exchange_error_text(
                status, &body,
            )));
        }

        let tokens = serde_json::from_str::<AccountOAuthTokens>(&body)
            .map_err(|_| AccountManageError::OAuthCodeExchange("invalid token response".into()))?;
        if tokens.access_token.trim().is_empty() {
            return Err(AccountManageError::OAuthCodeExchange(
                "empty access token".into(),
            ));
        }

        self.remove_session(&input.session_id).await;
        Ok(tokens)
    }

    async fn session(&self, session_id: &str) -> Result<AccountOAuthSession, AccountManageError> {
        let now = Utc::now();
        let mut sessions = self.sessions.lock().await;
        sessions.retain(|_, session| session.expires_at > now);
        sessions
            .get(session_id)
            .cloned()
            .ok_or(AccountManageError::OAuthSessionInvalid)
    }

    async fn remove_session(&self, session_id: &str) {
        self.sessions.lock().await.remove(session_id);
    }
}

impl AccountManageService {
    pub async fn oauth_authorize(&self) -> Result<OAuthAuthorizeResult, AccountManageError> {
        self.oauth.authorize().await
    }

    pub async fn oauth_exchange(
        &self,
        input: OAuthExchangeInput,
    ) -> Result<super::types::ImportedAccounts, AccountManageError> {
        let tokens = self.oauth.exchange(input).await?;
        self.import(serde_json::json!({
            "sourceFormat": "cpr",
            "accounts": [{
                "token": tokens.access_token,
                "refreshToken": tokens.refresh_token,
            }]
        }))
        .await
    }
}

#[derive(Debug, Clone)]
struct AccountOAuthSession {
    state: String,
    code_verifier: String,
    expires_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
struct AccountOAuthTokens {
    access_token: String,
    refresh_token: Option<String>,
}

fn oauth_callback_parts(
    input: &OAuthExchangeInput,
) -> Result<(String, String), AccountManageError> {
    if let Some(callback_url) = input.callback_url.as_deref() {
        let (code, state) = callback_url_parts(callback_url)?;
        return Ok((code, state));
    }

    let code = normalized(input.code.as_deref()).ok_or(AccountManageError::OAuthCallbackInvalid)?;
    let state =
        normalized(input.state.as_deref()).ok_or(AccountManageError::OAuthCallbackInvalid)?;
    Ok((code, state))
}

fn callback_url_parts(callback_url: &str) -> Result<(String, String), AccountManageError> {
    let url =
        Url::parse(callback_url.trim()).map_err(|_| AccountManageError::OAuthCallbackInvalid)?;
    let mut code = None;
    let mut state = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => code = normalized(Some(value.as_ref())),
            "state" => state = normalized(Some(value.as_ref())),
            _ => {}
        }
    }
    code.zip(state)
        .ok_or(AccountManageError::OAuthCallbackInvalid)
}

fn normalized(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn code_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn random_url_token(size: usize) -> String {
    let mut bytes = vec![0u8; size];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn random_hex_token(size: usize) -> String {
    let mut bytes = vec![0u8; size];
    rand::rng().fill_bytes(&mut bytes);
    let mut token = String::with_capacity(size * 2);
    for byte in bytes {
        let _ = write!(token, "{byte:02x}");
    }
    token
}

fn exchange_error_text(status: StatusCode, body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        return format!("upstream returned {status}");
    }
    format!("upstream returned {status}: {body}")
}
