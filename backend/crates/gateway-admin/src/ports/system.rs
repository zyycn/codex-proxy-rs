//! Host 为系统管理用例提供的进程与操作系统能力。

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use crate::model::system::{
    SystemOperationAccepted, SystemUpdateDetail, SystemUpdateEvent, SystemUpdateStatus,
    SystemVersion,
};

/// Host 系统操作失败类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemOperationErrorKind {
    Invalid,
    Conflict,
    Upstream,
    Internal,
}

/// 不泄漏路径、命令行或发布凭据的系统操作错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("system operation failed: {message}")]
pub struct SystemOperationError {
    kind: SystemOperationErrorKind,
    message: String,
}

impl SystemOperationError {
    #[must_use]
    pub fn new(kind: SystemOperationErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> SystemOperationErrorKind {
        self.kind
    }

    /// 返回 Host 已完成脱敏的客户端安全消息。
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// 每个订阅者独占的系统事件流。
pub type SystemUpdateEventStream = Pin<Box<dyn Stream<Item = SystemUpdateEvent> + Send + 'static>>;

/// 版本、自更新、回滚和重启能力；实现唯一归 gateway-host。
#[async_trait]
pub trait SystemOperations: Send + Sync {
    async fn version(&self) -> Result<SystemVersion, SystemOperationError>;

    async fn update_detail(
        &self,
        refresh: bool,
    ) -> Result<SystemUpdateDetail, SystemOperationError>;

    fn update_events(&self) -> SystemUpdateEventStream;

    async fn perform_update(
        &self,
        target_version: Option<String>,
    ) -> Result<SystemOperationAccepted, SystemOperationError>;

    async fn update_status(&self) -> Result<SystemUpdateStatus, SystemOperationError>;

    async fn rollback(&self) -> Result<SystemOperationAccepted, SystemOperationError>;

    async fn restart(&self) -> Result<SystemOperationAccepted, SystemOperationError>;
}
