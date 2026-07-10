use std::{env, error::Error, time::Duration};

use axum::Router;

use crate::bootstrap::config::AppConfig;
use crate::bootstrap::services::{
    apply_settings_to_config, fingerprint_from_config, settings_snapshot_from_config,
    BackgroundTaskStores, Services,
};
use crate::bootstrap::shutdown::{restart_executable_path, shutdown_signal};
use crate::bootstrap::state::{AppState, RuntimeConfig};
use crate::bootstrap::tasks::coordinator::TaskCoordinator;
use crate::infra::database::connect;
use crate::infra::logging::{init_tracing, LogGuard, RotationConfig};
use crate::infra::paths::{ensure_data_dir, load_or_create_installation_id};
use crate::infra::redis::RedisConnection;
use crate::settings::service::RuntimeSettingsService;
use crate::upstream::openai::fingerprint::{PgFingerprintStore, RuntimeFingerprint};

const RESTART_DELAY_ENV: &str = "CPR_RESTART_DELAY_MS";

pub async fn run() -> Result<(), Box<dyn Error + Send + Sync>> {
    wait_for_scheduled_restart().await;

    let config = AppConfig::load()?;
    let host = config.server.host.clone();
    let port = config.server.port;
    let log_guard = init_logging(&config)?;
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

    if let Some(executable_path) = restart_executable_path() {
        tracing::info!(
            executable = %executable_path.display(),
            "正在以更新后的二进制替换当前进程"
        );
        drop(log_guard);
        exec_replacement_process(executable_path)?;
    }

    Ok(())
}

async fn build_application(
    mut config: AppConfig,
) -> Result<(Router, TaskCoordinator), Box<dyn Error + Send + Sync>> {
    let pool = connect(&config.database.url).await?;
    let redis = RedisConnection::connect(&config.redis.url, "cpr").await?;
    let settings =
        RuntimeSettingsService::load_or_initialize(settings_snapshot_from_config(&config), &pool)
            .await?;
    apply_settings_to_config(&mut config, &settings);

    let fingerprint_store = PgFingerprintStore::new(pool.clone());
    let stores = BackgroundTaskStores {
        redis: redis.clone(),
        accounts: crate::accounts::store::PgAccountStore::new(pool.clone()),
        admin_users: crate::auth::store::PgAdminUserStore::new(pool.clone()),
        admin_sessions: crate::auth::store::RedisAdminSessionStore::new(redis.clone()),
        cookies: crate::accounts::cookies::PgCookieStore::new(pool.clone()),
        fingerprints: fingerprint_store.clone(),
        session_affinity: crate::dispatch::affinity::RedisSessionAffinityStore::new(redis.clone()),
        refresh_leases: crate::accounts::refresh::RedisRefreshLeaseStore::new(redis.clone()),
        model_snapshots: crate::models::store::RedisModelSnapshotStore::new(redis.clone()),
        client_keys: crate::keys::store::PgClientKeyStore::new(pool.clone()),
        usage_records: crate::telemetry::usage::store::PgUsageRecordStore::new(pool.clone()),
        ops_errors: crate::telemetry::ops::store::PgOpsErrorLogStore::new(pool.clone()),
        account_usage: crate::telemetry::account_usage::store::PgAccountUsageStore::new(
            pool.clone(),
        ),
        request_buckets: crate::telemetry::buckets::store::PgRequestBucketStore::new(pool),
    };

    config.admin.validate_default_password()?;
    let default_admin_password = config.admin.default_password.clone();
    let data_dir = ensure_data_dir()?;
    let installation_id = load_or_create_installation_id(Some(&data_dir))?;
    let default_fingerprint = fingerprint_from_config(&config.fingerprint);
    let runtime_fingerprint = fingerprint_store
        .ensure_current_seed(&default_fingerprint)
        .await?;
    let runtime_fingerprint = RuntimeFingerprint::new(runtime_fingerprint);
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

    let restored_accounts = services.account_pool.restore_from_store().await?;
    tracing::info!(
        count = restored_accounts,
        "运行时账号池已从 PostgreSQL 恢复"
    );

    let task_coordinator = TaskCoordinator::start(&runtime_config, &services);
    let app_state = AppState { services };

    Ok((
        crate::api::router::router().with_state(app_state),
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

async fn wait_for_scheduled_restart() {
    let Some(delay_ms) = env::var(RESTART_DELAY_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
    else {
        return;
    };

    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
}

#[cfg(unix)]
fn exec_replacement_process(
    executable_path: std::path::PathBuf,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    let error = Command::new(executable_path)
        .args(env::args_os().skip(1))
        .exec();
    Err(Box::new(error))
}

#[cfg(not(unix))]
fn exec_replacement_process(
    _executable_path: std::path::PathBuf,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    Err(Box::new(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "process exec restart is only supported on Unix",
    )))
}
