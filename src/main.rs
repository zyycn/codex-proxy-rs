use codex_proxy_rs::{
    config::AppConfig,
    platform::logging::rotation::{init_tracing, RotationConfig},
    runtime::{bootstrap::build_state, build_router, tasks::start_background_tasks},
};
use tokio::signal;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = AppConfig::load()?;

    let _log_guard = init_tracing(RotationConfig::new(
        &config.logging.directory,
        config.logging.retention_days,
    ))?;

    let host = config.server.host.clone();
    let port = config.server.port;
    let (state, db_pool, restored_accounts) = build_state(config.clone()).await?;
    tracing::info!(account_count = restored_accounts, "账户池已从 SQLite 恢复");

    let background_tasks = start_background_tasks(&state, db_pool.clone(), &config).await;

    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
    tracing::info!(host, port, "codex-proxy-rs 已开始监听");

    tokio::select! {
        result = axum::serve(listener, app) => {
            if let Err(e) = result {
                tracing::error!(error = %e, "服务器运行失败");
            }
        }
        _ = signal::ctrl_c() => {
            tracing::info!("收到关闭信号");
        }
    }

    background_tasks.shutdown().await;

    Ok(())
}
