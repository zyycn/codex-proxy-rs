use std::error::Error;

use axum::Router;
use chrono::Utc;

use crate::config::settings::RuntimeSettingsService;
use crate::config::types::AppConfig;
use crate::infra::database::connect_sqlite;
use crate::infra::logging::{init_tracing, LogGuard, RotationConfig};
use crate::infra::paths::{ensure_data_dir, load_or_create_installation_id};
use crate::runtime::services::{BackgroundTaskStores, Services};
use crate::runtime::shutdown::shutdown_signal;
use crate::runtime::state::{AppState, RuntimeConfig};
use crate::runtime::tasks::coordinator::TaskCoordinator;
use crate::upstream::fingerprint::{Fingerprint, FingerprintRepository};

pub async fn run() -> Result<(), Box<dyn Error + Send + Sync>> {
    let config = AppConfig::load()?;
    let host = config.server.host.clone();
    let port = config.server.port;
    let _log_guard = init_logging(&config)?;
    let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
    let (app, task_coordinator) = build_application(config).await?;

    let serve_result = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await;

    task_coordinator.shutdown().await;
    serve_result?;

    Ok(())
}

async fn build_application(
    config: AppConfig,
) -> Result<(Router, TaskCoordinator), Box<dyn Error + Send + Sync>> {
    let pool = connect_sqlite(&config.database.url).await?;
    let config = RuntimeSettingsService::load_or_initialize_config(config, &pool).await?;

    let fingerprint_repository = FingerprintRepository::new(pool.clone());
    let stores = BackgroundTaskStores {
        accounts: crate::upstream::accounts::store::SqliteAccountStore::new(pool.clone()),
        admin_sessions: crate::admin::auth::service::SqliteAdminSessionStore::new(pool.clone()),
        cookies: crate::upstream::accounts::cookies::SqliteCookieStore::new(pool.clone()),
        fingerprints: fingerprint_repository.clone(),
        session_affinity: crate::proxy::dispatch::session_affinity::SqliteSessionAffinityStore::new(
            pool.clone(),
        ),
        refresh_leases: crate::upstream::accounts::token_refresh::RefreshLeaseStore::new(
            pool.clone(),
        ),
        client_keys: crate::admin::keys::service::SqliteClientKeyStore::new(pool.clone()),
        usage_records: crate::admin::monitoring::usage_record_store::SqliteUsageRecordStore::new(
            pool,
        ),
    };

    let default_admin_password = config.admin.default_password.clone();
    let data_dir = ensure_data_dir()?;
    let installation_id = load_or_create_installation_id(Some(&data_dir))?;
    let default_fingerprint = Fingerprint::from_config(&config.fingerprint);
    let runtime_fingerprint = fingerprint_repository
        .ensure_current_seed(&default_fingerprint)
        .await?;
    let runtime_config = RuntimeConfig::from(config.clone());
    let services = Services::try_with_installation_id(
        &config,
        stores,
        runtime_fingerprint,
        Some(installation_id),
    )?;
    services.initialize_hot_path_state().await?;

    let created_default_admin = services
        .admin_sessions
        .ensure_default_admin(&default_admin_password)
        .await?;
    tracing::info!(
        created = created_default_admin,
        "默认管理员账号已完成初始化检查"
    );

    let restored_accounts = services.account_pool.restore_from_repository().await?;
    tracing::info!(count = restored_accounts, "运行时账号池已从 SQLite 恢复");

    let restored_session_affinities = services
        .session_affinity
        .restore_from_repository(Utc::now())
        .await?;
    tracing::info!(
        count = restored_session_affinities,
        "会话亲和性映射已从 SQLite 恢复"
    );

    let task_coordinator = TaskCoordinator::start(&runtime_config, &services);
    let app_state = AppState { config, services };

    Ok((
        crate::http::router::router().with_state(app_state),
        task_coordinator,
    ))
}

fn init_logging(config: &AppConfig) -> Result<Option<LogGuard>, crate::infra::logging::LogError> {
    if !config.logging.enabled {
        return Ok(None);
    }

    let rotation = RotationConfig::new(&config.logging.directory, config.logging.retention_days);
    Ok(Some(init_tracing(&rotation)?))
}
