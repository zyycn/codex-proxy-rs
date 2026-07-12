//! 通用过期数据清理任务骨架。

use std::{fmt::Display, future::Future};

use chrono::{DateTime, Utc};
use tracing::{debug, info, warn};

use super::{
    coordinator::SchedulerHandle,
    periodic::{PeriodicTaskConfig, PeriodicTaskRunner, spawn_periodic_task},
};

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
        let config = PeriodicTaskConfig::new(
            self.interval_secs,
            self.messages.started,
            self.messages.stopped,
        );
        spawn_periodic_task(self, config)
    }

    pub(crate) async fn cleanup_once(&self) -> Result<u64, S::Error> {
        self.cleanup_once_at(Utc::now()).await
    }

    pub(crate) async fn cleanup_once_at(&self, now: DateTime<Utc>) -> Result<u64, S::Error> {
        self.store.delete_expired_at(now).await
    }
}

impl<S> PeriodicTaskRunner for CleanupTask<S>
where
    S: ExpiredCleanupStore,
{
    fn tick(&mut self) -> super::periodic::TaskFuture<'_, ()> {
        Box::pin(async move {
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
        })
    }
}
