//! 网关核心使用的稳定错误分类。

use std::fmt;
use std::time::Duration;

use bytes::Bytes;
use thiserror::Error;

use crate::engine::UpstreamSendState;
use crate::event::{ProviderEvent, ProviderResponseHeader};

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
    /// 标识不满足该领域值对象的固定格式。
    #[error("identifier has an invalid format")]
    InvalidFormat,
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
    /// Restricted scope 必须保留至少一个持久化分组 binding。
    #[error("account routing scope is invalid")]
    InvalidAccountScope,
    /// Key 的账号范围中当前没有任何可参与路由的账号。
    #[error("account routing scope is empty")]
    EmptyAccountScope,
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
    /// 固定平台内没有可执行本次请求的 Provider。
    #[error("no provider can execute model `{model}`")]
    NoCapableProvider {
        /// 客户端提交的模型名称。
        model: String,
    },
    /// 固定 Provider 的原生端点当前不可执行。
    #[error("provider endpoint `{provider}` is unavailable")]
    NoCapableProviderEndpoint {
        /// adapter 已绑定的 Provider。
        provider: String,
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
    /// 仍有符合条件的账号，但它们暂时没有可调度容量。
    AccountCapacityUnavailable,
    /// Provider 已确认当前请求无法选出可用账号。
    NoEligibleAccount,
    /// Provider 的账号存储、租约协调或本地凭据数据不可用。
    ProviderInfrastructureUnavailable,
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
            Self::AccountCapacityUnavailable => "account_capacity_unavailable",
            Self::NoEligibleAccount => "no_eligible_account",
            Self::ProviderInfrastructureUnavailable => "provider_infrastructure_unavailable",
            Self::Timeout => "timeout",
            Self::Transport => "transport",
            Self::Protocol => "protocol",
            Self::Unavailable => "unavailable",
            Self::Cancelled => "cancelled",
            Self::ProcessTerminated => "process_terminated",
        }
    }
}

/// Provider 对 previous-response 失败给出的稳定恢复语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContinuationFailure {
    /// 原生 continuation 正被同账号的另一请求占用。
    Busy,
    /// 原生 history 在当前连接或账号不可用，可由完整输入重放恢复。
    HistoryUnavailable,
}

/// Adapter 已明确分类为非 bearer 的不透明上游值。
///
/// Core 不解释或校验内容，只通过自定义 [`Debug`] 避免它意外进入日志。协议 adapter
/// 可在原客户端响应中读取原值。
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct OpaqueUpstreamValue(String);

impl OpaqueUpstreamValue {
    /// 原样保存上游值。
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// 返回已经由 adapter 安全分类的原值。
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for OpaqueUpstreamValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OpaqueUpstreamValue(<redacted-from-Debug>)")
    }
}

/// 已由 Provider 从结构化上游错误中提取、仅供原客户端协议展示的错误详情。
///
/// 它不是诊断日志或持久化错误信息：[`Debug`] 会隐藏全部值，调用方应只在
/// 原请求的协议响应中使用它。
#[derive(Clone, PartialEq, Eq)]
pub struct ClientVisibleUpstreamError {
    message: String,
    code: Option<OpaqueUpstreamValue>,
    error_type: Option<OpaqueUpstreamValue>,
}

impl ClientVisibleUpstreamError {
    /// 原样保存结构化上游 `message`、`code` 和 `type`。
    #[must_use]
    pub fn new(
        message: impl Into<String>,
        code: Option<String>,
        error_type: Option<String>,
    ) -> Self {
        Self {
            message: message.into(),
            code: code.map(OpaqueUpstreamValue::new),
            error_type: error_type.map(OpaqueUpstreamValue::new),
        }
    }

    /// 返回原上游的结构化 message。
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// 返回原上游的结构化 code。
    #[must_use]
    pub fn code(&self) -> Option<&str> {
        self.code.as_ref().map(OpaqueUpstreamValue::as_str)
    }

