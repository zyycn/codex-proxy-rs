//! 快照编译与客户端读取需要的 Provider 目录合同，不暴露执行或账号选择能力。

use std::collections::BTreeMap;

use futures::future::BoxFuture;

use crate::identity::ProviderKind;
use crate::operation::RawJsonPayload;

use super::{
    ModelCapabilities, ModelPresentation, PublicModelId, PublicModelProfile, UpstreamModelId,
};

/// Provider 原生客户端目录条目；Core 只解释模型标识，不解释协议正文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderModelDescriptor {
    pub model: UpstreamModelId,
    pub payload: RawJsonPayload,
}

/// 对外目录保留原生正文；只有没有原生目录的 Provider 才使用通用画像适配。
#[derive(Debug, Clone)]
pub enum PublicModelDescriptor {
    Native {
        model: PublicModelId,
        payload: RawJsonPayload,
    },
    Adapted(PublicModelProfile),
}

/// Provider 实时目录编译后的单模型能力。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderModelCapabilities {
    upstream_model: UpstreamModelId,
    capabilities: ModelCapabilities,
    presentation: Option<ModelPresentation>,
}

impl ProviderModelCapabilities {
    #[must_use]
    pub const fn new(upstream_model: UpstreamModelId, capabilities: ModelCapabilities) -> Self {
        Self {
            upstream_model,
            capabilities,
            presentation: None,
        }
    }

    #[must_use]
    pub fn with_presentation(mut self, presentation: ModelPresentation) -> Self {
        self.presentation = Some(presentation);
        self
    }

    #[must_use]
    pub const fn upstream_model(&self) -> &UpstreamModelId {
        &self.upstream_model
    }

    #[must_use]
    pub const fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    #[must_use]
    pub const fn presentation(&self) -> Option<&ModelPresentation> {
        self.presentation.as_ref()
    }
}

/// Provider 实时目录成功发布后的进程内单调代次。
///
/// 代次只表达“目录内容已经变化”，不承载模型、ETag 或 Provider 私有数据。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderCatalogGeneration(u64);

impl ProviderCatalogGeneration {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// 目录未知，不能据此判定某个模型不存在；不携带执行错误或上游协议细节。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("provider model catalog is unavailable")]
pub struct ProviderCatalogUnavailable;

/// 快照编译与对账使用的对象安全目录端口。
pub trait ProviderCatalogPort: Send + Sync {
    /// 返回全部已注册 Provider 的目录代次；注册集合在初始化后保持不变。
    ///
    /// 即使某个目录暂时不可读，也必须保留它的 Provider 与最近成功发布的代次。
    fn catalog_generations(&self) -> BTreeMap<ProviderKind, ProviderCatalogGeneration>;

    /// 查询指定 Provider 的模型事实；成功的空列表表示已知无模型，失败表示未知。
    fn query_model_capabilities(
        &self,
        provider: &ProviderKind,
    ) -> BoxFuture<'_, Result<Vec<ProviderModelCapabilities>, ProviderCatalogUnavailable>>;
}
