//! Provider 原生 previous-response 的调用方隔离、账号绑定与复用约束。
//!
//! Core 不解释 Provider transcript；同一客户端连接需要的可携带状态由
//! [`ProviderSessionState`](crate::operation::ProviderSessionState) 不透明承载。

use std::fmt;

use futures::future::BoxFuture;

use crate::engine::credential::ProviderAccountId;
use crate::error::{IdentifierError, SafeUpstreamValue, validate_text};
use crate::policy::ClientApiKeyId;
use crate::routing::ProviderKind;

/// 客户端传入的 previous response ID。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PreviousResponseId(String);

impl PreviousResponseId {
    /// 校验并创建 previous response ID。
    ///
    /// # Errors
    ///
    /// ID 为空、过长或包含控制字符时返回错误。
    pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = value.into();
        validate_text(&value, 256, false, None)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Provider 原生 response handle 的续接范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeContinuationScope {
    /// 上游已持久化，可由同账号的新连接继续。
    Persisted,
    /// 只存在于完成上一轮的 WebSocket。
    ConnectionLocal,
}

/// 外层已解析并认证的 native previous-response pin。
///
/// 该值不代表数据库 transcript；它只阻止 native handle 被发送到错误的
/// Provider 或账号。
#[derive(Clone, PartialEq, Eq)]
pub struct NativeContinuationPin {
    /// 客户端提交、仅供 Store 在调用方隔离下查找的网关 response ID。
    previous_response_id: PreviousResponseId,
    /// Store 从已提交成功请求解析出的 Provider 原生 response handle。
    upstream_response_id: SafeUpstreamValue,
    provider: ProviderKind,
    account: ProviderAccountId,
    scope: NativeContinuationScope,
}

impl NativeContinuationPin {
    #[must_use]
    pub const fn new(
        previous_response_id: PreviousResponseId,
        upstream_response_id: SafeUpstreamValue,
        provider: ProviderKind,
        account: ProviderAccountId,
    ) -> Self {
        Self {
            previous_response_id,
            upstream_response_id,
            provider,
            account,
            scope: NativeContinuationScope::ConnectionLocal,
        }
    }

    /// 设置 Store 已确认的原生续接范围。
    #[must_use]
    pub const fn with_scope(mut self, scope: NativeContinuationScope) -> Self {
        self.scope = scope;
        self
    }

    #[must_use]
    pub const fn previous_response_id(&self) -> &PreviousResponseId {
        &self.previous_response_id
    }

    /// 返回只允许发送给已冻结 Provider 目标的原生上游 handle。
    #[must_use]
    pub const fn upstream_response_id(&self) -> &SafeUpstreamValue {
        &self.upstream_response_id
    }

    #[must_use]
    pub const fn provider(&self) -> &ProviderKind {
        &self.provider
    }

    #[must_use]
    pub const fn account(&self) -> &ProviderAccountId {
        &self.account
    }

    #[must_use]
    pub const fn scope(&self) -> NativeContinuationScope {
        self.scope
    }

    /// 校验本次 route/account 选择没有破坏 native pin。
    #[must_use]
    pub fn matches(&self, provider: &ProviderKind, account: &ProviderAccountId) -> bool {
        self.provider == *provider && self.account == *account
    }
}

impl fmt::Debug for NativeContinuationPin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeContinuationPin")
            .field("previous_response_id", &"<redacted>")
            .field("upstream_response_id", &"<redacted>")
            .field("provider", &self.provider)
            .field("account", &self.account)
            .field("scope", &self.scope)
            .finish()
    }
}

/// 一次请求最终采用的 previous-response 绑定方式。
///
/// 已命中网关历史的 handle 携带完整账号 pin；未命中历史的外部 handle 只保留
/// 客户端提交的 opaque ID，由目标 Provider 在首次且唯一一次 attempt 中解释。
#[derive(Clone, PartialEq, Eq)]
pub enum ContinuationBinding {
    Pinned(NativeContinuationPin),
    External(PreviousResponseId),
}

impl ContinuationBinding {
    #[must_use]
    pub const fn previous_response_id(&self) -> &PreviousResponseId {
        match self {
            Self::Pinned(pin) => pin.previous_response_id(),
            Self::External(previous_response_id) => previous_response_id,
        }
    }

    #[must_use]
    pub const fn pinned(&self) -> Option<&NativeContinuationPin> {
        match self {
            Self::Pinned(pin) => Some(pin),
            Self::External(_) => None,
        }
    }
}

impl fmt::Debug for ContinuationBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pinned(pin) => formatter.debug_tuple("Pinned").field(pin).finish(),
            Self::External(_) => formatter
                .debug_tuple("External")
                .field(&"<redacted>")
                .finish(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("native continuation store is unavailable")]
pub struct NativeContinuationStoreError;

/// 调用方隔离后的 previous-response 解析端口。
pub trait NativeContinuationPort: Send + Sync {
    fn resolve<'a>(
        &'a self,
        client_api_key_id: &'a ClientApiKeyId,
        previous_response_id: &'a PreviousResponseId,
    ) -> BoxFuture<'a, Result<Option<NativeContinuationPin>, NativeContinuationStoreError>>;
}
