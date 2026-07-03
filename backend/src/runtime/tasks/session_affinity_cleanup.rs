//! 会话亲和性清理任务。

use std::future::Future;

use chrono::{DateTime, Utc};

use crate::proxy::dispatch::session_affinity::{
    SqliteSessionAffinityStore, SqliteSessionAffinityStoreError,
};

use super::{
    cleanup::{CleanupTask, CleanupTaskMessages, ExpiredCleanupStore},
    coordinator::SchedulerHandle,
};

const MESSAGES: CleanupTaskMessages = CleanupTaskMessages {
    started: "会话亲和性清理任务已启动",
    deleted: "已清理过期会话亲和性",
    empty: "没有需要清理的过期会话亲和性",
    failed: "清理过期会话亲和性失败",
    stopped: "会话亲和性清理任务已关闭",
};

/// 会话亲和性清理任务。
pub struct SessionAffinityCleanupTask {
    task: CleanupTask<SqliteSessionAffinityStore>,
}

impl SessionAffinityCleanupTask {
    /// 构造会话亲和性清理任务。
    pub fn new(store: SqliteSessionAffinityStore, interval_secs: u64) -> Self {
        Self {
            task: CleanupTask::new(store, interval_secs, MESSAGES),
        }
    }

    /// 启动后台清理任务。
    pub fn start(self) -> SchedulerHandle {
        self.task.start()
    }

    /// 执行一次清理。
    pub async fn cleanup_once(&self) -> Result<u64, SqliteSessionAffinityStoreError> {
        self.task.cleanup_once().await
    }

    /// 在指定时间点执行一次清理。
    pub async fn cleanup_once_at(
        &self,
        now: DateTime<Utc>,
    ) -> Result<u64, SqliteSessionAffinityStoreError> {
        self.task.cleanup_once_at(now).await
    }
}

impl ExpiredCleanupStore for SqliteSessionAffinityStore {
    type Error = SqliteSessionAffinityStoreError;

    fn delete_expired_at(
        &self,
        now: DateTime<Utc>,
    ) -> impl Future<Output = Result<u64, Self::Error>> + Send + '_ {
        self.delete_expired(now)
    }
}