    /// 返回原上游的结构化 type。
    #[must_use]
    pub fn error_type(&self) -> Option<&str> {
        self.error_type.as_ref().map(OpaqueUpstreamValue::as_str)
    }
}

impl fmt::Debug for ClientVisibleUpstreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ClientVisibleUpstreamError(<redacted-from-Debug>)")
    }
}

/// 只供当前客户端请求使用的原始上游 HTTP 失败响应。
///
/// 该值不属于稳定诊断事实，不能进入日志或持久化。它刻意不实现 [`Clone`]；
/// [`ProviderError`] 的普通 clone 会丢弃它，只有最终失败的原对象才能把响应交给
/// 原客户端协议，或由认证管理端的账号连接测试显式复制到请求局部结果。
#[derive(PartialEq, Eq)]
pub struct ClientVisibleUpstreamResponse {
    status: u16,
    content_type: Option<Vec<u8>>,
    headers: Vec<ProviderResponseHeader>,
    body: Bytes,
}

impl ClientVisibleUpstreamResponse {
    /// 保存 transport 实际收到的状态码、Content-Type 原值和正文。
    #[must_use]
    pub fn new(status: u16, content_type: Option<Vec<u8>>, body: Bytes) -> Self {
        Self {
            status,
            content_type,
            headers: Vec::new(),
            body,
        }
    }

    /// 附加 Provider 已剔除账号绑定与 framing 信息的普通响应头。
    #[must_use]
    pub fn with_headers(mut self, headers: Vec<ProviderResponseHeader>) -> Self {
        self.headers = headers;
        self
    }

    #[must_use]
    pub const fn status(&self) -> u16 {
        self.status
    }

    #[must_use]
    pub fn content_type(&self) -> Option<&[u8]> {
        self.content_type.as_deref()
    }

    #[must_use]
    pub fn headers(&self) -> &[ProviderResponseHeader] {
        &self.headers
    }

    #[must_use]
    pub const fn body(&self) -> &Bytes {
        &self.body
    }
}

impl fmt::Debug for ClientVisibleUpstreamResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClientVisibleUpstreamResponse")
            .field("status", &self.status)
            .field(
                "content_type",
                &self.content_type.as_ref().map(|_| "<present>"),
            )
            .field("header_count", &self.headers.len())
            .field("body", &"<redacted>")
            .finish()
    }
}

/// 单次 Provider 调用的稳定错误。
///
/// 稳定诊断字段不接收原始响应正文，也不会在 `Debug` 或 `Display` 中打印上游
/// code/request ID/response ID。Adapter 若捕获到可能含 secret 的上下文，只能调用
/// [`ProviderError::redact_sensitive_context`] 丢弃正文并留下脱敏标记。
///
/// 唯一例外是 [`ProviderError::with_atomic_client_events`]：它只承载尚未交付客户端、
/// 必须与本错误一起完成重试判断的协议事件。Core 会在任何持久化或日志记录前取走
/// 这些事件，不能把它们当作诊断上下文使用。
pub struct ProviderError {
    kind: ProviderErrorKind,
    send_state: UpstreamSendState,
    upstream_status: Option<u16>,
    upstream_values: Option<Box<ProviderErrorUpstreamValues>>,
    retry_after: Option<Duration>,
    continuation_failure: Option<ContinuationFailure>,
    replay_safe: bool,
    pre_delivery_retry: bool,
    credential_recovery_required: bool,
    retry_same_account: bool,
    sensitive_context_redacted: bool,
    client_visible_upstream_error: Option<Box<ClientVisibleUpstreamError>>,
    client_visible_upstream_response: Option<Box<ClientVisibleUpstreamResponse>>,
    atomic_client_events: Option<Box<AtomicClientEvents>>,
}

#[derive(Clone, Default)]
struct ProviderErrorUpstreamValues {
    code: Option<OpaqueUpstreamValue>,
    request_id: Option<OpaqueUpstreamValue>,
}

#[derive(Clone)]
struct AtomicClientEvents(Vec<ProviderEvent>);

