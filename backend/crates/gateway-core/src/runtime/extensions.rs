//! 可发布能力集合的中立身份与保活合同；不解释包格式、进程或管理配置。

use std::{fmt, sync::Arc};

use futures::future::BoxFuture;

use crate::routing::ConfigRevision;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExtensionSetId(String);

impl ExtensionSetId {
    pub fn new(value: String) -> Result<Self, ExtensionPreparationError> {
        if value.is_empty()
            || value.len() > 128
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
        {
            return Err(ExtensionPreparationError);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 发布视图及在途请求持有引用；准备器只能用非拥有索引查找代次。
pub trait ExtensionSetLease: Send + Sync {
    fn is_ready(&self) -> bool;

    /// 局部故障已有请求级处理计划时，集合仍可提供服务。
    fn can_serve(&self) -> bool {
        self.is_ready()
    }
}

#[derive(Clone)]
pub struct ExtensionSetReference {
    id: ExtensionSetId,
    lease: Arc<dyn ExtensionSetLease>,
}

impl ExtensionSetReference {
    #[must_use]
    pub fn new(id: ExtensionSetId, lease: Arc<dyn ExtensionSetLease>) -> Self {
        Self { id, lease }
    }
    #[must_use]
    pub const fn id(&self) -> &ExtensionSetId {
        &self.id
    }
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.lease.is_ready()
    }
    #[must_use]
    pub fn can_serve(&self) -> bool {
        self.lease.can_serve()
    }
}

impl fmt::Debug for ExtensionSetReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExtensionSetReference")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("extension preparation is unavailable")]
pub struct ExtensionPreparationError;

/// 准备阶段读取与请求快照相同的持久 revision；不得自行发布当前代次。
pub trait ExtensionPreparationPort: Send + Sync {
    fn prepare(
        &self,
        revision: ConfigRevision,
    ) -> BoxFuture<'_, Result<ExtensionSetReference, ExtensionPreparationError>>;
}
