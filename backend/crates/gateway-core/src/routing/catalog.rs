//! 快照编译与客户端读取需要的 Provider 目录合同，不暴露执行或账号选择能力。

use std::collections::{BTreeMap, BTreeSet};

use futures::future::BoxFuture;

use crate::account::ProviderAccountId;
use crate::identity::ProviderKind;
use crate::operation::RawJsonPayload;

use super::{
    ModelCapabilities, ModelPresentation, PublicModelId, PublicModelProfile, UpstreamModelId,
};

/// 扩展集合持有的直接模型别名；执行与元数据仍归属内置 Provider。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContributedModelAlias {
    pub owner: String,
    pub id: PublicModelId,
    pub provider: ProviderKind,
    pub target: UpstreamModelId,
}

/// Provider 客户端目录条目；缺少原生协议正文时使用已编译的通用画像。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderModelDescriptor {
    pub model: UpstreamModelId,
    pub content: ProviderModelContent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderModelContent {
    Native(RawJsonPayload),
    Adapted(ModelPresentation),
}

/// 对外目录按条目保留原生正文或使用通用画像适配。
#[derive(Debug, Clone)]
pub enum PublicModelDescriptor {
    Native {
        model: PublicModelId,
        payload: RawJsonPayload,
    },
    Adapted(PublicModelProfile),
}

impl ProviderModelContent {
    pub(crate) fn for_public_model(&self, model: PublicModelId) -> PublicModelDescriptor {
        match self {
            Self::Native(payload) => PublicModelDescriptor::Native {
                model,
                payload: payload.clone(),
            },
            Self::Adapted(presentation) => {
                PublicModelDescriptor::Adapted(PublicModelProfile::new(model, presentation.clone()))
            }
        }
    }
}

/// Provider 实时目录编译后的单模型能力。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderModelCapabilities {
    upstream_model: UpstreamModelId,
    capabilities: ModelCapabilities,
    presentation: Option<ModelPresentation>,
    catalog_accounts: Option<BTreeSet<ProviderAccountId>>,
}

impl ProviderModelCapabilities {
    #[must_use]
    pub const fn new(upstream_model: UpstreamModelId, capabilities: ModelCapabilities) -> Self {
        Self {
            upstream_model,
            capabilities,
            presentation: None,
            catalog_accounts: None,
        }
    }

    #[must_use]
    pub fn with_presentation(mut self, presentation: ModelPresentation) -> Self {
        self.presentation = Some(presentation);
        self
    }

    /// 声明发现此模型的账号；仅用于目录可见性，不作为推理能力白名单。
    #[must_use]
    pub fn with_catalog_accounts(mut self, accounts: BTreeSet<ProviderAccountId>) -> Self {
        self.catalog_accounts = Some(accounts);
        self
    }

    /// 未提供账号来源的 Provider 沿用 Provider 级目录；空集合表示无可展示来源。
    #[must_use]
    pub const fn catalog_accounts(&self) -> Option<&BTreeSet<ProviderAccountId>> {
        self.catalog_accounts.as_ref()
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
    /// 目录是否完整到足以根据缺项拒绝请求；发现型目录交由上游验证模型名。
    fn model_catalog_is_exhaustive(&self, _provider: &ProviderKind) -> bool {
        true
    }

    /// 返回当前冻结视图内全部 Provider 的目录代次。
    ///
    /// 即使某个目录暂时不可读，也必须保留它的 Provider 与最近成功发布的代次。
    fn catalog_generations(&self) -> BTreeMap<ProviderKind, ProviderCatalogGeneration>;

    /// 查询模型发现事实；只有完整目录的空列表能证明无模型，失败表示未知。
    fn query_model_capabilities(
        &self,
        provider: &ProviderKind,
    ) -> BoxFuture<'_, Result<Vec<ProviderModelCapabilities>, ProviderCatalogUnavailable>>;
}
