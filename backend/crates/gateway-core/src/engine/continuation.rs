//! Provider 原生 previous-response 的调用方隔离、账号绑定与复用约束。
//!
//! Core 不解释 Provider transcript；同一客户端连接需要的可携带状态由
//! [`ProviderSessionState`](crate::operation::ProviderSessionState) 不透明承载。

use std::fmt;

use futures::future::BoxFuture;

use crate::engine::credential::ProviderAccountId;
use crate::operation::ProviderSessionState;
use crate::policy::ClientApiKeyId;
use crate::routing::ProviderKind;

/// 客户端或 Provider 传递的 opaque response handle。
///
/// Codex 定义这个值的语义和格式。网关只将其作为亲和查找键或同账号上游
/// continuation 的载体，不得按私有长度、字符集或前缀规则拒绝、归一化或改写。
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PreviousResponseId(String);

impl PreviousResponseId {
    /// 按原样创建 opaque response handle。
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PreviousResponseId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PreviousResponseId(<redacted>)")
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
/// 该值同时绑定调用方 API Key、Provider 与账号，阻止 native handle 和不透明
/// Provider state 被跨租户或跨上游目标复用。
#[derive(Clone, PartialEq)]
pub struct NativeContinuationPin {
    /// 客户端提交、用于查找可丢失会话亲和记录的 response ID。
    previous_response_id: PreviousResponseId,
    /// Provider 原生 response handle。
    upstream_response_id: PreviousResponseId,
    client_api_key_id: ClientApiKeyId,
    provider: ProviderKind,
    account: ProviderAccountId,
    scope: NativeContinuationScope,
    session_state: Option<ProviderSessionState>,
}

impl NativeContinuationPin {
    #[must_use]
    pub const fn new(
        previous_response_id: PreviousResponseId,
        upstream_response_id: PreviousResponseId,
        client_api_key_id: ClientApiKeyId,
        provider: ProviderKind,
        account: ProviderAccountId,
    ) -> Self {
        Self {
            previous_response_id,
            upstream_response_id,
            client_api_key_id,
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
    pub const fn upstream_response_id(&self) -> &PreviousResponseId {
        &self.upstream_response_id
    }

    #[must_use]
    pub const fn client_api_key_id(&self) -> &ClientApiKeyId {
        &self.client_api_key_id
    }

    #[must_use]
    pub fn matches_client(&self, client_api_key_id: &ClientApiKeyId) -> bool {
        self.client_api_key_id == *client_api_key_id
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
            .field("client_api_key_id", &self.client_api_key_id)
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

/// Native continuation lookup 的稳定失败分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NativeContinuationStoreErrorKind {
    /// Redis 等可丢失协调存储暂不可用；调用方可以按外部 continuation fail-open。
    Unavailable,
    /// 存储中的记录无法还原为可信 pin；不得将原始 handle 继续透传上游。
    InvalidData,
    /// 已存在的记录属于另一个客户端 API Key；必须 fail closed。
    OwnershipMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("native continuation store failed: {kind:?}: {detail}")]
pub struct NativeContinuationStoreError {
    kind: NativeContinuationStoreErrorKind,
    detail: String,
}

impl NativeContinuationStoreError {
    /// 构造可 fail-open 的协调存储不可用错误；`detail` 不应包含 response handle。
    #[must_use]
    pub fn unavailable(detail: impl Into<String>) -> Self {
        Self {
            kind: NativeContinuationStoreErrorKind::Unavailable,
            detail: detail.into(),
        }
    }

    /// 构造必须 fail closed 的无效记录错误；`detail` 不应包含 response handle。
    #[must_use]
    pub fn invalid_data(detail: impl Into<String>) -> Self {
        Self {
            kind: NativeContinuationStoreErrorKind::InvalidData,
            detail: detail.into(),
        }
    }

    /// 构造不泄露记录归属细节的跨调用方错误。
    #[must_use]
    pub fn ownership_mismatch() -> Self {
        Self {
            kind: NativeContinuationStoreErrorKind::OwnershipMismatch,
            detail: "continuation belongs to a different client API key".to_owned(),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> NativeContinuationStoreErrorKind {
        self.kind
    }

    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

/// 可丢失的 previous-response 亲和存储端口。
///
/// 亲和记录按调用方 API Key 隔离；Redis 不可用或超时必须由调用方 fail-open，
/// 但命中其他 Key 的记录必须 fail closed。
pub trait NativeContinuationPort: Send + Sync {
    fn resolve<'a>(
        &'a self,
        client_api_key_id: &'a ClientApiKeyId,
        previous_response_id: &'a PreviousResponseId,
    ) -> BoxFuture<'a, Result<Option<NativeContinuationPin>, NativeContinuationStoreError>>;

    fn record<'a>(
        &'a self,
        pin: NativeContinuationPin,
    ) -> BoxFuture<'a, Result<(), NativeContinuationStoreError>>;
}
