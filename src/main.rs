use codex_proxy_rs::{
    app::{bootstrap::build_state, build_router},
    config::AppConfig,
    logs::rotation::{init_tracing, RotationConfig},
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
    let (state, restored_accounts) = build_state(config).await?;
    tracing::info!(restored_accounts, "account pool restored from sqlite");
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
    tracing::info!(host, port, "codex-proxy-rs listening");
    axum::serve(listener, app).await?;
    Ok(())
}