impl ProviderError {
    /// 创建 Provider 错误。
    #[must_use]
    pub const fn new(kind: ProviderErrorKind, send_state: UpstreamSendState) -> Self {
        Self {
            kind,
            send_state,
            upstream_status: None,
            upstream_values: None,
            retry_after: None,
            continuation_failure: None,
            replay_safe: false,
            pre_delivery_retry: false,
            credential_recovery_required: false,
            retry_same_account: false,
            sensitive_context_redacted: false,
            client_visible_upstream_error: None,
            client_visible_upstream_response: None,
            atomic_client_events: None,
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
    pub fn with_upstream_code(mut self, code: OpaqueUpstreamValue) -> Self {
        self.upstream_values_mut().code = Some(code);
        self
    }

    /// 附加 adapter 已分类为非 bearer 的上游 request ID。
    #[must_use]
    pub fn with_upstream_request_id(mut self, request_id: OpaqueUpstreamValue) -> Self {
        self.upstream_values_mut().request_id = Some(request_id);
        self
    }

    fn upstream_values_mut(&mut self) -> &mut ProviderErrorUpstreamValues {
        self.upstream_values
            .get_or_insert_with(|| Box::new(ProviderErrorUpstreamValues::default()))
    }

    /// 附加 Provider 建议的冷却时间。
    #[must_use]
    pub const fn with_retry_after(mut self, retry_after: Duration) -> Self {
        self.retry_after = Some(retry_after);
        self
    }

    /// 附加 previous-response 的恢复语义。
    #[must_use]
    pub const fn with_continuation_failure(mut self, failure: ContinuationFailure) -> Self {
        self.continuation_failure = Some(failure);
        self
    }

    /// 标记 Provider 已证明本次拒绝没有执行生成，可在下游提交前重放。
    #[must_use]
    pub const fn with_replay_safe(mut self) -> Self {
        self.replay_safe = true;
        self
    }

    /// 允许 Core 仅在客户端尚未收到任何事件时执行一次受预算约束的换号恢复。
    ///
    /// 该标记不证明上游未执行，也不等价于 [`Self::with_replay_safe`]；它只表达
    /// Provider 选择了“客户端无感恢复优先”的传输策略。Core 在 continuation、
    /// 指定账号或下游已经进入提交状态时必须忽略它。
    #[must_use]
    pub const fn with_pre_delivery_retry(mut self) -> Self {
        self.pre_delivery_retry = true;
        self
    }

    /// 要求 Core 在账号凭据已恢复后仅对原账号重放一次。
    #[must_use]
    pub const fn with_same_account_retry(mut self) -> Self {
        self.retry_same_account = true;
        self
    }

    /// 标记上游明确拒绝应先触发 OAuth 恢复，而不是直接废弃账号。
    #[must_use]
    pub const fn with_credential_recovery(mut self) -> Self {
        self.credential_recovery_required = true;
        self
    }

    /// 附加只用于原客户端协议响应的结构化上游错误。
    #[must_use]
    pub fn with_client_visible_upstream_error(mut self, error: ClientVisibleUpstreamError) -> Self {
        self.client_visible_upstream_error = Some(Box::new(error));
        self
    }

    /// 附加仅可返回给当前请求方的原始上游 HTTP 失败响应。
    #[must_use]
    pub fn with_client_visible_upstream_response(
        mut self,
        response: ClientVisibleUpstreamResponse,
    ) -> Self {
        self.client_visible_upstream_response = Some(Box::new(response));
        self
    }

    /// 附加必须先由 Core 判断重试、再决定丢弃或交付的客户端协议事件。
    ///
    /// Provider 只能放入当前 attempt 尚未交付的事件；调用方应设置明确的有界缓冲，
    /// 并保证失败事件本身位于这批事件的末尾。
    #[must_use]
    pub fn with_atomic_client_events(mut self, events: Vec<ProviderEvent>) -> Self {
        self.atomic_client_events =
            (!events.is_empty()).then(|| Box::new(AtomicClientEvents(events)));
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
    pub fn upstream_code(&self) -> Option<&OpaqueUpstreamValue> {
        self.upstream_values
            .as_deref()
            .and_then(|values| values.code.as_ref())
    }

    /// 返回安全分类的上游 request ID。
    #[must_use]
    pub fn upstream_request_id(&self) -> Option<&OpaqueUpstreamValue> {
        self.upstream_values
            .as_deref()
            .and_then(|values| values.request_id.as_ref())
    }

    /// 返回 Provider 建议的冷却时间。
    #[must_use]
    pub const fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }

    /// 返回 previous-response 的恢复语义。
    #[must_use]
    pub const fn continuation_failure(&self) -> Option<ContinuationFailure> {
        self.continuation_failure
    }

    /// 返回 Provider 是否已证明本次失败可安全重放。
    #[must_use]
    pub const fn replay_is_safe(&self) -> bool {
        self.replay_safe
    }

    /// 返回 Provider 是否允许在首个客户端事件前换号恢复传输失败。
    #[must_use]
    pub const fn allows_pre_delivery_retry(&self) -> bool {
        self.pre_delivery_retry
    }

    /// 返回 Provider 是否已完成凭据恢复并要求原账号重放。
    #[must_use]
    pub const fn retries_same_account(&self) -> bool {
        self.retry_same_account
    }

    #[must_use]
    pub const fn requires_credential_recovery(&self) -> bool {
        self.credential_recovery_required
    }

    /// 表示 adapter 是否丢弃过敏感错误正文。
    #[must_use]
    pub const fn sensitive_context_was_redacted(&self) -> bool {
        self.sensitive_context_redacted
    }

    /// 返回仅供原客户端协议展示的结构化上游错误。
    #[must_use]
    pub fn client_visible_upstream_error(&self) -> Option<&ClientVisibleUpstreamError> {
        self.client_visible_upstream_error.as_deref()
    }

    /// 返回只供当前协议响应使用的原始上游 HTTP 失败响应。
    #[must_use]
    pub fn client_visible_upstream_response(&self) -> Option<&ClientVisibleUpstreamResponse> {
        self.client_visible_upstream_response.as_deref()
    }

    /// 取走只供本次重试/提交决策使用的原子客户端事件。
    ///
    /// Core 必须在克隆错误、记录中间失败或构造终态前调用本方法，避免原始 wire
    /// 进入诊断或持久化对象。
    #[must_use]
    pub fn take_atomic_client_events(&mut self) -> Vec<ProviderEvent> {
        self.atomic_client_events
            .take()
            .map_or_else(Vec::new, |events| events.0)
    }

    /// 返回是否携带尚未提交的原子客户端事件；不暴露事件内容。
    #[must_use]
    pub fn has_atomic_client_events(&self) -> bool {
        self.atomic_client_events.is_some()
    }
}

impl Clone for ProviderError {
    fn clone(&self) -> Self {
        Self {
            kind: self.kind,
            send_state: self.send_state,
            upstream_status: self.upstream_status,
            upstream_values: self.upstream_values.clone(),
            retry_after: self.retry_after,
            continuation_failure: self.continuation_failure,
            replay_safe: self.replay_safe,
            pre_delivery_retry: self.pre_delivery_retry,
            credential_recovery_required: self.credential_recovery_required,
            retry_same_account: self.retry_same_account,
            sensitive_context_redacted: self.sensitive_context_redacted,
            client_visible_upstream_error: self.client_visible_upstream_error.clone(),
            client_visible_upstream_response: None,
            atomic_client_events: None,
        }
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
                &self.upstream_code().map(|_| "<classified-safe>"),
            )
            .field(
                "upstream_request_id",
                &self.upstream_request_id().map(|_| "<classified-safe>"),
            )
            .field("retry_after", &self.retry_after)
            .field("continuation_failure", &self.continuation_failure)
            .field("replay_safe", &self.replay_safe)
            .field("pre_delivery_retry", &self.pre_delivery_retry)
            .field(
                "credential_recovery_required",
                &self.credential_recovery_required,
            )
            .field("retry_same_account", &self.retry_same_account)
            .field(
                "sensitive_context",
                &self.sensitive_context_redacted.then_some("<redacted>"),
            )
            .field(
                "client_visible_upstream_error",
                &self
                    .client_visible_upstream_error
                    .as_ref()
                    .map(|_| "<present>"),
            )
            .field(
                "client_visible_upstream_response",
                &self
                    .client_visible_upstream_response
                    .as_ref()
                    .map(|_| "<present>"),
            )
            .field(
                "atomic_client_events",
                &self
                    .atomic_client_events
                    .as_ref()
                    .map_or(0, |events| events.0.len()),
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
    /// 符合条件的上游账号暂时没有调度容量。
    AccountCapacityUnavailable,
    /// Provider 的本地账号基础设施不可用。
    ProviderInfrastructureUnavailable,
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
            Self::AccountCapacityUnavailable => "account_capacity_unavailable",
            Self::ProviderInfrastructureUnavailable => "provider_infrastructure_unavailable",
            Self::RateLimited => "rate_limited",
            Self::UpstreamUnavailable => "upstream_unavailable",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::Internal => "internal_error",
        }
    }
}

/// 协议无关、可安全暴露的网关错误。
#[derive(Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct GatewayError {
    kind: GatewayErrorKind,
    message: &'static str,
    client_visible_upstream_error: Option<ClientVisibleUpstreamError>,
}

