//! 后台任务协调器。

use std::path::PathBuf;

use tokio::task::JoinHandle;

use crate::upstream::token_client::{default_openai_token_client, TokenClientConfig};

use super::{
    cookie_cleanup::CookieCleanupTask,
    fingerprint_update::{
        start_fingerprint_update_task, CODEX_DESKTOP_APPCAST_URL,
        DEFAULT_EXTRACTED_FINGERPRINT_PATH,
    },
    model_refresh::ModelRefreshTask,
    quota_refresh::QuotaRefreshTask,
    session_affinity_cleanup::SessionAffinityCleanupTask,
    session_cleanup::SessionCleanupTask,
    token_refresh::TokenRefreshTask,
};

/// 后台任务协调器。
#[derive(Default)]
pub struct TaskCoordinator {
    handles: Vec<(&'static str, SchedulerHandle)>,
}

impl TaskCoordinator {
    /// 注册一个后台任务句柄。
    pub(crate) fn push(&mut self, name: &'static str, handle: SchedulerHandle) {
        self.handles.push((name, handle));
    }

    /// 启动所有后台任务。
    pub(crate) fn start(
        config: &crate::runtime::state::RuntimeConfig,
        services: &crate::runtime::services::Services,
    ) -> Self {
        let mut coordinator = TaskCoordinator::default();
        let stores = &services.background_tasks;

        coordinator.push(
            "cookie_cleanup",
            CookieCleanupTask::new(stores.cookies.clone()).start(),
        );
        coordinator.push(
            "session_cleanup",
            SessionCleanupTask::new(
                stores.admin_sessions.clone(),
                config.admin.session_cleanup_interval_secs,
            )
            .start(),
        );
        coordinator.push(
            "session_affinity_cleanup",
            SessionAffinityCleanupTask::new(
                stores.session_affinity.clone(),
                config.admin.session_cleanup_interval_secs,
            )
            .start(),
        );
        coordinator.push(
            "model_refresh",
            ModelRefreshTask::new(services.models.clone(), services.account_pool.clone())
                .with_installation_id(services.installation_id.clone())
                .start(),
        );
        if config.auth.refresh_enabled {
            coordinator.push(
                "token_refresh",
                TokenRefreshTask::new(
                    stores.accounts.clone(),
                    services.refresh_policy.clone(),
                    default_openai_token_client(token_client_config(config)),
                )
                .with_refresh_lease_store(stores.refresh_leases.clone())
                .start(),
            );
        }
        coordinator.push(
            "quota_refresh",
            QuotaRefreshTask::with_intervals(
                stores.accounts.clone(),
                services.codex.clone(),
                config
                    .quota
                    .refresh_interval_minutes
                    .saturating_mul(60)
                    .max(1),
                30 * 60,
            )
            .with_installation_id(services.installation_id.clone())
            .with_cookie_store(stores.cookies.clone())
            .with_account_pool(services.account_pool.clone())
            .start(),
        );
        coordinator.push(
            "fingerprint_update",
            start_fingerprint_update_task(
                Some(stores.fingerprints.clone()),
                CODEX_DESKTOP_APPCAST_URL.to_string(),
                PathBuf::from(DEFAULT_EXTRACTED_FINGERPRINT_PATH),
                services.fingerprint.app_version.clone(),
                services.fingerprint.build_number.clone(),
            ),
        );

        coordinator
    }

    /// 关闭所有后台任务。
    pub async fn shutdown(self) {
        tracing::info!("正在关闭后台任务");
        for (name, handle) in self.handles {
            handle.shutdown().await;
            tracing::info!(task = name, "后台任务已停止");
        }
        tracing::info!("所有后台任务已停止");
    }
}

fn token_client_config(config: &crate::runtime::state::RuntimeConfig) -> TokenClientConfig {
    TokenClientConfig {
        client_id: config.auth.oauth_client_id.clone(),
        token_endpoint: config.auth.oauth_token_endpoint.clone(),
    }
}

/// 后台任务关闭句柄。
pub enum SchedulerHandle {
    /// 通过 channel 发送关闭信号。
    Channel(tokio::sync::mpsc::Sender<()>),
    /// 直接持有 tokio 任务句柄。
    JoinHandle(JoinHandle<()>),
}

impl SchedulerHandle {
    /// 使用关闭 channel 构造句柄。
    pub(crate) fn new(shutdown_tx: tokio::sync::mpsc::Sender<()>) -> Self {
        Self::Channel(shutdown_tx)
    }

    /// 使用 `JoinHandle` 构造句柄。
    pub(crate) fn from_join_handle(handle: JoinHandle<()>) -> Self {
        Self::JoinHandle(handle)
    }

    /// 关闭任务。
    pub async fn shutdown(self) {
        match self {
            Self::Channel(shutdown_tx) => {
                let _ = shutdown_tx.send(()).await;
            }
            Self::JoinHandle(handle) => {
                handle.abort();
            }
        }
    }
}
