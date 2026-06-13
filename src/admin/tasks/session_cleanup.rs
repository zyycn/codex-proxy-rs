use tokio::time::{interval, Duration};
use tracing::{error, info};

use crate::runtime::tasks::types::SchedulerHandle;

/// 会话清理调度器 - 定期删除过期的管理员会话
pub struct SessionCleanupScheduler {
    db: sqlx::SqlitePool,
    interval_secs: u64,
}

impl SessionCleanupScheduler {
    pub fn new(db: sqlx::SqlitePool, interval_secs: u64) -> Self {
        Self { db, interval_secs }
    }

    /// 启动会话清理调度器
    pub fn start(self) -> SchedulerHandle {
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel(1);

        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(self.interval_secs));
            info!(interval_secs = self.interval_secs, "会话清理调度器已启动");

            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        match self.cleanup_expired_sessions().await {
                            Ok(count) if count > 0 => {
                                info!(count, "已清理过期会话");
                            }
                            Ok(_) => {
                                // 没有过期会话，不输出日志
                            }
                            Err(e) => {
                                error!(error = %e, "清理过期会话失败");
                            }
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        info!("会话清理调度器已关闭");
                        break;
                    }
                }
            }
        });

        SchedulerHandle::new(shutdown_tx)
    }

    async fn cleanup_expired_sessions(&self) -> Result<u64, sqlx::Error> {
        let now = chrono::Utc::now().to_rfc3339();
        let result = sqlx::query("DELETE FROM admin_sessions WHERE expires_at < ?")
            .bind(&now)
            .execute(&self.db)
            .await?;
        Ok(result.rows_affected())
    }
}
