//! Cookie 清理任务。

use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::time::interval;
use tracing::{debug, info, warn};

use crate::accounts::cookies::{SqliteCookieStore, SqliteCookieStoreError};

use super::coordinator::SchedulerHandle;

const DEFAULT_INTERVAL_SECS: u64 = 5 * 60;

/// Cookie 清理任务。
pub struct CookieCleanupTask {
    store: SqliteCookieStore,
    interval_secs: u64,
}

impl CookieCleanupTask {
    /// 构造默认 Cookie 清理任务。
    pub fn new(store: SqliteCookieStore) -> Self {
        Self {
            store,
            interval_secs: DEFAULT_INTERVAL_SECS,
        }
    }

    /// 使用自定义间隔构造 Cookie 清理任务。
    pub fn with_interval_secs(store: SqliteCookieStore, interval_secs: u64) -> Self {
        Self {
            store,
            interval_secs,
        }
    }

    /// 启动后台清理任务。
    pub fn start(self) -> SchedulerHandle {
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel(1);

        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(self.interval_secs));
            info!(interval_secs = self.interval_secs, "Cookie 清理任务已启动");

            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        match self.cleanup_once().await {
                            Ok(deleted) if deleted > 0 => {
                                info!(deleted, "已清理过期 Cookie");
                            }
                            Ok(_) => {
                                debug!("没有需要清理的过期 Cookie");
                            }
                            Err(error) => {
                                warn!(error = %error, "清理过期 Cookie 失败");
                            }
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        info!("Cookie 清理任务已关闭");
                        break;
                    }
                }
            }
        });

        SchedulerHandle::new(shutdown_tx)
    }

    /// 执行一次清理。
    pub async fn cleanup_once(&self) -> Result<u64, SqliteCookieStoreError> {
        self.cleanup_once_at(Utc::now()).await
    }

    /// 在指定时间点执行一次清理。
    pub async fn cleanup_once_at(&self, now: DateTime<Utc>) -> Result<u64, SqliteCookieStoreError> {
        self.store.cleanup_expired(now).await
    }
}
