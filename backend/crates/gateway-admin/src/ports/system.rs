//! Host 为系统管理用例提供的进程与操作系统能力

use std::{pin::Pin, sync::Arc};

use async_trait::async_trait;
use futures::Stream;

use crate::model::Revision;
use crate::model::system::{
    SystemOperationAccepted, SystemUpdateChannel, SystemUpdateDetail, SystemUpdateEvent,
    SystemUpdateStatus, SystemVersion,
};

/// Host 已校验并解包的同一更新候选；字节只含发行清单，不含运行配置或凭据
#[derive(Clone)]
pub struct SystemUpdateCandidate {
    pub target_version: String,
    pub release_manifest: Arc<[u8]>,
}

impl std::fmt::Debug for SystemUpdateCandidate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SystemUpdateCandidate")
            .field("target_version", &self.target_version)
            .field("release_manifest_bytes", &self.release_manifest.len())
            .finish()
    }
}

/// Host 系统操作失败类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemOperationErrorKind {
    Invalid,
    Conflict,
    Upstream,
    Internal,
}

/// 不泄漏路径、命令行或发布凭据的系统操作错误
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

    /// 返回 Host 已完成脱敏的客户端安全消息
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// 每个订阅者独占的系统事件流
pub type SystemUpdateEventStream = Pin<Box<dyn Stream<Item = SystemUpdateEvent> + Send + 'static>>;

/// Admin 对启用实例和制品作兼容决策；Host 只负责候选下载、校验与文件交换
#[async_trait]
pub trait SystemUpdatePreflight: Send + Sync {
    async fn validate(
        &self,
        candidate: SystemUpdateCandidate,
    ) -> Result<Revision, SystemOperationError>;

    /// 回滚保持严格预检，不能把旧目标当成具有当前重启确认能力的版本
    async fn validate_rollback(
        &self,
        candidate: SystemUpdateCandidate,
    ) -> Result<Revision, SystemOperationError> {
        self.validate(candidate).await
    }

    /// 在文件交换临界点复核全局配置 CAS，不能用较早快照掩盖实例变更
    async fn confirm_revision(&self, expected: Revision) -> Result<(), SystemOperationError>;
}

/// 重启临界区内复查候选，确认后停用不兼容插件，再允许进程退出
#[async_trait]
pub trait SystemRestartPreflight: Send + Sync {
    async fn prepare(
        &self,
        candidate: Option<SystemUpdateCandidate>,
    ) -> Result<(), SystemOperationError>;
}

/// 版本、自更新、回滚和重启能力；实现唯一归 gateway-host
#[async_trait]
pub trait SystemOperations: Send + Sync {
    async fn version(&self) -> Result<SystemVersion, SystemOperationError>;

    async fn update_detail(
        &self,
        refresh: bool,
        channel: Option<SystemUpdateChannel>,
    ) -> Result<SystemUpdateDetail, SystemOperationError>;

    fn update_events(&self) -> SystemUpdateEventStream;

    async fn perform_update(
        &self,
        target_version: Option<String>,
        channel: Option<SystemUpdateChannel>,
        preflight: Arc<dyn SystemUpdatePreflight>,
    ) -> Result<SystemOperationAccepted, SystemOperationError>;

    async fn update_status(&self) -> Result<SystemUpdateStatus, SystemOperationError>;

    async fn rollback(
        &self,
        preflight: Arc<dyn SystemUpdatePreflight>,
    ) -> Result<SystemOperationAccepted, SystemOperationError>;

    /// 返回下一次启动实际使用的发行；源码运行没有封存发行清单
    async fn restart_candidate(
        &self,
    ) -> Result<Option<SystemUpdateCandidate>, SystemOperationError> {
        Ok(None)
    }

    async fn restart(
        &self,
        preflight: Arc<dyn SystemRestartPreflight>,
    ) -> Result<SystemOperationAccepted, SystemOperationError>;
}
