use chrono::{Duration, Utc};
use secrecy::SecretString;

use codex_proxy_rs::{
    app::state::AppState,
    codex::accounts::{
        model::AccountStatus,
        repository::{NewAccount, UsageDelta},
    },
    config::{
        AdminConfig, ApiConfig, AppConfig, AuthConfig, DatabaseConfig, LoggingConfig, ModelConfig,
        QuotaConfig, QuotaWarningThresholds, SecurityConfig, ServerConfig, TlsConfig,
        UsageStatsConfig,
    },
    storage::db::connect_sqlite,
    utils::crypto::SecretBox,
};

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
            aliases: Default::default(),
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

#[tokio::test]
async fn app_state_should_restore_account_pool_from_sqlite_accounts() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("startup-pool.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let secret_box = SecretBox::new([31u8; 32]);
    let repo = codex_proxy_rs::codex::accounts::repository::AccountRepository::new(
        pool.clone(),
        secret_box.clone(),
    );
    repo.insert(NewAccount {
        id: "acct_restored".to_string(),
        email: Some("user@example.com".to_string()),
        account_id: Some("chatgpt-account".to_string()),
        user_id: None,
        label: Some("primary".to_string()),
        plan_type: Some("plus".to_string()),
        access_token: SecretString::new("access-secret".to_string().into()),
        refresh_token: Some(SecretString::new("refresh-secret".to_string().into())),
        access_token_expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
        status: AccountStatus::Active,
    })
    .await
    .unwrap();
    repo.record_usage(
        "acct_restored",
        UsageDelta {
            input_tokens: 10,
            output_tokens: 3,
            cached_tokens: 2,
        },
    )
    .await
    .unwrap();
    let state = AppState::with_pool_and_secret_box(test_config(url), pool, secret_box);

    let restored = state.reload_account_pool_from_repository().await.unwrap();

    assert_eq!(restored, 1);
    let acquired = state
        .services
        .accounts
        .acquire_runtime_account("gpt-5.5")
        .await
        .unwrap();
    assert_eq!(acquired.id, "acct_restored");
    assert_eq!(acquired.access_token, "access-secret");
    assert!(acquired.last_used_at.is_some());
}

#[tokio::test]
async fn app_state_should_restore_persisted_cooldowns_into_runtime_pool() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("startup-pool-cooldown.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let secret_box = SecretBox::new([32u8; 32]);
    let repo = codex_proxy_rs::codex::accounts::repository::AccountRepository::new(
        pool.clone(),
        secret_box.clone(),
    );
    repo.insert(NewAccount {
        id: "acct_cooling".to_string(),
        email: Some("cooling@example.com".to_string()),
        account_id: Some("chatgpt-cooling".to_string()),
        user_id: None,
        label: None,
        plan_type: Some("plus".to_string()),
        access_token: SecretString::new("access-cooling".to_string().into()),
        refresh_token: Some(SecretString::new("refresh-cooling".to_string().into())),
        access_token_expires_at: Some(Utc::now() + Duration::hours(1)),
        status: AccountStatus::Active,
    })
    .await
    .unwrap();
    let cooldown_until = (Utc::now() + Duration::minutes(10)).to_rfc3339();
    sqlx::query(
        "update accounts set quota_limit_reached = 1, quota_cooldown_until = ?, cloudflare_cooldown_until = ? where id = ?",
    )
    .bind(&cooldown_until)
    .bind(&cooldown_until)
    .bind("acct_cooling")
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::with_pool_and_secret_box(test_config(url), pool, secret_box);

    let restored = state.reload_account_pool_from_repository().await.unwrap();

    assert_eq!(restored, 1);
    assert!(state
        .services
        .accounts
        .acquire_runtime_account("gpt-5.5")
        .await
        .is_none());
}
