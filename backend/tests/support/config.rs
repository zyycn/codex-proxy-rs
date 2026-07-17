use codex_proxy_rs::bootstrap::config::{
    AdminConfig, ApiConfig, AppConfig, AuthConfig, DatabaseConfig, FileLoggingConfig,
    LoggingConfig, QuotaConfig, RedisConfig, RuntimePathsConfig, ServerConfig, TelemetryConfig,
    WebSocketPoolSettings,
};

pub(crate) fn test_config(database_url: String) -> AppConfig {
    AppConfig {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
        },
        api: ApiConfig {
            base_url: "https://chatgpt.com/backend-api".to_string(),
        },
        model_aliases: std::collections::BTreeMap::new(),
        auth: AuthConfig {
            refresh_margin_seconds: 300,
            refresh_enabled: true,
            refresh_concurrency: 2,
            max_concurrent_per_account: 3,
            request_interval_ms: 50,
            rotation_strategy: "smart".to_string(),
            tier_priority: Vec::new(),
            oauth_client_id: "app_EMoamEEZ73f0CkXaXp7hrann".to_string(),
            oauth_token_endpoint: "https://auth.openai.com/oauth/token".to_string(),
        },
        quota: QuotaConfig {
            refresh_interval_minutes: 5,
            skip_exhausted: true,
        },
        database: DatabaseConfig { url: database_url },
        redis: RedisConfig {
            url: crate::support::storage::test_redis_url(),
        },
        runtime: RuntimePathsConfig {
            data_directory: std::path::PathBuf::from(".runtime/test-data"),
        },
        ws_pool: WebSocketPoolSettings {
            enabled: true,
            max_age_ms: 3_300_000,
            max_per_account: 8,
            max_total: 64,
            max_connecting: 16,
            initial_event_timeout_ms: 20_000,
        },
        wire_profile: crate::support::wire_profile::test_wire_profile_config(),
        admin: AdminConfig {
            session_ttl_minutes: 1440,
            default_username: "admin".to_string(),
        },
        logging: LoggingConfig {
            level: "info".to_string(),
            stdout: false,
            file: FileLoggingConfig {
                enabled: false,
                directory: std::path::PathBuf::from("logs"),
                retention_days: 14,
                max_file_size_mb: 20,
                max_files: 20,
            },
        },
        telemetry: TelemetryConfig { enabled: false },
    }
}
