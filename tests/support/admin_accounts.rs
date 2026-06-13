use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::Utc;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tower::ServiceExt;

use codex_proxy_rs::{
    codex::gateway::oauth::{
        DeviceCode, OAuthClient, OAuthError, RefreshFailure, TokenPair, TokenRefresher,
    },
    config::{
        AdminConfig, ApiConfig, AppConfig, AuthConfig, DatabaseConfig, LoggingConfig, ModelConfig,
        QuotaConfig, QuotaWarningThresholds, SecurityConfig, ServerConfig, TlsConfig,
        UsageStatsConfig,
    },
    platform::crypto::SecretBox,
    platform::identity::api_key::ApiKeyHasher,
    platform::storage::db::connect_sqlite,
    runtime::build_router,
    runtime::state::AppState,
};

use super::seed_admin_session;

#[derive(Clone)]
pub struct StaticTokenRefresher {
    pub result: Result<TokenPair, RefreshFailure>,
    pub calls: Arc<Mutex<Vec<String>>>,
}

#[derive(Clone)]
pub struct FailingTokenRefresher {
    pub failure: RefreshFailure,
}

#[derive(Clone)]
pub struct StaticOAuthClient {
    pub device_response: Result<DeviceCode, OAuthError>,
    pub poll_response: Result<TokenPair, OAuthError>,
    pub exchange_response: Result<TokenPair, OAuthError>,
    pub poll_calls: Arc<Mutex<Vec<String>>>,
    pub exchange_calls: Arc<Mutex<Vec<ExchangeCall>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExchangeCall {
    pub code: String,
    pub code_verifier: String,
    pub redirect_uri: String,
}

#[async_trait]
impl TokenRefresher for StaticTokenRefresher {
    async fn refresh(&self, refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        self.calls.lock().await.push(refresh_token.to_string());
        self.result.clone()
    }
}

#[async_trait]
impl TokenRefresher for FailingTokenRefresher {
    async fn refresh(&self, _refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        Err(self.failure)
    }
}

#[async_trait]
impl TokenRefresher for StaticOAuthClient {
    async fn refresh(&self, _refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        Err(RefreshFailure::Transport)
    }
}

#[async_trait]
impl OAuthClient for StaticOAuthClient {
    async fn exchange_code(
        &self,
        code: &str,
        code_verifier: &str,
        redirect_uri: &str,
    ) -> Result<TokenPair, OAuthError> {
        self.exchange_calls.lock().await.push(ExchangeCall {
            code: code.to_string(),
            code_verifier: code_verifier.to_string(),
            redirect_uri: redirect_uri.to_string(),
        });
        self.exchange_response.clone()
    }

    async fn request_device_code(&self) -> Result<DeviceCode, OAuthError> {
        self.device_response.clone()
    }

    async fn poll_device_token(&self, device_code: &str) -> Result<TokenPair, OAuthError> {
        self.poll_calls.lock().await.push(device_code.to_string());
        self.poll_response.clone()
    }
}

pub fn test_config(database_url: String) -> AppConfig {
    AppConfig {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
        },
        api: ApiConfig {
            base_url: "https://chatgpt.com/backend-api".to_string(),
        },
        model: ModelConfig {
            default_model: "gpt-5.5".to_string(),
            default_reasoning_effort: None,
            service_tier: None,
            aliases: BTreeMap::new(),
        },
        auth: AuthConfig {
            refresh_margin_seconds: 300,
            refresh_enabled: true,
            refresh_concurrency: 2,
            max_concurrent_per_account: 3,
            request_interval_ms: 50,
            rotation_strategy: "least_used".to_string(),
            tier_priority: Vec::new(),
        },
        quota: QuotaConfig {
            refresh_interval_minutes: 5,
            warning_thresholds: QuotaWarningThresholds {
                primary: vec![80, 90],
                secondary: vec![80, 90],
            },
            skip_exhausted: true,
        },
        usage_stats: UsageStatsConfig {
            history_retention_days: None,
        },
        database: DatabaseConfig { url: database_url },
        security: SecurityConfig {
            master_key_file: "data/master.key".to_string(),
            api_key_pepper_file: "data/api-key-pepper.key".to_string(),
        },
        tls: TlsConfig {
            force_http11: false,
        },
        admin: AdminConfig {
            session_ttl_minutes: 1440,
            default_username: "admin".to_string(),
            default_password: "admin".to_string(),
            session_cleanup_interval_secs: 3600,
        },
        logging: LoggingConfig {
            directory: "logs".to_string(),
            max_file_bytes: 10_485_760,
            retention_days: 14,
            enabled: false,
            capacity: 2_000,
            capture_body: false,
        },
    }
}

