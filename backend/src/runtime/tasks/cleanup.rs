//! 通用过期数据清理任务骨架。

use std::{fmt::Display, future::Future, time::Duration};

use chrono::{DateTime, Utc};
use tokio::time::interval;
use tracing::{debug, info, warn};

use super::coordinator::SchedulerHandle;

/// 支持按时间清理过期记录的存储。
pub(crate) trait ExpiredCleanupStore: Send + Sync + 'static {
    type Error: Display + Send + Sync + 'static;

    fn delete_expired_at(
        &self,
        now: DateTime<Utc>,
    ) -> impl Future<Output = Result<u64, Self::Error>> + Send + '_;
}

#[derive(Clone, Copy)]
pub(crate) struct CleanupTaskMessages {
    pub started: &'static str,
    pub deleted: &'static str,
    pub empty: &'static str,
    pub failed: &'static str,
    pub stopped: &'static str,
}

/// 周期性过期数据清理任务。
pub(crate) struct CleanupTask<S> {
    store: S,
    interval_secs: u64,
    messages: CleanupTaskMessages,
}

impl<S> CleanupTask<S>
where
    S: ExpiredCleanupStore,
{
    pub(crate) fn new(store: S, interval_secs: u64, messages: CleanupTaskMessages) -> Self {
        Self {
            store,
            interval_secs,
            messages,
        }
    }

    pub(crate) fn start(self) -> SchedulerHandle {
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel(1);

        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(self.interval_secs));
            info!(
                interval_secs = self.interval_secs,
                "{}", self.messages.started
            );

            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        match self.cleanup_once().await {
                            Ok(deleted) if deleted > 0 => {
                                info!(deleted, "{}", self.messages.deleted);
                            }
                            Ok(_) => {
                                debug!("{}", self.messages.empty);
                            }
                            Err(error) => {
                                warn!(error = %error, "{}", self.messages.failed);
                            }
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        info!("{}", self.messages.stopped);
                        break;
                    }
                }
            }
        });

        SchedulerHandle::new(shutdown_tx)
    }

    pub(crate) async fn cleanup_once(&self) -> Result<u64, S::Error> {
        self.cleanup_once_at(Utc::now()).await
    }

    pub(crate) async fn cleanup_once_at(&self, now: DateTime<Utc>) -> Result<u64, S::Error> {
        self.store.delete_expired_at(now).await
    }
}
