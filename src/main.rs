use codex_proxy_rs::{
    app::build_router,
    config::{
        AdminConfig, ApiConfig, AppConfig, AuthConfig, DatabaseConfig, LoggingConfig,
        SecurityConfig, ServerConfig, TlsConfig,
    },
    logs::rotation::{init_tracing, RotationConfig},
    state::AppState,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = AppConfig {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 8080,
        },
        api: ApiConfig {
            base_url: "https://chatgpt.com/backend-api".to_string(),
        },
        auth: AuthConfig {
            refresh_margin_seconds: 300,
            refresh_enabled: true,
            refresh_concurrency: 2,
        },
        database: DatabaseConfig {
            url: "sqlite://data/codex-proxy-rs.sqlite".to_string(),
        },
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
        },
    };

    let _log_writer = init_tracing(RotationConfig::new(
        &config.logging.directory,
        config.logging.max_file_bytes,
        config.logging.retention_days,
    ))?;
    let host = config.server.host.clone();
    let port = config.server.port;
    let app = build_router(AppState::new(config));
    let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
    tracing::info!(host, port, "codex-proxy-rs listening");
    axum::serve(listener, app).await?;
    Ok(())
}
