//! 网关核心使用的稳定错误分类。

use std::fmt;
use std::time::Duration;

use thiserror::Error;

use crate::engine::UpstreamSendState;

/// 应用层标识不满足约束。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IdentifierError {
    /// 标识为空。
    #[error("identifier must not be empty")]
    Empty,
    /// 标识超过核心允许的字节数。
    #[error("identifier exceeds {max_bytes} bytes")]
    TooLong {
        /// 最大字节数。
        max_bytes: usize,
    },
    /// 标识使用了保留的系统前缀。
    #[error("identifier uses the reserved system prefix")]
    ReservedPrefix,
    /// 标识包含控制字符。
    #[error("identifier contains control characters")]
    ControlCharacter,
    /// 标识缺少规定的语义前缀。
    #[error("identifier must start with `{expected}`")]
    MissingPrefix {
        /// 规定前缀。
        expected: &'static str,
    },
}

/// Operation 构造或校验失败。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OperationError {
    /// 必填文本为空。
    #[error("`{field}` must not be empty")]
    EmptyField {
        /// 字段名。
        field: &'static str,
    },
    /// 数量字段为零。
    #[error("`{field}` must be greater than zero")]
    ZeroValue {
        /// 字段名。
        field: &'static str,
    },
    /// JSON 字段必须是 object。
    #[error("`{field}` must be a JSON object")]
    JsonObjectRequired {
        /// 字段名。
        field: &'static str,
    },
    /// Provider 专属参数重复。
    #[error("provider options for `{provider}` already exist")]
    DuplicateProviderOptions {
        /// Provider 名称。
        provider: String,
    },
}

/// 路由快照或 Route Plan 不满足不变量。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RoutingError {
    /// 动态 Provider/model 标识无法构造。
    #[error("routing identifier is invalid")]
    InvalidIdentifier,
    /// 配置 revision 必须为正数。
    #[error("config revision must be greater than zero")]
    InvalidRevision,
    /// 快照中存在重复实体。
    #[error("duplicate {entity} `{id}`")]
    DuplicateEntity {
        /// 实体类型。
        entity: &'static str,
        /// 实体 ID。
        id: String,
    },
    /// 实体引用不存在。
    #[error("{entity} `{id}` was not found")]
    NotFound {
        /// 实体类型。
        entity: &'static str,
        /// 实体 ID。
        id: String,
    },
    /// 固定平台内没有可执行本次请求的 Provider instance。
    #[error("no provider instance can execute model `{model}`")]
    NoCapableProvider {
        /// 客户端提交的模型名称。
        model: String,
    },
}

/// 调用方策略不满足约束或拒绝请求。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PolicyError {
    /// 请求超过调用方策略。
    #[error("request was denied by caller policy: {reason}")]
    Denied {
        /// 稳定拒绝原因。
        reason: &'static str,
    },
}

/// 用量或价格估算不满足事实约束。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AccountingError {
    /// 十进制定点值格式无效或超过 `numeric(20, 10)`。
    #[error("decimal value must fit unsigned numeric(20, 10)")]
    InvalidDecimal,
    /// 货币代码无效。
    #[error("currency must be a three-letter uppercase ASCII code")]
    InvalidCurrency,
    /// 价格估算状态与金额字段不一致。
    #[error("cost estimate fields do not match status `{status}`")]
    InvalidCostEstimate {
        /// 估算状态。
        status: &'static str,
    },
}

/// 跨 Provider 稳定的上游失败分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProviderErrorKind {
    /// 请求语义错误。
    InvalidRequest,
    /// Provider 不支持所需能力。
    Unsupported,
    /// Credential 认证失败。
    Unauthorized,
    /// Credential 没有权限。
    PermissionDenied,
    /// Provider 限流。
    RateLimited,
    /// Credential 配额耗尽。
    QuotaExhausted,
    /// 请求超时。
    Timeout,
    /// 网络或 transport 失败。
    Transport,
    /// 上游协议不合法。
    Protocol,
    /// Provider 暂不可用。
    Unavailable,
    /// 请求被取消。
    Cancelled,
    /// 进程终止后由恢复流程收敛。
    ProcessTerminated,
}