pub fn test_config_with_base_url(database_url: String, base_url: String) -> AppConfig {
    AppConfig {
        api: ApiConfig { base_url },
        ..test_config(database_url)
    }
}

pub async fn admin_accounts_test_app(
    db_name: &str,
    key_byte: u8,
) -> (Router, AppState, sqlx::SqlitePool, tempfile::TempDir) {
    admin_accounts_test_app_with_base_url(
        db_name,
        key_byte,
        "https://chatgpt.com/backend-api".to_string(),
    )
    .await
}

pub async fn admin_accounts_test_app_with_base_url(
    db_name: &str,
    key_byte: u8,
    base_url: String,
) -> (Router, AppState, sqlx::SqlitePool, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(db_name);
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let state = AppState::with_pool_and_secret_box(
        test_config_with_base_url(url, base_url),
        pool.clone(),
        SecretBox::new([key_byte; 32]),
    );
    let app = build_router(state.clone());
    (app, state, pool, dir)
}

pub async fn admin_accounts_test_app_with_refresher<C>(
    db_name: &str,
    key_byte: u8,
    token_refresher: C,
) -> (Router, AppState, sqlx::SqlitePool, tempfile::TempDir)
where
    C: TokenRefresher,
{
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(db_name);
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let state = AppState::with_pool_secret_api_key_hasher_and_token_refresher(
        test_config(url),
        pool.clone(),
        SecretBox::new([key_byte; 32]),
        ApiKeyHasher::new([key_byte; 32]),
        token_refresher,
    );
    let app = build_router(state.clone());
    (app, state, pool, dir)
}

pub async fn admin_accounts_test_app_with_oauth_client<C>(
    db_name: &str,
    key_byte: u8,
    oauth_client: C,
) -> (Router, AppState, sqlx::SqlitePool, tempfile::TempDir)
where
    C: OAuthClient + TokenRefresher,
{
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(db_name);
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let state = AppState::with_pool_secret_api_key_hasher_and_oauth_client(
        test_config(url),
        pool.clone(),
        SecretBox::new([key_byte; 32]),
        ApiKeyHasher::new([key_byte; 32]),
        oauth_client,
    );
    let app = build_router(state.clone());
    (app, state, pool, dir)
}

pub async fn post_admin_account(app: &Router, payload: Value) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

pub async fn import_test_account(app: &Router, session_id: &str, account_id: &str) {
    let import_body = json!({
        "accounts": [{
            "id": account_id,
            "email": format!("{account_id}@example.com"),
            "planType": "plus",
            "token": format!("access-{account_id}"),
            "refreshToken": format!("refresh-{account_id}"),
            "status": "active"
        }]
    });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", format!("cpr_admin_session={session_id}"))
                .body(Body::from(import_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

pub fn test_jwt(
    account_id: Option<&str>,
    user_id: Option<&str>,
    email: Option<&str>,
    plan_type: Option<&str>,
    exp_offset_seconds: i64,
) -> String {
    let mut auth = serde_json::Map::new();
    if let Some(account_id) = account_id {
        auth.insert(
            "chatgpt_account_id".to_string(),
            Value::String(account_id.to_string()),
        );
    }
    if let Some(user_id) = user_id {
        auth.insert(
            "chatgpt_user_id".to_string(),
            Value::String(user_id.to_string()),
        );
    }
    if let Some(plan_type) = plan_type {
        auth.insert(
            "chatgpt_plan_type".to_string(),
            Value::String(plan_type.to_string()),
        );
    }
    let mut profile = serde_json::Map::new();
    if let Some(email) = email {
        profile.insert("email".to_string(), Value::String(email.to_string()));
    }
    let payload = json!({
        "exp": Utc::now().timestamp() + exp_offset_seconds,
        "https://api.openai.com/auth": Value::Object(auth),
        "https://api.openai.com/profile": Value::Object(profile),
    });
    let header = json!({ "alg": "none", "typ": "JWT" });
    format!("{}.{}.", jwt_part(&header), jwt_part(&payload),)
}

fn jwt_part(value: &Value) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).unwrap())
}
