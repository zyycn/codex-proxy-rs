//! 配额刷新后台任务接线器。

use std::{collections::HashMap, sync::Arc};

use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::{
    accounts::{
        cookies::PgCookieStore,
        pool::RuntimeAccountPoolService,
        quota::{QuotaRefreshSummary, RuntimeQuotaRefreshService},
        store::PgAccountStore,
    },
    upstream::openai::transport::CodexBackendClient,
};

use super::{
    coordinator::SchedulerHandle,
    periodic::{spawn_periodic_task, PeriodicTaskConfig, PeriodicTaskRunner},
};

/// 主动配额刷新后台任务。
pub struct QuotaRefreshTask {
    service: RuntimeQuotaRefreshService,
    interval_secs: u64,
    last_refreshed: HashMap<String, Instant>,
}

impl QuotaRefreshTask {
    /// 使用自定义间隔构造配额刷新后台任务。
    pub fn with_intervals(
        store: PgAccountStore,
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
            last_refreshed: HashMap::new(),
        }
    }

    /// 设置 Codex installation id。
    pub fn with_installation_id(mut self, installation_id: Option<String>) -> Self {
        self.service = self.service.with_installation_id(installation_id);
        self
    }

    /// 设置 usage 请求可复用的账号 Cookie 存储。
    pub fn with_cookie_store(mut self, cookie_store: PgCookieStore) -> Self {
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
        let config = PeriodicTaskConfig::new(
            self.interval_secs,
            "quota 刷新任务已启动",
            "quota 刷新任务已关闭",
        );
        spawn_periodic_task(self, config)
    }
}

impl PeriodicTaskRunner for QuotaRefreshTask {
    fn tick(&mut self) -> super::periodic::TaskFuture<'_, ()> {
        Box::pin(async move {
            match self
                .service
                .refresh_locked_accounts(&mut self.last_refreshed)
                .await
            {
                Ok(summary) if summary.refreshed > 0 => {
                    log_refreshed_summary(summary);
                }
                Ok(summary) => {
                    debug!(
                        candidates = summary.candidates,
                        skipped_cooldown = summary.skipped_cooldown,
                        skipped_recent = summary.skipped_recent,
                        failed = summary.failed,
                        "没有需要刷新的 quota 锁定或待验证账号"
                    );
                }
                Err(error) => {
                    warn!(error = %error, "quota 刷新任务失败");
                }
            }
        })
    }
}

fn log_refreshed_summary(summary: QuotaRefreshSummary) {
    info!(
        refreshed = summary.refreshed,
        failed = summary.failed,
        "quota 刷新任务完成"
    );
}
