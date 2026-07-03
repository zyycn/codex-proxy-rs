//! 会话清理任务。

use std::future::Future;

use chrono::{DateTime, Utc};

use crate::admin::auth::service::SqliteAdminSessionStore;

use super::{
    cleanup::{CleanupTask, CleanupTaskMessages, ExpiredCleanupStore},
    coordinator::SchedulerHandle,
};

const MESSAGES: CleanupTaskMessages = CleanupTaskMessages {
    started: "管理员会话清理任务已启动",
    deleted: "已清理过期管理员会话",
    empty: "没有需要清理的过期管理员会话",
    failed: "清理过期管理员会话失败",
    stopped: "管理员会话清理任务已关闭",
};

/// 管理员会话清理任务。
pub struct SessionCleanupTask {
    task: CleanupTask<SqliteAdminSessionStore>,
}

impl SessionCleanupTask {
    /// 构造管理员会话清理任务。
    pub fn new(store: SqliteAdminSessionStore, interval_secs: u64) -> Self {
        Self {
            task: CleanupTask::new(store, interval_secs, MESSAGES),
        }
    }

    /// 启动后台清理任务。
    pub fn start(self) -> SchedulerHandle {
        self.task.start()
    }

    /// 执行一次清理。
    pub async fn cleanup_once(&self) -> Result<u64, sqlx::Error> {
        self.task.cleanup_once().await
    }

    /// 在指定时间点执行一次清理。
    pub async fn cleanup_once_at(&self, now: DateTime<Utc>) -> Result<u64, sqlx::Error> {
        self.task.cleanup_once_at(now).await
    }
}

impl ExpiredCleanupStore for SqliteAdminSessionStore {
    type Error = sqlx::Error;

    fn delete_expired_at(
        &self,
        now: DateTime<Utc>,
    ) -> impl Future<Output = Result<u64, Self::Error>> + Send + '_ {
        self.cleanup_expired_sessions(now)
    }
}
