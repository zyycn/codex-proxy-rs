use std::collections::BTreeMap;

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::{json, Value};
use tower::ServiceExt;

use codex_proxy_rs::{
    codex::gateway::oauth::{RefreshFailure, TokenPair, TokenRefresher},
    config::{
        AdminConfig, ApiConfig, AppConfig, AuthConfig, DatabaseConfig, LoggingConfig, ModelConfig,
        QuotaConfig, QuotaWarningThresholds, SecurityConfig, ServerConfig, TlsConfig,
        UsageStatsConfig,
    },
    platform::crypto::SecretBox,
    platform::identity::{client_key::ApiKeyHasher, client_key_repository::ClientApiKeyRepository},
    platform::storage::db::connect_sqlite,
    runtime::build_router,
    runtime::state::AppState,
};

use super::seed_admin_session;

pub struct ImportedApp {
    pub app: axum::Router,
    pub pool: sqlx::SqlitePool,
    pub secret_box: SecretBox,
    pub client_api_key: String,
    _tempdir: tempfile::TempDir,
}

#[derive(Clone)]
pub struct StaticTokenRefresher {
    pub access_token: String,
    pub refresh_token: Option<String>,
}

#[derive(Clone)]
pub struct FailingTokenRefresher {
    pub failure: RefreshFailure,
}

pub struct ImportAccount {
    pub id: &'static str,
    pub account_id: &'static str,
    pub token: &'static str,
    pub refresh_token: &'static str,
}

#[async_trait]
impl TokenRefresher for StaticTokenRefresher {
    async fn refresh(&self, _refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        Ok(TokenPair {
            access_token: self.access_token.clone(),
            refresh_token: self.refresh_token.clone(),
        })
    }
}

#[async_trait]
impl TokenRefresher for FailingTokenRefresher {
    async fn refresh(&self, _refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        Err(self.failure)
    }
}

pub fn test_config(database_url: String, base_url: String) -> AppConfig {
    AppConfig {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
        },
        api: ApiConfig { base_url },
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
            retention_days: 14,
            enabled: false,
            capacity: 2_000,
            capture_body: false,
        },
    }
}

pub async fn build_imported_app(base_url: String) -> ImportedApp {
    build_imported_app_with_accounts(
        base_url,
        &[ImportAccount {
            id: "acct_imported",
            account_id: "chatgpt-account",
            token: "access-secret",
            refresh_token: "refresh-secret",
        }],
    )
    .await
}

pub async fn build_imported_app_with_accounts(
    base_url: String,
    accounts: &[ImportAccount],
) -> ImportedApp {
    let tempdir = tempfile::tempdir().unwrap();
    let db = tempdir.path().join("v1-upstream.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([21u8; 32]);
    let hasher = ApiKeyHasher::new([22u8; 32]);
    let generated = hasher.generate_client_api_key("test");
    ClientApiKeyRepository::new(pool.clone())
        .insert_generated("test", &generated)
        .await
        .unwrap();
    let app = build_router(AppState::with_pool_secret_and_api_key_hasher(
        test_config(url, base_url),
        pool.clone(),
        secret_box.clone(),
        hasher,
    ));
    let accounts = accounts
        .iter()
        .map(|account| {
            json!({
                "id": account.id,
                "accountId": account.account_id,
                "token": account.token,
                "refreshToken": account.refresh_token,
                "accessTokenExpiresAt": "2999-01-01T00:00:00Z",
                "status": "active"
            })
        })
        .collect::<Vec<_>>();

    let import_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(json!({ "accounts": accounts }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(import_response.status(), StatusCode::OK);

    ImportedApp {
        app,
        pool,
        secret_box,
        client_api_key: generated.plaintext,
        _tempdir: tempdir,
    }
}

pub async fn build_imported_app_with_refresher(
    base_url: String,
    access_token: &str,
) -> ImportedApp {
    build_imported_app_with_token_refresher(
        base_url,
        StaticTokenRefresher {
            access_token: access_token.to_string(),
            refresh_token: None,
        },
    )
    .await
}

pub async fn build_imported_app_with_token_refresher<C>(
    base_url: String,
    token_refresher: C,
) -> ImportedApp
where
    C: TokenRefresher,
{
    build_imported_app_with_accounts_and_token_refresher(
        base_url,
        &[ImportAccount {
            id: "acct_imported",
            account_id: "chatgpt-account",
            token: "access-secret",
            refresh_token: "refresh-secret",
        }],
        token_refresher,
    )
    .await
}

pub async fn build_imported_app_with_accounts_and_token_refresher<C>(
    base_url: String,
    accounts: &[ImportAccount],
    token_refresher: C,
) -> ImportedApp
where
    C: TokenRefresher,
{
    let tempdir = tempfile::tempdir().unwrap();
    let db = tempdir.path().join("v1-upstream-refresh.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([31u8; 32]);
    let hasher = ApiKeyHasher::new([32u8; 32]);
    let generated = hasher.generate_client_api_key("test");
    ClientApiKeyRepository::new(pool.clone())
        .insert_generated("test", &generated)
        .await
        .unwrap();
    let app = build_router(
        AppState::with_pool_secret_api_key_hasher_and_token_refresher(
            test_config(url, base_url),
            pool.clone(),
            secret_box.clone(),
            hasher,
            token_refresher,
        ),
    );
    let accounts = accounts
        .iter()
        .map(|account| {
            json!({
                "id": account.id,
                "accountId": account.account_id,
                "token": account.token,
                "refreshToken": account.refresh_token,
                "accessTokenExpiresAt": "2999-01-01T00:00:00Z",
                "status": "active"
            })
        })
        .collect::<Vec<_>>();

    let import_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(json!({ "accounts": accounts }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(import_response.status(), StatusCode::OK);

    ImportedApp {
        app,
        pool,
        secret_box,
        client_api_key: generated.plaintext,
        _tempdir: tempdir,
    }
}

pub async fn fetch_v1_event_log(
    pool: &sqlx::SqlitePool,
    request_id: &str,
) -> (String, String, String, i64, Value) {
    let row: (String, String, String, i64, String) = sqlx::query_as(
        "select account_id, route, model, status_code, metadata_json from event_logs where request_id = ? and kind = 'v1.response'",
    )
    .bind(request_id)
    .fetch_one(pool)
    .await
    .unwrap();
    (
        row.0,
        row.1,
        row.2,
        row.3,
        serde_json::from_str(&row.4).unwrap(),
    )
}

pub async fn enable_runtime_logging(app: &axum::Router) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/admin/logs/state")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(json!({ "enabled": true }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
