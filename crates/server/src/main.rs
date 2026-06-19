use std::{error::Error, path::Path};

use axum::Router;
use codex_proxy_platform::{
    config::AppConfig,
    crypto::SecretBox,
    identity::ApiKeyHasher,
    logging::{init_tracing, LogGuard, RotationConfig},
    storage::{connect_sqlite, ensure_data_dir, load_or_create_installation_id},
};
use codex_proxy_runtime::{
    state::AppState,
    tasks::coordinator::{start_background_tasks, BackgroundTaskCoordinator},
};
use codex_proxy_server::router;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let config = AppConfig::load()?;
    let host = config.server.host.clone();
    let port = config.server.port;
    let _log_guard = init_logging(&config)?;
    let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
    let (app, task_coordinator) = build_application(config).await?;

    let serve_result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await;

    task_coordinator.shutdown().await;
    serve_result?;

    Ok(())
}

async fn build_application(
    config: AppConfig,
) -> Result<(Router, BackgroundTaskCoordinator), Box<dyn Error>> {
    let pool = connect_sqlite(&config.database.url).await?;
    let secret_box = SecretBox::load_or_create(Path::new(&config.security.master_key_file))?;
    let api_key_hasher =
        ApiKeyHasher::load_or_create(Path::new(&config.security.api_key_pepper_file))?;
    let default_admin_password = config.admin.default_password.clone();
    let data_dir = ensure_data_dir()?;
    let installation_id = load_or_create_installation_id(Some(&data_dir))?;
    let state = AppState::with_pool_secret_api_key_hasher_and_installation_id(
        config,
        pool,
        secret_box,
        api_key_hasher,
        installation_id,
    );
    let created_default_admin = state
        .services
        .admin_sessions
        .ensure_default_admin(&default_admin_password)
        .await?;
    tracing::info!(
        created = created_default_admin,
        "默认管理员账号已完成初始化检查"
    );
    let restored_accounts = state.restore_account_pool_from_repository().await?;
    tracing::info!(count = restored_accounts, "运行时账号池已从 SQLite 恢复");
    let restored_session_affinities = state.restore_session_affinity_from_repository_now().await?;
    tracing::info!(
        count = restored_session_affinities,
        "会话亲和性映射已从 SQLite 恢复"
    );
    let task_coordinator = start_background_tasks(&state).await;
    Ok((router::router().with_state(state), task_coordinator))
}

fn init_logging(config: &AppConfig) -> Result<Option<LogGuard>, Box<dyn Error>> {
    if !config.logging.enabled {
        return Ok(None);
    }

    let rotation = RotationConfig::new(&config.logging.directory, config.logging.retention_days);
    Ok(Some(init_tracing(rotation)?))
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        eprintln!("failed to listen for shutdown signal: {error}");
    }
}
