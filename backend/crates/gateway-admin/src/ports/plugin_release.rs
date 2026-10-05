//! 官方插件发行清单与制品文件的读取端口

use std::sync::Arc;

use async_trait::async_trait;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfficialPluginReleaseReadErrorKind {
    Invalid,
    NotFound,
    Unavailable,
}

/// 文件系统错误不携带部署路径，避免把宿主布局传播到管理域和日志之外
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("official plugin release files are unavailable")]
pub struct OfficialPluginReleaseReadError {
    kind: OfficialPluginReleaseReadErrorKind,
}

impl OfficialPluginReleaseReadError {
    #[must_use]
    pub const fn new(kind: OfficialPluginReleaseReadErrorKind) -> Self {
        Self { kind }
    }

    #[must_use]
    pub const fn kind(self) -> OfficialPluginReleaseReadErrorKind {
        self.kind
    }
}

/// Host 只暴露与宿主发行物同信任边界的只读文件，不解释官方身份或插件包
#[async_trait]
pub trait OfficialPluginReleaseFiles: Send + Sync {
    async fn manifest(&self) -> Result<Option<Arc<[u8]>>, OfficialPluginReleaseReadError>;

    async fn artifact(&self, file_name: &str) -> Result<Arc<[u8]>, OfficialPluginReleaseReadError>;
}
