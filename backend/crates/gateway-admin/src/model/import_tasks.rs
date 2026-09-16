//! 进程内账号导入任务；只保存执行所需输入与可公开的结果。

use chrono::{DateTime, Utc};
use gateway_core::{account::ProviderAccountId, routing::ProviderKind};
use uuid::Uuid;

use super::{MutationContext, provider_credentials::ImportCredentials};

pub const MAX_IMPORT_TASK_ITEMS: usize = 200;

#[derive(Debug)]
pub struct ImportTaskInput {
    pub provider: ProviderKind,
    pub command: ImportCredentials,
}

#[derive(Debug)]
pub struct SubmitImportTask {
    pub submission_id: Uuid,
    /// API 对完整输入计算摘要；用于区分丢失响应后的重试与修改后的新提交。
    pub fingerprint: [u8; 32],
    pub context: MutationContext,
    pub items: Vec<ImportTaskInput>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportItemStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Unknown,
    Skipped,
}

impl ImportItemStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
            Self::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ImportTaskItem {
    pub index: usize,
    pub provider: ProviderKind,
    pub status: ImportItemStatus,
    pub account_ids: Vec<ProviderAccountId>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ImportTaskCounts {
    pub pending: usize,
    pub running: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub unknown: usize,
    pub skipped: usize,
    pub imported_accounts: usize,
}

#[derive(Debug, Clone)]
pub struct ImportTaskSummary {
    pub task_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub stop_requested: bool,
    pub total: usize,
    pub counts: ImportTaskCounts,
}

#[derive(Debug, Clone)]
pub struct ImportTaskDetail {
    pub summary: ImportTaskSummary,
    pub items: Vec<ImportTaskItem>,
}
