//! 会话清理任务。

use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::time::interval;
use tracing::{debug, info, warn};

use crate::access::admin_session::SqliteAdminSessionStore;

use super::coordinator::SchedulerHandle;

/// 管理员会话清理任务。
pub struct SessionCleanupTask {
    store: SqliteAdminSessionStore,
    interval_secs: u64,
}

impl SessionCleanupTask {
    /// 构造管理员会话清理任务。
    pub fn new(store: SqliteAdminSessionStore, interval_secs: u64) -> Self {
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
                "管理员会话清理任务已启动"
            );

            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        match self.cleanup_once().await {
                            Ok(deleted) if deleted > 0 => {
                                info!(deleted, "已清理过期管理员会话");
                            }
                            Ok(_) => {
                                debug!("没有需要清理的过期管理员会话");
                            }
                            Err(error) => {
                                warn!(error = %error, "清理过期管理员会话失败");
                            }
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        info!("管理员会话清理任务已关闭");
                        break;
                    }
                }
            }
        });

        SchedulerHandle::new(shutdown_tx)
    }

    /// 执行一次清理。
    pub async fn cleanup_once(&self) -> Result<u64, sqlx::Error> {
        self.cleanup_once_at(Utc::now()).await
    }

    /// 在指定时间点执行一次清理。
    pub async fn cleanup_once_at(&self, now: DateTime<Utc>) -> Result<u64, sqlx::Error> {
        self.store.cleanup_expired_sessions(now).await
    }
}
