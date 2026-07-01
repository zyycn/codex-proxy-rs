//! 配额刷新后台任务接线器。

use std::{collections::HashMap, sync::Arc, time::Duration};

use tokio::time::interval;
use tracing::{debug, info, warn};

use crate::{
    upstream::accounts::{
        cookies::SqliteCookieStore,
        pool::RuntimeAccountPoolService,
        quota::{QuotaRefreshSummary, RuntimeQuotaRefreshService},
        store::SqliteAccountStore,
    },
    upstream::transport::CodexBackendClient,
};

use super::coordinator::SchedulerHandle;

/// 主动配额刷新后台任务。
pub struct QuotaRefreshTask {
    service: RuntimeQuotaRefreshService,
    interval_secs: u64,
}

impl QuotaRefreshTask {
    /// 使用自定义间隔构造配额刷新后台任务。
    pub fn with_intervals(
        store: SqliteAccountStore,
        codex: Arc<CodexBackendClient>,
        interval_secs: u64,
        min_refresh_interval_secs: u64,
    ) -> Self {
        Self {
            service: RuntimeQuotaRefreshService::with_min_refresh_interval_secs(
                store,
                codex,
                min_refresh_interval_secs,
            ),
            interval_secs,
        }
    }

    /// 设置 Codex installation id。
    pub fn with_installation_id(mut self, installation_id: Option<String>) -> Self {
        self.service = self.service.with_installation_id(installation_id);
        self
    }

    /// 设置 usage 请求可复用的账号 Cookie 存储。
    pub fn with_cookie_store(mut self, cookie_store: SqliteCookieStore) -> Self {
        self.service = self.service.with_cookie_store(cookie_store);
        self
    }

    /// 设置运行时账号池，用于刷新后同步内存调度状态。
    pub fn with_account_pool(mut self, account_pool: Arc<RuntimeAccountPoolService>) -> Self {
        self.service = self.service.with_account_pool(account_pool);
        self
    }

    /// 启动后台刷新任务。
    pub fn start(self) -> SchedulerHandle {
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel(1);

        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(self.interval_secs));
            let mut last_refreshed = HashMap::new();
            info!(interval_secs = self.interval_secs, "quota 刷新任务已启动");

            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        match self.service.refresh_locked_accounts(&mut last_refreshed).await {
                            Ok(summary) if summary.refreshed > 0 => {
                                log_refreshed_summary(summary);
                            }
                            Ok(summary) => {
                                debug!(
                                    candidates = summary.candidates,
                                    skipped_recent = summary.skipped_recent,
                                    failed = summary.failed,
                                    "没有需要刷新的 quota 锁定或待验证账号"
                                );
                            }
                            Err(error) => {
                                warn!(error = %error, "quota 刷新任务失败");
                            }
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        info!("quota 刷新任务已关闭");
                        break;
                    }
                }
            }
        });

        SchedulerHandle::new(shutdown_tx)
    }
}

fn log_refreshed_summary(summary: QuotaRefreshSummary) {
    info!(
        refreshed = summary.refreshed,
        failed = summary.failed,
        "quota 刷新任务完成"
    );
}
