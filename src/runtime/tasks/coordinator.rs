use sqlx::SqlitePool;

use crate::{
    admin::tasks::session_cleanup::SessionCleanupScheduler,
    codex::{
        gateway::fingerprint::{
            model::Fingerprint, repository::FingerprintRepository, update_checker::UpdateChecker,
        },
        tasks::{model::ModelRefresher, quota::QuotaRefresher, refresh::RefreshScheduler},
    },
    config::AppConfig,
    runtime::state::AppState,
};

use super::types::SchedulerHandle;

#[derive(Default)]
pub struct BackgroundTaskCoordinator {
    handles: Vec<(&'static str, SchedulerHandle)>,
}

impl BackgroundTaskCoordinator {
    fn push(&mut self, name: &'static str, handle: SchedulerHandle) {
        self.handles.push((name, handle));
    }

    pub async fn shutdown(self) {
        tracing::info!("shutting down background tasks");
        for (name, handle) in self.handles {
            handle.shutdown().await;
            tracing::info!(task = name, "background task stopped");
        }
        tracing::info!("all background tasks stopped");
    }
}

pub async fn start_background_tasks(
    state: &AppState,
    db_pool: SqlitePool,
    config: &AppConfig,
) -> BackgroundTaskCoordinator {
    let mut coordinator = BackgroundTaskCoordinator::default();

    // 加载指纹：优先数据库 auto_update，否则使用默认
    let fingerprint_repo = FingerprintRepository::new(db_pool.clone());
    let fingerprint = match fingerprint_repo.load_latest_auto_updated().await {
        Ok(Some(fp)) => {
            tracing::info!(
                version = %fp.app_version,
                build = %fp.build_number,
                "loaded fingerprint from database (auto_update)"
            );
            fp
        }
        Ok(None) => {
            let fp = Fingerprint::default_codex_desktop();
            tracing::info!(
                version = %fp.app_version,
                build = %fp.build_number,
                "using default fingerprint"
            );
            fp
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to load fingerprint from database, using default");
            Fingerprint::default_codex_desktop()
        }
    };

    // 指纹自动更新（3 天轮询一次）
    let update_checker = UpdateChecker::new(
        Some(db_pool.clone()),
        fingerprint.app_version.clone(),
        fingerprint.build_number.clone(),
    );
    let update_handle = update_checker.start_background_checker();
    coordinator.push(
        "fingerprint_update",
        SchedulerHandle::from_join_handle(update_handle),
    );
    tracing::info!("fingerprint update checker started");

    let refresh_scheduler = RefreshScheduler::new(state.account_service(), config.clone());
    coordinator.push("refresh", refresh_scheduler.start().await);
    tracing::info!("refresh scheduler started");

    let session_cleanup =
        SessionCleanupScheduler::new(db_pool, config.admin.session_cleanup_interval_secs);
    coordinator.push("session_cleanup", session_cleanup.start());
    tracing::info!("session cleanup scheduler started");

    let quota_refresher = QuotaRefresher::new(state.account_service());
    coordinator.push("quota", quota_refresher.start());
    tracing::info!("quota refresher started");

    let model_refresher = ModelRefresher::new(state.model_service());
    coordinator.push("model", model_refresher.start());
    tracing::info!("model refresher started");

    coordinator
}
