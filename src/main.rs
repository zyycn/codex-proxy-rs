use codex_proxy_rs::{
    app::build_router,
    config::AppConfig,
    crypto::SecretBox,
    logs::rotation::{init_tracing, RotationConfig},
    state::AppState,
    storage::db::connect_sqlite,
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
    let secret_box = SecretBox::load_or_create(&config.security.master_key_file)?;
    let pool = connect_sqlite(&config.database.url).await?;
    let state = AppState::with_pool_and_secret_box(config, pool, secret_box);
    let restored_accounts = state.reload_account_pool_from_repository().await?;
    tracing::info!(restored_accounts, "account pool restored from sqlite");
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
    tracing::info!(host, port, "codex-proxy-rs listening");
    axum::serve(listener, app).await?;
    Ok(())
}
