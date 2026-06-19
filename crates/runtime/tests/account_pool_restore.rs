use std::collections::BTreeMap;

use chrono::{TimeZone, Utc};
use codex_proxy_platform::{
    config::{
        AdminConfig, ApiConfig, AppConfig, AuthConfig, DatabaseConfig, LoggingConfig, ModelConfig,
        QuotaConfig, QuotaWarningThresholds, SecurityConfig, ServerConfig, TlsConfig,
        UsageStatsConfig, WebSocketPoolConfig,
    },
    crypto::SecretBox,
    storage::connect_sqlite,
};
use codex_proxy_runtime::state::AppState;
use secrecy::SecretString;

#[tokio::test]
async fn app_state_should_restore_active_accounts_into_runtime_pool() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("runtime-account-pool.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.expect("sqlite pool");
    let secret_box = SecretBox::new([34u8; 32]);
    insert_account(&pool, &secret_box, "acct_pool", "active").await;

    let state = AppState::with_pool_and_secret_box(test_config(url), pool, secret_box);
    let restored = state
        .restore_account_pool_from_repository()
        .await
        .expect("account pool should restore");
    let acquired = state
        .services
        .account_pool
        .acquire(
            "gpt-5.5",
            Utc.with_ymd_and_hms(2026, 6, 18, 12, 0, 0).unwrap(),
        )
        .await
        .expect("active account should be acquired");

    assert_eq!(restored, 1);
    assert_eq!(acquired.account.id, "acct_pool");
}

async fn insert_account(pool: &sqlx::SqlitePool, secret_box: &SecretBox, id: &str, status: &str) {
    let access_token = secret_box
        .encrypt(&SecretString::new(format!("access-{id}").into()))
        .expect("access token should encrypt");
    sqlx::query(
        "insert into accounts (id, email, account_id, user_id, access_token_cipher, access_token_expires_at, status, added_at, updated_at) values (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(format!("{id}@example.com"))
    .bind(format!("chatgpt-{id}"))
    .bind(format!("user-{id}"))
    .bind(access_token)
    .bind("2026-06-18T13:00:00Z")
    .bind(status)
    .bind("2026-06-18T00:00:00Z")
    .bind("2026-06-18T00:00:00Z")
    .execute(pool)
    .await
    .expect("account should be inserted");
}

fn test_config(database_url: String) -> AppConfig {
    AppConfig {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
        },
        api: ApiConfig {
            base_url: "https://example.invalid".to_string(),
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
            oauth_client_id: "app_id".to_string(),
            oauth_auth_endpoint: "https://auth.invalid".to_string(),
            oauth_token_endpoint: "https://token.invalid".to_string(),
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
