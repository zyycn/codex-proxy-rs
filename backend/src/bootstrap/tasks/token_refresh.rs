//! 令牌刷新后台任务接线器。

use tracing::{debug, info, warn};

use crate::accounts::{
    refresh::{RedisRefreshLeaseStore, RuntimeRefreshPolicy, RuntimeTokenRefreshService},
    store::PgAccountStore,
};
use crate::upstream::openai::token_client::TokenRefresher;

use super::{
    coordinator::SchedulerHandle,
    periodic::{spawn_periodic_task, PeriodicTaskConfig, PeriodicTaskRunner},
};

const DEFAULT_INTERVAL_SECS: u64 = 60;

/// token refresh 后台任务。
pub struct TokenRefreshTask<C>
where
    C: TokenRefresher,
{
    service: RuntimeTokenRefreshService<C>,
    interval_secs: u64,
}

impl<C> TokenRefreshTask<C>
where
    C: TokenRefresher,
{
    /// 构造默认后台任务。
    pub fn new(store: PgAccountStore, policy: RuntimeRefreshPolicy, client: C) -> Self {
        Self {
            service: RuntimeTokenRefreshService::new(store, policy, client),
            interval_secs: DEFAULT_INTERVAL_SECS,
        }
    }

    /// 使用已创建的运行时刷新服务构造后台任务。
    pub fn from_service(service: RuntimeTokenRefreshService<C>) -> Self {
        Self {
            service,
            interval_secs: DEFAULT_INTERVAL_SECS,
        }
    }

    /// 使用刷新租约存储保护账号刷新。
    pub fn with_refresh_lease_store(mut self, refresh_leases: RedisRefreshLeaseStore) -> Self {
        self.service = self.service.with_refresh_lease_store(refresh_leases);
        self
    }

    /// 启动后台任务。
    pub fn start(self) -> SchedulerHandle {
        let config = PeriodicTaskConfig::new(
            self.interval_secs,
            "token 刷新任务已启动",
            "token 刷新任务已关闭",
        );
        spawn_periodic_task(self, config)
    }
}

impl<C> PeriodicTaskRunner for TokenRefreshTask<C>
where
    C: TokenRefresher,
{
    fn tick(&mut self) -> super::periodic::TaskFuture<'_, ()> {
        Box::pin(async move {
            match self.service.schedule_account_timers_once().await {
                Ok(summary) if summary.changed() > 0 => {
                    info!(
                        scheduled = summary.scheduled,
                        immediate = summary.immediate,
                        replaced = summary.replaced,
                        "token 刷新定时器已调度"
                    );
                }
                Ok(summary) => {
                    debug!(
                        scanned = summary.scanned,
                        skipped = summary.skipped,
                        "没有需要调度刷新的 token"
                    );
                }
                Err(error) => {
                    warn!(error = %error, "token 刷新定时器调度失败");
                }
            }
        })
    }

    fn shutdown(&mut self) -> super::periodic::TaskFuture<'_, ()> {
        Box::pin(async move {
            self.service.clear_scheduled_timers().await;
        })
    }
}
