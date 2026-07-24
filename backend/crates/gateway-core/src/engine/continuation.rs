//! Provider 原生 previous-response 的调用方隔离、账号绑定与复用约束。
//!
//! Core 不解释 Provider transcript；同一客户端连接需要的可携带状态由
//! [`ProviderSessionState`](crate::operation::ProviderSessionState) 不透明承载。

use std::fmt;

use futures::future::BoxFuture;

use crate::engine::credential::ProviderAccountId;
use crate::error::{IdentifierError, SafeUpstreamValue, validate_text};
use crate::operation::ProviderSessionState;
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

/// 从可丢失会话亲和存储恢复的 native previous-response pin。
///
/// 该值不依赖调用方 API Key；它只阻止 native handle 与不透明 Provider state
/// 被发送到错误的 Provider 或账号。
#[derive(Clone, PartialEq)]
pub struct NativeContinuationPin {
    /// 客户端提交、用于查找可丢失会话亲和记录的 response ID。
    previous_response_id: PreviousResponseId,
    /// Provider 原生 response handle。
    upstream_response_id: SafeUpstreamValue,
    provider: ProviderKind,
    account: ProviderAccountId,
    scope: NativeContinuationScope,
    session_state: Option<ProviderSessionState>,
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
            session_state: None,
        }
    }

    /// 设置 Store 已确认的原生续接范围。
    #[must_use]
    pub const fn with_scope(mut self, scope: NativeContinuationScope) -> Self {
        self.scope = scope;
        self
    }

    /// 附着仅由对应 Provider 解释的不透明会话状态。
    #[must_use]
    pub fn with_session_state(mut self, state: ProviderSessionState) -> Self {
        self.session_state = Some(state);
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

    /// 返回与本 pin 同账号绑定的 Provider 私有会话状态。
    #[must_use]
    pub const fn session_state(&self) -> Option<&ProviderSessionState> {
        self.session_state.as_ref()
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
            .field(
                "session_state",
                &self.session_state.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// 一次请求最终采用的 previous-response 绑定方式。
///
/// 已命中网关历史的 handle 携带完整账号 pin；未命中历史的外部 handle 只保留
/// 客户端提交的 opaque ID，由目标 Provider 在首次且唯一一次 attempt 中解释。
#[derive(Clone, PartialEq)]
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

/// 可丢失的 previous-response 亲和存储端口。
///
/// 亲和记录不按调用方 API Key 隔离：与旧版一样，response ID 是会话级 opaque
/// handle；Redis 不可用或超时必须由调用方 fail-open。
pub trait NativeContinuationPort: Send + Sync {
    fn resolve<'a>(
        &'a self,
        previous_response_id: &'a PreviousResponseId,
    ) -> BoxFuture<'a, Result<Option<NativeContinuationPin>, NativeContinuationStoreError>>;

    fn record<'a>(
        &'a self,
        pin: NativeContinuationPin,
    ) -> BoxFuture<'a, Result<(), NativeContinuationStoreError>>;
}
