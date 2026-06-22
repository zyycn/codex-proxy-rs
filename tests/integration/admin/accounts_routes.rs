use std::{collections::BTreeMap, fs, sync::Arc};

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use codex_proxy_rs::{
    access::admin_session::SqliteAdminSessionStore,
    access::client_keys::SqliteClientKeyStore,
    accounts::{
        cookies::SqliteCookieStore,
        model::AccountStatus,
        store::{NewAccount, SqliteAccountStore},
        token_refresh::RefreshLeaseStore,
    },
    app::services::{BackgroundTaskStores, Services},
    app::state::AppState,
    codex::fingerprint::{Fingerprint, FingerprintRepository},
    config::types::{
        AdminConfig, ApiConfig, AuthConfig, DatabaseConfig, LoggingConfig, ModelConfig,
        QuotaConfig, QuotaWarningThresholds, SecurityConfig, ServerConfig, TlsConfig,
        UsageStatsConfig, WebSocketPoolConfig,
    },
    config::AppConfig,
    gateway::dispatch::session_affinity::SqliteSessionAffinityStore,
    infra::{crypto::SecretBox, database::connect_sqlite, identity::ApiKeyHasher},
    telemetry::event_store::SqliteEventLogStore,
};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tower::util::ServiceExt;

#[path = "accounts_routes/import_export.rs"]
mod accounts_import_export;
#[path = "accounts_routes/lifecycle.rs"]
mod accounts_lifecycle;
#[path = "accounts_routes/list.rs"]
mod accounts_list;
#[path = "accounts_routes/oauth.rs"]
mod accounts_oauth;
#[path = "accounts_routes/quota.rs"]
mod accounts_quota;

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
    sqlx::query("insert into accounts (id, email, label, plan_type, access_token_cipher, status, added_at, updated_at) values (?, ?, ?, ?, ?, 'active', ?, ?)")
        .bind(id).bind(email).bind(label).bind(plan_type).bind("encrypted")
        .bind("2026-06-11T00:00:00Z").bind("2026-06-11T00:00:00Z")
        .execute(pool).await.unwrap();
    sqlx::query("insert into account_usage (account_id, request_count, empty_response_count, input_tokens, output_tokens, cached_tokens, last_used_at) values (?, ?, ?, ?, ?, ?, ?)")
        .bind(id).bind(request_count).bind(empty_response_count).bind(input_tokens).bind(output_tokens).bind(cached_tokens).bind(last_used_at)
        .execute(pool).await.unwrap();
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
    admin_accounts_test_app_with_api_base_url(
        db_name,
        key_byte,
        "https://chatgpt.com/backend-api".to_string(),
    )
    .await
}

async fn admin_accounts_test_app_with_api_base_url(
    db_name: &str,
    key_byte: u8,
    api_base_url: String,
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
    let hasher = ApiKeyHasher::new([key_byte; 32]);
    let mut config = test_config(url);
    config.api.base_url = api_base_url;
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone(), secret_box.clone()),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone(), secret_box.clone()),
        fingerprints: FingerprintRepository::new(pool.clone()),
        session_affinity: SqliteSessionAffinityStore::new(pool.clone()),
        refresh_leases: RefreshLeaseStore::new(pool.clone()),
        client_keys: SqliteClientKeyStore::new(pool.clone(), hasher),
        event_logs: SqliteEventLogStore::new(pool.clone()),
    };
    let fingerprint = Fingerprint::default_for_tests();
    let services = Arc::new(Services::new(&config, stores, fingerprint));
    let state = AppState {
        config,
        services: (*services).clone(),
    };
    let app = codex_proxy_rs::http::router::router().with_state(state.clone());
    (app, state, pool, dir, secret_box)
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
        "https://api.openai.com/profile": { "email": email }
    });
    format!("{}.{}.", jwt_part(&header), jwt_part(&payload))
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
        fingerprint: Default::default(),
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