impl GatewayError {
    /// 使用静态安全消息创建错误。
    #[must_use]
    pub const fn new(kind: GatewayErrorKind, message: &'static str) -> Self {
        Self {
            kind,
            message,
            client_visible_upstream_error: None,
        }
    }

    /// 将 Provider 错误归一为客户端无关错误。
    #[must_use]
    pub fn from_provider(error: &ProviderError) -> Self {
        let gateway = match error.kind() {
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
            ProviderErrorKind::AccountCapacityUnavailable => Self::new(
                GatewayErrorKind::AccountCapacityUnavailable,
                "all eligible upstream accounts are temporarily busy",
            ),
            ProviderErrorKind::NoEligibleAccount => Self::new(
                GatewayErrorKind::NoAvailableProvider,
                "no upstream provider is currently available for this request",
            ),
            ProviderErrorKind::ProviderInfrastructureUnavailable => Self::new(
                GatewayErrorKind::ProviderInfrastructureUnavailable,
                "provider account infrastructure is temporarily unavailable",
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
        };
        match error.client_visible_upstream_error() {
            Some(upstream) => gateway.with_client_visible_upstream_error(upstream.clone()),
            None => gateway,
        }
    }

    /// 附加只供请求方协议展示的结构化上游错误。
    #[must_use]
    pub fn with_client_visible_upstream_error(mut self, error: ClientVisibleUpstreamError) -> Self {
        self.client_visible_upstream_error = Some(error);
        self
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

    /// 返回优先用于原客户端协议响应的 message；持久化和日志必须继续使用
    /// [`Self::safe_message`]。
    #[must_use]
    pub fn client_message(&self) -> &str {
        self.client_visible_upstream_error
            .as_ref()
            .map_or(self.message, ClientVisibleUpstreamError::message)
    }

    /// 返回原上游结构化 error type；没有时调用方应回退稳定网关 type。
    #[must_use]
    pub fn client_error_type(&self) -> Option<&str> {
        self.client_visible_upstream_error
            .as_ref()
            .and_then(ClientVisibleUpstreamError::error_type)
    }

    /// 返回原上游结构化 error code；没有时调用方应回退稳定网关 code。
    #[must_use]
    pub fn client_error_code(&self) -> Option<&str> {
        self.client_visible_upstream_error
            .as_ref()
            .and_then(ClientVisibleUpstreamError::code)
    }
}

impl fmt::Debug for GatewayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GatewayError")
            .field("kind", &self.kind)
            .field("message", &self.message)
            .field(
                "client_visible_upstream_error",
                &self
                    .client_visible_upstream_error
                    .as_ref()
                    .map(|_| "<present>"),
            )
            .finish()
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
