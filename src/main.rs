use codex_proxy_rs::{
    app::build_router,
    config::AppConfig,
    logs::rotation::{init_tracing, RotationConfig},
    state::AppState,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = AppConfig::load()?;

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
