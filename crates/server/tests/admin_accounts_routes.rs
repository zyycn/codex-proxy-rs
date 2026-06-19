use std::{collections::BTreeMap, fs, sync::Arc};

use async_trait::async_trait;
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{Duration, Utc};
use codex_proxy_adapters::sqlite::accounts::{NewAccount, SqliteAccountStore};
use codex_proxy_core::{
    accounts::{model::AccountStatus, ports::AccountStore},
    auth::{
        oauth::{DeviceCode, OAuthError, RefreshFailure, TokenPair},
        ports::{OAuthClient, TokenRefresher},
    },
};
use codex_proxy_platform::{
    config::{
        AdminConfig, ApiConfig, AppConfig, AuthConfig, DatabaseConfig, LoggingConfig, ModelConfig,
        QuotaConfig, QuotaWarningThresholds, SecurityConfig, ServerConfig, TlsConfig,
        UsageStatsConfig, WebSocketPoolConfig,
    },
    crypto::SecretBox,
    identity::ApiKeyHasher,
    storage::connect_sqlite,
};
use codex_proxy_runtime::state::AppState;
use codex_proxy_server::router;
use secrecy::{ExposeSecret, SecretString};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tokio::sync::Mutex;
use tower::util::ServiceExt;
use wiremock::{
    matchers::{header, method, path},
    Mock, MockServer, ResponseTemplate,
};

#[path = "admin_accounts_routes/admin_accounts_import_export.rs"]
mod admin_accounts_import_export;
#[path = "admin_accounts_routes/admin_accounts_lifecycle.rs"]
mod admin_accounts_lifecycle;
#[path = "admin_accounts_routes/admin_accounts_list.rs"]
mod admin_accounts_list;
#[path = "admin_accounts_routes/admin_accounts_oauth.rs"]
mod admin_accounts_oauth;
#[path = "admin_accounts_routes/admin_accounts_quota.rs"]
mod admin_accounts_quota;

async fn seed_admin_session(pool: &SqlitePool, session_id: &str) {
    sqlx::query(
        "insert into admin_users (id, password_hash, created_at, updated_at) values (?, ?, ?, ?)",
    )
    .bind("admin_1")
    .bind("hash")
    .bind("2026-06-18T00:00:00Z")
    .bind("2026-06-18T00:00:00Z")
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into admin_sessions (id, user_id, expires_at, created_at) values (?, ?, ?, ?)",
    )
    .bind(session_id)
    .bind("admin_1")
    .bind("2999-01-01T00:00:00Z")
    .bind("2026-06-18T00:00:00Z")
    .execute(pool)
    .await
    .unwrap();
}

#[expect(
    clippy::too_many_arguments,
    reason = "test fixture keeps usage rows explicit"
)]
async fn seed_usage_account(
    pool: &SqlitePool,
    id: &str,
    email: &str,
    label: &str,
    plan_type: &str,
    request_count: i64,
    empty_response_count: i64,
    input_tokens: i64,
    output_tokens: i64,
    cached_tokens: i64,
    last_used_at: &str,
) {
    sqlx::query(
        "insert into accounts (id, email, label, plan_type, access_token_cipher, status, added_at, updated_at) values (?, ?, ?, ?, ?, 'active', ?, ?)",
    )
    .bind(id)
    .bind(email)
    .bind(label)
    .bind(plan_type)
    .bind("encrypted")
    .bind("2026-06-11T00:00:00Z")
    .bind("2026-06-11T00:00:00Z")
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into account_usage (account_id, request_count, empty_response_count, input_tokens, output_tokens, cached_tokens, last_used_at) values (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(request_count)
    .bind(empty_response_count)
    .bind(input_tokens)
    .bind(output_tokens)
    .bind(cached_tokens)
    .bind(last_used_at)
    .execute(pool)
    .await
    .unwrap();
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn post_admin_account(app: &axum::Router, payload: Value) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn admin_accounts_test_app(
    db_name: &str,
    key_byte: u8,
) -> (
    axum::Router,
    AppState,
    SqlitePool,
    tempfile::TempDir,
    SecretBox,
) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(db_name);
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([key_byte; 32]);
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool.clone(),
        secret_box.clone(),
        ApiKeyHasher::new([key_byte; 32]),
    );
    let app = router::router().with_state(state.clone());
    (app, state, pool, dir, secret_box)
}

