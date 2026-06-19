use codex_proxy_adapters::codex::fingerprint::FingerprintRepository;
use codex_proxy_core::models::model::{BackendModelEntry, ModelPlanSnapshot};
use codex_proxy_platform::storage::connect_sqlite;
use std::collections::BTreeMap;

use codex_proxy_platform::config::{
    AdminConfig, ApiConfig, AppConfig, AuthConfig, DatabaseConfig, LoggingConfig, ModelConfig,
    QuotaConfig, QuotaWarningThresholds, SecurityConfig, ServerConfig, TlsConfig, UsageStatsConfig,
};
use codex_proxy_platform::crypto::SecretBox;

#[tokio::test]
async fn runtime_should_load_latest_auto_updated_fingerprint_when_present() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("fingerprints.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let repo = FingerprintRepository::new(pool);
    repo.upsert_auto_update("26.900.1", "7001", Some("147"))
        .await
        .expect("insert fingerprint");

    let stored = codex_proxy_runtime::bootstrap::load_runtime_fingerprint(&repo).await;

    assert_eq!(stored.app_version, "26.900.1");
}

#[tokio::test]
async fn runtime_should_build_state_with_model_service() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("runtime-state.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let state = codex_proxy_runtime::state::AppState::with_pool_and_secret_box(
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
            database: DatabaseConfig {
                url: format!("sqlite://{}", db.display()),
            },
            security: SecurityConfig {
                master_key_file: "data/master.key".to_string(),
                api_key_pepper_file: "data/api-key-pepper.key".to_string(),
            },
            tls: TlsConfig {
                force_http11: false,
            },
            ws_pool: Default::default(),
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
        },
        pool,
        SecretBox::new([42u8; 32]),
    );

    let catalog = state.services.models.catalog().await;
    assert_eq!(catalog.models().len(), 1);
}

#[tokio::test]
async fn runtime_state_should_expose_backend_model_snapshot_through_model_service() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("runtime-models.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let config = AppConfig {
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
        database: DatabaseConfig {
            url: format!("sqlite://{}", db.display()),
        },
        security: SecurityConfig {
            master_key_file: "data/master.key".to_string(),
            api_key_pepper_file: "data/api-key-pepper.key".to_string(),
        },
        tls: TlsConfig {
            force_http11: false,
        },
        ws_pool: Default::default(),
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
    };
    let snapshot_store =
        codex_proxy_adapters::sqlite::models::ModelSnapshotRepository::new(pool.clone());
    snapshot_store
        .replace_plan_snapshot(&ModelPlanSnapshot::from_backend_entries(
            "plus",
            vec![BackendModelEntry {
                id: Some("gpt-6".to_string()),
                name: Some("GPT-6".to_string()),
                ..BackendModelEntry::default()
            }],
        ))
        .await
        .expect("replace snapshot");

    let state = codex_proxy_runtime::state::AppState::with_pool_and_secret_box(
        config,
        pool,
        SecretBox::new([43u8; 32]),
    );

    let catalog = state.services.models.catalog().await;
    assert_eq!(catalog.models()[0].id, "gpt-6");
}