impl ProviderErrorKind {
    /// 返回可持久化的稳定名称。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::Unsupported => "unsupported",
            Self::Unauthorized => "unauthorized",
            Self::PermissionDenied => "permission_denied",
            Self::RateLimited => "rate_limited",
            Self::QuotaExhausted => "quota_exhausted",
            Self::Timeout => "timeout",
            Self::Transport => "transport",
            Self::Protocol => "protocol",
            Self::Unavailable => "unavailable",
            Self::Cancelled => "cancelled",
            Self::ProcessTerminated => "process_terminated",
        }
    }
}

/// Adapter 已明确分类为非 bearer、可用于诊断的上游值。
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SafeUpstreamValue(String);

impl SafeUpstreamValue {
    const MAX_BYTES: usize = 256;

    /// 校验并创建诊断值。
    ///
    /// # Errors
    ///
    /// 空值、超长值或控制字符会返回 [`IdentifierError`]。
    pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = value.into();
        validate_text(&value, Self::MAX_BYTES, false, None)?;
        Ok(Self(value))
    }

    /// 返回已经由 adapter 安全分类的原值。
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SafeUpstreamValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SafeUpstreamValue(<redacted-from-Debug>)")
    }
}

/// 单次 Provider 调用的稳定错误。
///
/// 该类型不接收原始响应正文，也不会在 `Debug` 或 `Display` 中打印上游
/// code/request ID/response ID。Adapter 若捕获到可能含 secret 的上下文，只能调用
/// [`ProviderError::redact_sensitive_context`] 丢弃正文并留下脱敏标记。
pub struct ProviderError {
    kind: ProviderErrorKind,
    send_state: UpstreamSendState,
    upstream_status: Option<u16>,
    upstream_code: Option<SafeUpstreamValue>,
    upstream_request_id: Option<SafeUpstreamValue>,
    upstream_response_id: Option<SafeUpstreamValue>,
    retry_after: Option<Duration>,
    replay_safe: bool,
    sensitive_context_redacted: bool,
}

impl ProviderError {
    /// 创建 Provider 错误。
    #[must_use]
    pub const fn new(kind: ProviderErrorKind, send_state: UpstreamSendState) -> Self {
        Self {
            kind,
            send_state,
            upstream_status: None,
            upstream_code: None,
            upstream_request_id: None,
            upstream_response_id: None,
            retry_after: None,
            replay_safe: false,
            sensitive_context_redacted: false,
        }
    }

    /// 附加合法上游状态码。
    #[must_use]
    pub fn with_status(mut self, status: u16) -> Self {
        if (100..=599).contains(&status) {
            self.upstream_status = Some(status);
        }
        self
    }

    /// 附加 adapter 已分类为安全的上游错误 code。
    #[must_use]
    pub fn with_upstream_code(mut self, code: SafeUpstreamValue) -> Self {
        self.upstream_code = Some(code);
        self
    }

    /// 附加 adapter 已分类为非 bearer 的上游 request ID。
    #[must_use]
    pub fn with_upstream_request_id(mut self, request_id: SafeUpstreamValue) -> Self {
        self.upstream_request_id = Some(request_id);
        self
    }

    /// 附加 adapter 已分类为非 bearer 的上游 response ID。
    #[must_use]
    pub fn with_upstream_response_id(mut self, response_id: SafeUpstreamValue) -> Self {
        self.upstream_response_id = Some(response_id);
        self
    }

    /// 附加 Provider 建议的冷却时间。
    #[must_use]
    pub const fn with_retry_after(mut self, retry_after: Duration) -> Self {
        self.retry_after = Some(retry_after);
        self
    }

