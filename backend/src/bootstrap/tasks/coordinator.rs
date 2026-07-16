//! 后台任务协调器。

use std::{future::Future, sync::Arc, time::Duration};

use tokio::task::JoinHandle;

use super::{
    cookie_cleanup::CookieCleanupTask, desktop_release_update::start_desktop_release_update_task,
    model_refresh::ModelRefreshTask, quota_refresh::QuotaRefreshTask,
    retention_trim::RetentionTrimTask, token_refresh::TokenRefreshTask,
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
        config: &crate::bootstrap::state::RuntimeConfig,
        services: &crate::bootstrap::services::Services,
    ) -> Self {
        let mut coordinator = Self::start_settings_subscriptions(services);
        let stores = &services.background_tasks;

        coordinator.push(
            "cookie_cleanup",
            CookieCleanupTask::new(stores.cookies.clone()).start(),
        );
        coordinator.push(
            "retention_trim",
            RetentionTrimTask::new(
                stores.usage_records.clone(),
                stores.ops_errors.clone(),
                stores.request_buckets.clone(),
            )
            .start(),
        );
        coordinator.push(
            "model_refresh",
            ModelRefreshTask::new(
                services.models.clone(),
                services.account_pool.clone(),
                services.account_pseudonymizer.clone(),
            )
            .start(),
        );
        coordinator.push(
            "model_etag_refresh",
            SchedulerHandle::spawn(
                ModelRefreshTask::new(
                    services.models.clone(),
                    services.account_pool.clone(),
                    services.account_pseudonymizer.clone(),
                )
                .run_etag_refreshes(),
            ),
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
                stores.account_usage.clone(),
                services.codex.clone(),
                services.account_pool.clone(),
                services.account_pseudonymizer.clone(),
                config
                    .quota
                    .refresh_interval_minutes
                    .saturating_mul(60)
                    .max(1),
                30 * 60,
            )
            .with_cookie_store(stores.cookies.clone())
            .start(),
        );
        coordinator.push(
            "desktop_release_update",
            start_desktop_release_update_task(
                services.desktop_release.clone(),
                services.wire_profile.clone(),
                crate::upstream::openai::desktop_release::CODEX_DESKTOP_APPCAST_URL.to_string(),
            ),
        );
        if let Some(pool) = &services.websocket_pool {
            coordinator.push(
                "websocket_pool",
                SchedulerHandle::from_websocket_pool(pool.clone()),
            );
        }

        coordinator
    }

    /// 启动各领域的设置订阅任务。
    pub fn start_settings_subscriptions(services: &crate::bootstrap::services::Services) -> Self {
        let mut coordinator = TaskCoordinator::default();
        coordinator.push(
            "model_settings",
            SchedulerHandle::spawn(
                services
                    .models
                    .clone()
                    .subscribe_settings(services.settings.subscribe()),
            ),
        );
        coordinator.push(
            "account_pool_settings",
            SchedulerHandle::spawn(services.account_pool.clone().subscribe_settings(
                services.settings.subscribe(),
                services.account_pool_static.clone(),
            )),
        );
        coordinator.push(
            "refresh_policy_settings",
            SchedulerHandle::spawn(
                services
                    .refresh_policy
                    .clone()
                    .subscribe_settings(services.settings.subscribe()),
            ),
        );
        coordinator
    }

    /// 关闭所有后台任务。
    pub async fn shutdown(self) {
        tracing::info!("正在关闭后台任务");
        let shutdown_tasks = self.handles.into_iter().map(|(name, handle)| async move {
            handle.shutdown().await;
            tracing::info!(task = name, "后台任务已停止");
        });
        if tokio::time::timeout(
            COORDINATOR_SHUTDOWN_TIMEOUT,
            futures::future::join_all(shutdown_tasks),
        )
        .await
        .is_err()
        {
            tracing::warn!(
                timeout_secs = COORDINATOR_SHUTDOWN_TIMEOUT.as_secs(),
                "后台任务并行关闭超时"
            );
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
    WebSocketPool(Arc<crate::upstream::openai::transport::CodexWebSocketPool>),
}

impl SchedulerHandle {
    fn spawn(future: impl Future<Output = ()> + Send + 'static) -> Self {
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel(1);
        let handle = tokio::spawn(async move {
            tokio::select! {
                () = future => {}
                _ = shutdown_rx.recv() => {}
            }
        });
        Self::new(shutdown_tx, handle)
    }

    /// 使用关闭 channel 构造句柄。
    pub(crate) fn new(shutdown_tx: tokio::sync::mpsc::Sender<()>, handle: JoinHandle<()>) -> Self {
        Self::Channel {
            shutdown_tx,
            handle,
        }
    }

    /// 使用 WebSocket 连接池构造句柄。
    pub fn from_websocket_pool(
        pool: Arc<crate::upstream::openai::transport::CodexWebSocketPool>,
    ) -> Self {
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
const COORDINATOR_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(6);

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
