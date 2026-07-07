//! 后台任务协调器。

use std::{path::PathBuf, sync::Arc, time::Duration};

use tokio::task::JoinHandle;

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
                TokenRefreshTask::from_service((*services.token_refresh).clone()).start(),
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
        coordinator.push("fingerprint_update", {
            let fingerprint = services.fingerprint.snapshot();
            start_fingerprint_update_task(
                stores.fingerprints.clone(),
                services.fingerprint.clone(),
                CODEX_DESKTOP_APPCAST_URL.to_string(),
                PathBuf::from(DEFAULT_EXTRACTED_FINGERPRINT_PATH),
                fingerprint.app_version,
                fingerprint.build_number,
            )
        });
        if let Some(pool) = &services.websocket_pool {
            coordinator.push(
                "websocket_pool",
                SchedulerHandle::from_websocket_pool(pool.clone()),
            );
        }

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

/// 后台任务关闭句柄。
pub enum SchedulerHandle {
    /// 通过 channel 发送关闭信号。
    Channel {
        shutdown_tx: tokio::sync::mpsc::Sender<()>,
        handle: JoinHandle<()>,
    },
    /// 关闭上游 WebSocket 连接池。
    WebSocketPool(Arc<crate::upstream::transport::CodexWebSocketPool>),
}

impl SchedulerHandle {
    /// 使用关闭 channel 构造句柄。
    pub(crate) fn new(shutdown_tx: tokio::sync::mpsc::Sender<()>, handle: JoinHandle<()>) -> Self {
        Self::Channel {
            shutdown_tx,
            handle,
        }
    }

    /// 使用 WebSocket 连接池构造句柄。
    pub fn from_websocket_pool(pool: Arc<crate::upstream::transport::CodexWebSocketPool>) -> Self {
        Self::WebSocketPool(pool)
    }

    /// 关闭任务。
    pub async fn shutdown(self) {
        match self {
            Self::Channel {
                shutdown_tx,
                handle,
            } => {
                let _ = shutdown_tx.send(()).await;
                wait_for_task_shutdown(handle).await;
            }
            Self::WebSocketPool(pool) => {
                pool.shutdown().await;
            }
        }
    }
}

const TASK_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

async fn wait_for_task_shutdown(handle: JoinHandle<()>) {
    let mut handle = handle;
    let timeout = tokio::time::sleep(TASK_SHUTDOWN_TIMEOUT);
    tokio::pin!(timeout);

    tokio::select! {
        result = &mut handle => {
            log_task_shutdown_result(result);
        }
        () = &mut timeout => {
            tracing::warn!(timeout_secs = TASK_SHUTDOWN_TIMEOUT.as_secs(), "等待后台任务关闭超时");
            handle.abort();
            log_task_shutdown_result(handle.await);
        }
    }
}

fn log_task_shutdown_result(result: Result<(), tokio::task::JoinError>) {
    match result {
        Ok(()) => {}
        Err(error) if error.is_cancelled() => {}
        Err(error) => {
            tracing::warn!(error = %error, "后台任务关闭时异常退出");
        }
    }
}
