//! 后台任务协调器。

use std::path::PathBuf;

use codex_proxy_adapters::oauth::openai::default_openai_oauth_client;
use codex_proxy_core::auth::oauth::{OAuthConfig, RefreshPolicy};
use tokio::task::JoinHandle;

use crate::{config::RuntimeConfig, state::AppState};

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
pub struct BackgroundTaskCoordinator {
    handles: Vec<(&'static str, SchedulerHandle)>,
}

impl BackgroundTaskCoordinator {
    /// 注册一个后台任务句柄。
    pub fn push(&mut self, name: &'static str, handle: SchedulerHandle) {
        self.handles.push((name, handle));
    }

    /// 返回已注册任务名称。
    pub fn task_names(&self) -> Vec<&'static str> {
        self.handles.iter().map(|(name, _)| *name).collect()
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

/// 启动后台任务。
pub async fn start_background_tasks(state: &AppState) -> BackgroundTaskCoordinator {
    let mut coordinator = BackgroundTaskCoordinator::default();
    let stores = &state.services.background_tasks;

    coordinator.push(
        "cookie_cleanup",
        CookieCleanupTask::new(stores.cookies.clone()).start(),
    );
    coordinator.push(
        "session_cleanup",
        SessionCleanupTask::new(
            stores.admin_sessions.clone(),
            state.config.admin.session_cleanup_interval_secs,
        )
        .start(),
    );
    coordinator.push(
        "session_affinity_cleanup",
        SessionAffinityCleanupTask::new(
            stores.session_affinity.clone(),
            state.config.admin.session_cleanup_interval_secs,
        )
        .start(),
    );
    coordinator.push(
        "model_refresh",
        ModelRefreshTask::new(
            state.services.models.clone(),
            state.services.accounts.clone(),
        )
        .with_installation_id(state.services.installation_id.clone())
        .start(),
    );
    if state.config.auth.refresh_enabled {
        coordinator.push(
            "token_refresh",
            TokenRefreshTask::new(
                stores.accounts.clone(),
                RefreshPolicy {
                    refresh_margin_seconds: state.config.auth.refresh_margin_seconds,
                    refresh_concurrency: state.config.auth.refresh_concurrency,
                },
                default_openai_oauth_client(oauth_config(&state.config)),
            )
            .with_refresh_lease_store(stores.refresh_leases.clone())
            .start(),
        );
    }
    coordinator.push(
        "quota_refresh",
        QuotaRefreshTask::with_intervals(
            stores.accounts.clone(),
            state.services.codex.clone(),
            state
                .config
                .quota
                .refresh_interval_minutes
                .saturating_mul(60)
                .max(1),
            30 * 60,
        )
        .with_installation_id(state.services.installation_id.clone())
        .start(),
    );
    coordinator.push(
        "fingerprint_update",
        start_fingerprint_update_task(
            Some(stores.fingerprints.clone()),
            CODEX_DESKTOP_APPCAST_URL.to_string(),
            PathBuf::from(DEFAULT_EXTRACTED_FINGERPRINT_PATH),
            state.services.fingerprint.app_version.clone(),
            state.services.fingerprint.build_number.clone(),
        ),
    );

    coordinator
}

fn oauth_config(config: &RuntimeConfig) -> OAuthConfig {
    OAuthConfig {
        client_id: config.auth.oauth_client_id.clone(),
        auth_endpoint: config.auth.oauth_auth_endpoint.clone(),
        device_code_endpoint: oauth_device_code_endpoint(&config.auth.oauth_token_endpoint),
        token_endpoint: config.auth.oauth_token_endpoint.clone(),
    }
}

fn oauth_device_code_endpoint(token_endpoint: &str) -> String {
    token_endpoint
        .strip_suffix("/token")
        .map(|prefix| format!("{prefix}/device/code"))
        .unwrap_or_else(|| "https://auth.openai.com/oauth/device/code".to_string())
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
    pub fn new(shutdown_tx: tokio::sync::mpsc::Sender<()>) -> Self {
        Self::Channel(shutdown_tx)
    }

    /// 使用 `JoinHandle` 构造句柄。
    pub fn from_join_handle(handle: JoinHandle<()>) -> Self {
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
