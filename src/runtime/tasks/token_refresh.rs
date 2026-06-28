//! 令牌刷新后台任务接线器。

use std::time::Duration;

use tokio::time::interval;
use tracing::{debug, info, warn};

use crate::upstream::accounts::{
    store::SqliteAccountStore,
    token_refresh::{RefreshLeaseStore, RefreshPolicy, RuntimeTokenRefreshService, TokenRefresher},
};

use super::coordinator::SchedulerHandle;

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
    pub fn new(store: SqliteAccountStore, policy: RefreshPolicy, client: C) -> Self {
        Self {
            service: RuntimeTokenRefreshService::new(store, policy, client),
            interval_secs: DEFAULT_INTERVAL_SECS,
        }
    }

    /// 使用刷新租约存储保护账号刷新。
    pub fn with_refresh_lease_store(mut self, refresh_leases: RefreshLeaseStore) -> Self {
        self.service = self.service.with_refresh_lease_store(refresh_leases);
        self
    }

    /// 启动后台任务。
    pub fn start(self) -> SchedulerHandle {
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel(1);

        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(self.interval_secs));
            info!(interval_secs = self.interval_secs, "token 刷新任务已启动");

            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        match self.service.schedule_account_timers_once().await {
                            Ok(summary) if summary.changed() > 0 => {
                                info!(
                                    scheduled = summary.scheduled,
                                    immediate = summary.immediate,
                                    recovery_scheduled = summary.recovery_scheduled,
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
                    }
                    _ = shutdown_rx.recv() => {
                        self.service.clear_scheduled_timers().await;
                        info!("token 刷新任务已关闭");
                        break;
                    }
                }
            }
        });

        SchedulerHandle::new(shutdown_tx)
    }
}
