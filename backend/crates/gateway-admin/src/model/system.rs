//! 版本、自更新、回滚与进程重启的 UTC 语义模型。

use chrono::{DateTime, Utc};

/// 当前运行版本及更新检查摘要。
///
/// `deployment_mode_label` 之类的本地化字符串由 API 根据原始枚举值生成。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemVersion {
    pub version: String,
    pub git_sha: String,
    pub build_time: String,
    pub deployment_mode: String,
    pub update_channel: String,
    pub latest_version: String,
    pub has_update: bool,
    pub update_cached: bool,
    pub update_warning: Option<String>,
}

/// 发布源提供的完整更新详情。
///
/// 部署模式与构建类型的展示标签归 API；其余字段均为 Host 已确认的原始事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemUpdateDetail {
    pub current_version: String,
    pub latest_version: String,
    pub has_update: bool,
    pub deployment_mode: String,
    pub build_type: String,
    pub release_url: Option<String>,
    pub notes: Option<String>,
    pub cached: bool,
    pub update_supported: bool,
    pub unsupported_reason: Option<String>,
    pub warning: Option<String>,
}

/// 系统操作类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemOperationKind {
    Update,
    Rollback,
    Restart,
}

/// 系统操作状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemOperationStatus {
    Idle,
    Running,
    Succeeded,
    Failed,
}

/// 持久化系统操作的完整状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemOperationState {
    pub operation_id: Option<String>,
    pub kind: Option<SystemOperationKind>,
    pub status: SystemOperationStatus,
    pub target_version: Option<String>,
    pub message: Option<String>,
    pub error: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// 最近一次系统操作及版本交换状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemUpdateStatus {
    pub previous_version: Option<String>,
    pub current_version: Option<String>,
    pub operation: SystemOperationState,
}

/// 自更新日志级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemUpdateEventLevel {
    Info,
    Warning,
    Success,
    Error,
}

/// 一条已脱敏的系统更新事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemUpdateEvent {
    pub id: String,
    pub operation_id: Option<String>,
    pub level: SystemUpdateEventLevel,
    pub step: Option<String>,
    pub message: String,
    pub terminal: bool,
    pub progress_percent: Option<u8>,
    pub occurred_at: DateTime<Utc>,
}

/// 已受理的异步系统操作；不同 wire shape 由枚举保持为合法状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemOperationAccepted {
    Update {
        operation_id: String,
        deployment_mode: String,
        message: String,
        need_restart: bool,
        target_version: String,
    },
    Rollback {
        operation_id: String,
        message: String,
        need_restart: bool,
    },
    Restart {
        operation_id: String,
        message: String,
    },
}

impl SystemOperationAccepted {
    #[must_use]
    pub const fn kind(&self) -> SystemOperationKind {
        match self {
            Self::Update { .. } => SystemOperationKind::Update,
            Self::Rollback { .. } => SystemOperationKind::Rollback,
            Self::Restart { .. } => SystemOperationKind::Restart,
        }
    }
}
