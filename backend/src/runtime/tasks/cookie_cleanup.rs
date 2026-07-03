//! Cookie 清理任务。

use std::future::Future;

use chrono::{DateTime, Utc};

use crate::upstream::accounts::cookies::{SqliteCookieStore, SqliteCookieStoreError};

use super::{
    cleanup::{CleanupTask, CleanupTaskMessages, ExpiredCleanupStore},
    coordinator::SchedulerHandle,
};

const DEFAULT_INTERVAL_SECS: u64 = 5 * 60;
const MESSAGES: CleanupTaskMessages = CleanupTaskMessages {
    started: "Cookie 清理任务已启动",
    deleted: "已清理过期 Cookie",
    empty: "没有需要清理的过期 Cookie",
    failed: "清理过期 Cookie 失败",
    stopped: "Cookie 清理任务已关闭",
};

/// Cookie 清理任务。
pub struct CookieCleanupTask {
    task: CleanupTask<SqliteCookieStore>,
}

impl CookieCleanupTask {
    /// 构造默认 Cookie 清理任务。
    pub fn new(store: SqliteCookieStore) -> Self {
        Self {
            task: CleanupTask::new(store, DEFAULT_INTERVAL_SECS, MESSAGES),
        }
    }

    /// 启动后台清理任务。
    pub fn start(self) -> SchedulerHandle {
        self.task.start()
    }

    /// 执行一次清理。
    pub async fn cleanup_once(&self) -> Result<u64, SqliteCookieStoreError> {
        self.task.cleanup_once().await
    }

    /// 在指定时间点执行一次清理。
    pub async fn cleanup_once_at(&self, now: DateTime<Utc>) -> Result<u64, SqliteCookieStoreError> {
        self.task.cleanup_once_at(now).await
    }
}

impl ExpiredCleanupStore for SqliteCookieStore {
    type Error = SqliteCookieStoreError;

    fn delete_expired_at(
        &self,
        now: DateTime<Utc>,
    ) -> impl Future<Output = Result<u64, Self::Error>> + Send + '_ {
        self.cleanup_expired(now)
    }
}
