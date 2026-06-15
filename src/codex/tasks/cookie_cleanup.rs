use std::time::Duration;

use chrono::Utc;
use tokio::time::interval;
use tracing::{debug, info, warn};

use crate::{
    codex::accounts::cookies::repository::{CookieRepository, CookieResult},
    runtime::tasks::types::SchedulerHandle,
};

const DEFAULT_INTERVAL_SECS: u64 = 5 * 60;

pub struct CookieCleanupScheduler {
    repository: CookieRepository,
    interval_secs: u64,
}

impl CookieCleanupScheduler {
    pub fn new(repository: CookieRepository) -> Self {
        Self {
            repository,
            interval_secs: DEFAULT_INTERVAL_SECS,
        }
    }

    pub fn start(self) -> SchedulerHandle {
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel(1);

        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(self.interval_secs));
            info!(
                interval_secs = self.interval_secs,
                "cookie 清理调度器已启动"
            );

            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        match self.cleanup_once().await {
                            Ok(deleted) if deleted > 0 => {
                                info!(deleted, "已清理过期 cookie");
                            }
                            Ok(_) => {
                                debug!("没有需要清理的过期 cookie");
                            }
                            Err(error) => {
                                warn!(error = %error, "清理过期 cookie 失败");
                            }
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        info!("cookie 清理调度器已关闭");
                        break;
                    }
                }
            }
        });

        SchedulerHandle::new(shutdown_tx)
    }

    pub async fn cleanup_once(&self) -> CookieResult<u64> {
        self.repository.cleanup_expired(Utc::now()).await
    }
}
