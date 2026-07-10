use codex_proxy_rs::bootstrap::config::{
    AdminConfig, ApiConfig, AppConfig, AuthConfig, DatabaseConfig, FingerprintConfig,
    LoggingConfig, QuotaConfig, RedisConfig, ServerConfig, TlsConfig, WebSocketPoolConfig,
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
        tls: TlsConfig {
            force_http11: false,
        },
        ws_pool: WebSocketPoolConfig::default(),
        fingerprint: FingerprintConfig::default(),
        admin: AdminConfig {
            session_ttl_minutes: 1440,
            default_username: "admin".to_string(),
            default_password: "test-admin-password".to_string(),
        },
        logging: LoggingConfig {
            directory: "logs".to_string(),
            retention_days: 14,
            enabled: false,
        },
    }
}
