use codex_proxy_rs::{
    codex::logs::rotation::{init_tracing, RotationConfig},
    config::AppConfig,
    runtime::{bootstrap::build_state, build_router, tasks::start_background_tasks},
};
use tokio::signal;

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
    let (state, db_pool, restored_accounts) = build_state(config.clone()).await?;
    tracing::info!(restored_accounts, "account pool restored from sqlite");

    let background_tasks = start_background_tasks(&state, db_pool.clone(), &config).await;

    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
    tracing::info!(host, port, "codex-proxy-rs listening");

    // 运行服务器，同时监听关闭信号
    tokio::select! {
        result = axum::serve(listener, app) => {
            if let Err(e) = result {
                tracing::error!(error = %e, "server error");
            }
        }
        _ = signal::ctrl_c() => {
            tracing::info!("received shutdown signal");
        }
    }

    background_tasks.shutdown().await;

    Ok(())
}
