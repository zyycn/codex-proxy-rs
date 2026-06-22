//! 会话亲和性清理任务。

use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::time::interval;
use tracing::{debug, info, warn};

use crate::gateway::dispatch::session_affinity::{
    SqliteSessionAffinityStore, SqliteSessionAffinityStoreError,
};

use super::coordinator::SchedulerHandle;

/// 会话亲和性清理任务。
pub struct SessionAffinityCleanupTask {
    store: SqliteSessionAffinityStore,
    interval_secs: u64,
}

impl SessionAffinityCleanupTask {
    /// 构造会话亲和性清理任务。
    pub fn new(store: SqliteSessionAffinityStore, interval_secs: u64) -> Self {
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
            info!(
                interval_secs = self.interval_secs,
                "会话亲和性清理任务已启动"
            );

            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        match self.cleanup_once().await {
                            Ok(deleted) if deleted > 0 => {
                                info!(deleted, "已清理过期会话亲和性");
                            }
                            Ok(_) => {
                                debug!("没有需要清理的过期会话亲和性");
                            }
                            Err(error) => {
                                warn!(error = %error, "清理过期会话亲和性失败");
                            }
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        info!("会话亲和性清理任务已关闭");
                        break;
                    }
                }
            }
        });

        SchedulerHandle::new(shutdown_tx)
    }

    /// 执行一次清理。
    pub async fn cleanup_once(&self) -> Result<u64, SqliteSessionAffinityStoreError> {
        self.cleanup_once_at(Utc::now()).await
    }

    /// 在指定时间点执行一次清理。
    pub async fn cleanup_once_at(
        &self,
        now: DateTime<Utc>,
    ) -> Result<u64, SqliteSessionAffinityStoreError> {
        self.store.delete_expired(now).await
    }
}