    /// 标记 Provider 已证明本次拒绝没有执行生成，可在下游提交前重放。
    #[must_use]
    pub const fn with_replay_safe(mut self) -> Self {
        self.replay_safe = true;
        self
    }

    /// 丢弃敏感正文，只记录“发生过脱敏”这一事实。
    #[must_use]
    pub fn redact_sensitive_context(mut self, _sensitive_context: impl AsRef<str>) -> Self {
        self.sensitive_context_redacted = true;
        self
    }

    /// 返回稳定错误分类。
    #[must_use]
    pub const fn kind(&self) -> ProviderErrorKind {
        self.kind
    }

    /// 返回本次 payload 的保守发送状态。
    #[must_use]
    pub const fn send_state(&self) -> UpstreamSendState {
        self.send_state
    }

    /// 返回合法上游状态码。
    #[must_use]
    pub const fn upstream_status(&self) -> Option<u16> {
        self.upstream_status
    }

    /// 返回安全分类的上游错误 code。
    #[must_use]
    pub fn upstream_code(&self) -> Option<&SafeUpstreamValue> {
        self.upstream_code.as_ref()
    }

    /// 返回安全分类的上游 request ID。
    #[must_use]
    pub fn upstream_request_id(&self) -> Option<&SafeUpstreamValue> {
        self.upstream_request_id.as_ref()
    }

    /// 返回安全分类的上游 response ID。
    #[must_use]
    pub fn upstream_response_id(&self) -> Option<&SafeUpstreamValue> {
        self.upstream_response_id.as_ref()
    }

    /// 返回 Provider 建议的冷却时间。
    #[must_use]
    pub const fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }

    /// 返回 Provider 是否已证明本次失败可安全重放。
    #[must_use]
    pub const fn replay_is_safe(&self) -> bool {
        self.replay_safe
    }

    /// 表示 adapter 是否丢弃过敏感错误正文。
    #[must_use]
    pub const fn sensitive_context_was_redacted(&self) -> bool {
        self.sensitive_context_redacted
    }
}

impl fmt::Debug for ProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderError")
            .field("kind", &self.kind)
            .field("send_state", &self.send_state)
            .field("upstream_status", &self.upstream_status)
            .field(
                "upstream_code",
                &self.upstream_code.as_ref().map(|_| "<classified-safe>"),
            )
            .field(
                "upstream_request_id",
                &self
                    .upstream_request_id
                    .as_ref()
                    .map(|_| "<classified-safe>"),
            )
            .field(
                "upstream_response_id",
                &self
                    .upstream_response_id
                    .as_ref()
                    .map(|_| "<classified-safe>"),
            )
            .field("retry_after", &self.retry_after)
            .field("replay_safe", &self.replay_safe)
            .field(
                "sensitive_context",
                &self.sensitive_context_redacted.then_some("<redacted>"),
            )
            .finish()
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "provider call failed: {}", self.kind.as_str())
    }
}

impl std::error::Error for ProviderError {}

/// 对客户端协议稳定的网关错误分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum GatewayErrorKind {
    /// 请求无效。
    InvalidRequest,
    /// 请求能力不支持。
    Unsupported,
    /// 下游认证失败。
    Unauthorized,
    /// 调用方策略拒绝。
    PolicyDenied,
    /// 对外模型或 route 不存在。
    ModelNotFound,
    /// 当前没有可用 target。
    NoAvailableProvider,
    /// 上游限流。
    RateLimited,
    /// 上游暂不可用。
    UpstreamUnavailable,
    /// 请求超时。
    Timeout,
    /// 请求取消。
    Cancelled,
    /// 内部持久化或状态机失败。
    Internal,
}

impl GatewayErrorKind {
    /// 返回适合客户端协议映射的稳定 code。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::Unsupported => "unsupported",
            Self::Unauthorized => "unauthorized",
            Self::PolicyDenied => "policy_denied",
            Self::ModelNotFound => "model_not_found",
            Self::NoAvailableProvider => "no_available_provider",
            Self::RateLimited => "rate_limited",
            Self::UpstreamUnavailable => "upstream_unavailable",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::Internal => "internal_error",
        }
    }
}

