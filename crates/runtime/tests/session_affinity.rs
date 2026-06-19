use chrono::{Duration, TimeZone, Utc};
use codex_proxy_adapters::sqlite::session_affinity::SqliteSessionAffinityStore;
use codex_proxy_core::serving::affinity::SessionAffinityEntry;
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

#[tokio::test]
async fn app_state_should_restore_session_affinity_from_sqlite() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("runtime-session-affinity.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.expect("sqlite pool");
    let store = SqliteSessionAffinityStore::new(pool.clone());
    let now = Utc.with_ymd_and_hms(2026, 6, 18, 12, 0, 0).unwrap();

    store
        .upsert(
            "resp_restore",
            &SessionAffinityEntry {
                account_id: "acct_restore".to_string(),
                conversation_id: "conv_restore".to_string(),
                turn_state: Some("turn_restore".to_string()),
                instructions_hash: Some("hash_restore".to_string()),
                input_tokens: Some(7),
                function_call_ids: vec!["call_restore".to_string()],
                variant_hash: Some("variant_restore".to_string()),
                created_at: now,
            },
            Duration::hours(4),
        )
        .await
        .expect("affinity should be stored");

    let state =
        AppState::with_pool_and_secret_box(test_config(url), pool, SecretBox::new([33u8; 32]));
    let restored = state
        .restore_session_affinity_from_repository(now + Duration::minutes(1))
        .await
        .expect("session affinity should restore");

    assert_eq!(restored, 1);
    assert_eq!(
        state
            .services
            .session_affinity
            .lookup_account("resp_restore", now + Duration::minutes(1))
            .await,
        Some("acct_restore".to_string())
    );
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