async fn admin_accounts_test_app_with_oauth_client<C>(
    db_name: &str,
    key_byte: u8,
    oauth_client: C,
) -> (axum::Router, AppState, SqlitePool, tempfile::TempDir)
where
    C: OAuthClient + TokenRefresher + Clone,
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
    let app = router::router().with_state(state.clone());
    (app, state, pool, dir)
}

async fn seed_encrypted_account(pool: &SqlitePool, secret_box: SecretBox, account: NewAccount) {
    SqliteAccountStore::new(pool.clone(), secret_box)
        .insert(account)
        .await
        .unwrap();
}

fn test_jwt(
    account_id: &str,
    user_id: Option<&str>,
    email: Option<&str>,
    plan_type: Option<&str>,
) -> String {
    test_jwt_with_exp(Some(account_id), user_id, email, plan_type, 4_102_444_800)
}

fn test_jwt_with_exp(
    account_id: Option<&str>,
    user_id: Option<&str>,
    email: Option<&str>,
    plan_type: Option<&str>,
    exp: i64,
) -> String {
    let header = json!({"alg": "none", "typ": "JWT"});
    let payload = json!({
        "exp": exp,
        "https://api.openai.com/auth": {
            "chatgpt_account_id": account_id,
            "chatgpt_user_id": user_id,
            "chatgpt_plan_type": plan_type,
        },
        "https://api.openai.com/profile": {
            "email": email,
        }
    });
    format!("{}.{}.", jwt_part(&header), jwt_part(&payload))
}

#[derive(Clone)]
struct StaticTokenRefresher {
    result: Result<TokenPair, RefreshFailure>,
    calls: Arc<Mutex<Vec<String>>>,
}

#[derive(Clone)]
struct HealthCheckTokenRefresher {
    calls: Arc<Mutex<Vec<String>>>,
}

#[derive(Clone)]
struct StaticOAuthClient {
    device_response: Result<DeviceCode, OAuthError>,
    poll_response: Result<TokenPair, OAuthError>,
    exchange_response: Result<TokenPair, OAuthError>,
    poll_calls: Arc<Mutex<Vec<String>>>,
    exchange_calls: Arc<Mutex<Vec<ExchangeCall>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExchangeCall {
    code: String,
    code_verifier: String,
    redirect_uri: String,
}

#[async_trait]
impl TokenRefresher for StaticTokenRefresher {
    async fn refresh(&self, refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        self.calls.lock().await.push(refresh_token.to_string());
        self.result.clone()
    }
}

#[async_trait]
impl TokenRefresher for HealthCheckTokenRefresher {
    async fn refresh(&self, refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        self.calls.lock().await.push(refresh_token.to_string());
        match refresh_token {
            "refresh-acct_health_alive" => Ok(TokenPair {
                access_token: test_jwt(
                    "health-alive-account",
                    Some("health-user"),
                    Some("health-alive@example.com"),
                    Some("plus"),
                ),
                refresh_token: Some("new-health-refresh".to_string()),
            }),
            "refresh-acct_health_dead" => Err(RefreshFailure::InvalidGrant),
            _ => Err(RefreshFailure::Transport),
        }
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

fn jwt_part(value: &Value) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).unwrap())
}

fn test_config(database_url: String) -> AppConfig {
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
            oauth_client_id: "app_EMoamEEZ73f0CkXaXp7hrann".to_string(),
            oauth_auth_endpoint: "https://auth.openai.com/oauth/authorize".to_string(),
            oauth_token_endpoint: "https://auth.openai.com/oauth/token".to_string(),
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
        ws_pool: WebSocketPoolConfig::default(),
        admin: AdminConfig {
            session_ttl_minutes: 1440,
            session_cleanup_interval_secs: 3600,
            default_username: "admin".to_string(),
            default_password: "admin".to_string(),
        },
        logging: LoggingConfig {
            directory: "logs".to_string(),
            retention_days: 14,
            enabled: false,
            capacity: 2_000,
            capture_body: false,
        },
    }
}