/// 协议无关、可安全暴露的网关错误。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct GatewayError {
    kind: GatewayErrorKind,
    message: &'static str,
}

impl GatewayError {
    /// 使用静态安全消息创建错误。
    #[must_use]
    pub const fn new(kind: GatewayErrorKind, message: &'static str) -> Self {
        Self { kind, message }
    }

    /// 将 Provider 错误归一为客户端无关错误。
    #[must_use]
    pub fn from_provider(error: &ProviderError) -> Self {
        match error.kind() {
            ProviderErrorKind::InvalidRequest => {
                Self::new(GatewayErrorKind::InvalidRequest, "invalid upstream request")
            }
            ProviderErrorKind::Unsupported => Self::new(
                GatewayErrorKind::Unsupported,
                "requested capability is unsupported",
            ),
            ProviderErrorKind::Unauthorized | ProviderErrorKind::PermissionDenied => Self::new(
                GatewayErrorKind::UpstreamUnavailable,
                "upstream authentication resource is unavailable",
            ),
            ProviderErrorKind::RateLimited | ProviderErrorKind::QuotaExhausted => Self::new(
                GatewayErrorKind::RateLimited,
                "upstream capacity is temporarily unavailable",
            ),
            ProviderErrorKind::Timeout => {
                Self::new(GatewayErrorKind::Timeout, "upstream request timed out")
            }
            ProviderErrorKind::Cancelled => {
                Self::new(GatewayErrorKind::Cancelled, "request was cancelled")
            }
            ProviderErrorKind::Transport
            | ProviderErrorKind::Protocol
            | ProviderErrorKind::Unavailable
            | ProviderErrorKind::ProcessTerminated => Self::new(
                GatewayErrorKind::UpstreamUnavailable,
                "upstream service is unavailable",
            ),
        }
    }

    /// 返回稳定错误分类。
    #[must_use]
    pub const fn kind(&self) -> GatewayErrorKind {
        self.kind
    }

    /// 返回已经脱敏的静态消息。
    #[must_use]
    pub const fn safe_message(&self) -> &'static str {
        self.message
    }
}

/// Store adapter 的稳定错误分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StoreErrorKind {
    /// CAS 或 expected revision 冲突。
    Conflict,
    /// 存储暂不可用。
    Unavailable,
    /// 生命周期状态转换非法。
    InvalidState,
    /// 数据无法转换为核心事实。
    InvalidData,
}

/// Core port 返回的脱敏存储错误。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("execution store failed: {kind:?}")]
pub struct StoreError {
    kind: StoreErrorKind,
}

impl StoreError {
    /// 创建不携带数据库正文的 store 错误。
    #[must_use]
    pub const fn new(kind: StoreErrorKind) -> Self {
        Self { kind }
    }

    /// 返回稳定错误分类。
    #[must_use]
    pub const fn kind(&self) -> StoreErrorKind {
        self.kind
    }
}

pub(crate) fn validate_text(
    value: &str,
    max_bytes: usize,
    reject_reserved_prefix: bool,
    required_prefix: Option<&'static str>,
) -> Result<(), IdentifierError> {
    if value.is_empty() {
        return Err(IdentifierError::Empty);
    }
    if value.len() > max_bytes {
        return Err(IdentifierError::TooLong { max_bytes });
    }
    if value.chars().any(char::is_control) {
        return Err(IdentifierError::ControlCharacter);
    }
    if reject_reserved_prefix && value.starts_with("__") {
        return Err(IdentifierError::ReservedPrefix);
    }
    if let Some(prefix) = required_prefix
        && !value.starts_with(prefix)
    {
        return Err(IdentifierError::MissingPrefix { expected: prefix });
    }
    Ok(())
}
