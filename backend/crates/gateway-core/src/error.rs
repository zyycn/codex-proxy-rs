//! 网关核心使用的稳定错误分类。

use std::fmt;
use std::num::NonZeroU32;
use std::time::Duration;

use bytes::Bytes;
use thiserror::Error;

use crate::event::{ProviderEvent, ProviderResponseHeader};
pub use crate::upstream::OpaqueUpstreamValue;
use crate::upstream::UpstreamSendState;
pub use crate::validation::{
    IdentifierError, MeteringError, OperationError, PolicyError, RoutingError,
};

/// 跨 Provider 稳定的上游失败分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProviderErrorKind {
    /// 请求语义错误。
    InvalidRequest,
    /// previous-response 续接状态已不可用，需要客户端重建请求链。
    ContinuationRecoveryRequired,
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
            Self::ContinuationRecoveryRequired => "continuation_recovery_required",
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

/// Provider 对 previous-response 失败给出的显式恢复边界。
///
/// Core 不根据失败分类猜测 continuation 是否可迁移；只有 Provider 明确允许时，
/// 才能放宽 owner 范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContinuationRecoveryDisposition {
    /// 只能再次取得同账号、同一精确连接，不能改变 continuation scope。
    RetryExactConnection,
    /// 代理停止内部恢复，由客户端携带完整输入创建新链。
    ClientReplayRequired,
    /// Provider 已证明可以进入 owner 或其他合法目标的恢复流程。
    ProviderReplayAllowed,
}

/// Provider 在下游尚未提交事件时请求的隐藏恢复方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PreDeliveryRetry {
    /// 排除本次账号，按普通调度策略重新选号。
    AccountRotation,
    /// 固定本次账号，并按 Provider 给出的序号重试当前传输。
    SameAccountTransportRetry {
        /// Provider-owned 传输重试序号，从 1 开始。
        retry_index: NonZeroU32,
        /// 发起下一次传输尝试前的退避时长。
        delay: Duration,
    },
    /// 固定本次账号，并要求 Provider 使用备用传输。
    SameAccountTransportFallback,
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

/// Provider 已分类为可安全持久化和展示的诊断摘要。
///
/// Adapter 只能放入不含凭据、cookie、原始请求正文和 opaque response ID 的有界摘要；
/// 原始上游错误返回由 [`RawUpstreamError`] 单独承载。
#[derive(Clone, PartialEq, Eq)]
pub struct ProviderDiagnostic(String);

impl ProviderDiagnostic {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ProviderDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProviderDiagnostic(<redacted-from-Debug>)")
    }
}

/// 运维错误详情中按原样展示的上游错误返回。
///
/// 该值只接收响应方向的错误正文或 WebSocket close/error frame；不得放入请求
/// Authorization、Cookie 或客户端输入。它会被持久化并由管理端显式返回，但普通
/// `Debug`/`Display` 不输出正文，避免在非运维日志中重复扩散。
#[derive(Clone, PartialEq, Eq)]
pub struct RawUpstreamError(String);

impl RawUpstreamError {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for RawUpstreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RawUpstreamError(<present>)")
    }
}

/// Provider 上游物理连接结束时的低基数观测。
///
/// 连接标识必须是随机或单向派生值，不得携带凭据、原始会话 ID
/// 或请求正文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderConnectionObservation {
    connection_id: String,
    exit_reason: String,
    age_ms: u64,
    idle_ms: u64,
}

impl ProviderConnectionObservation {
    #[must_use]
    pub fn new(
        connection_id: impl Into<String>,
        exit_reason: impl Into<String>,
        age_ms: u64,
        idle_ms: u64,
    ) -> Self {
        Self {
            connection_id: connection_id.into(),
            exit_reason: exit_reason.into(),
            age_ms,
            idle_ms,
        }
    }

    #[must_use]
    pub fn connection_id(&self) -> &str {
        &self.connection_id
    }

    #[must_use]
    pub fn exit_reason(&self) -> &str {
        &self.exit_reason
    }

    #[must_use]
    pub const fn age_ms(&self) -> u64 {
        self.age_ms
    }

    #[must_use]
    pub const fn idle_ms(&self) -> u64 {
        self.idle_ms
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
    continuation_recovery_disposition: Option<ContinuationRecoveryDisposition>,
    failure_observation: Option<Box<ProviderErrorFailureObservation>>,
    replay_safe: bool,
    pre_delivery_retry: Option<PreDeliveryRetry>,
    credential_recovery_required: bool,
    retry_same_account: bool,
    sensitive_context_redacted: bool,
    diagnostic: Option<Box<ProviderDiagnostic>>,
    raw_upstream_error: Option<Box<RawUpstreamError>>,
    client_visible_upstream_error: Option<Box<ClientVisibleUpstreamError>>,
    client_visible_upstream_response: Option<Box<ClientVisibleUpstreamResponse>>,
    atomic_client_events: Option<Box<AtomicClientEvents>>,
}

#[derive(Clone, Default)]
struct ProviderErrorUpstreamValues {
    code: Option<OpaqueUpstreamValue>,
    request_id: Option<OpaqueUpstreamValue>,
}

#[derive(Clone, Default)]
struct ProviderErrorFailureObservation {
    continuation_unavailable_reason: Option<&'static str>,
    connection: Option<ProviderConnectionObservation>,
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
            continuation_recovery_disposition: None,
            failure_observation: None,
            replay_safe: false,
            pre_delivery_retry: None,
            credential_recovery_required: false,
            retry_same_account: false,
            sensitive_context_redacted: false,
            diagnostic: None,
            raw_upstream_error: None,
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

    /// 附加 Provider 明确选择的 previous-response 恢复边界。
    #[must_use]
    pub const fn with_continuation_recovery_disposition(
        mut self,
        disposition: ContinuationRecoveryDisposition,
    ) -> Self {
        self.continuation_recovery_disposition = Some(disposition);
        self
    }

    /// 附加 Provider 给出的低基数 continuation 不可用原因。
    #[must_use]
    pub fn with_continuation_unavailable_reason(mut self, reason: &'static str) -> Self {
        self.failure_observation_mut()
            .continuation_unavailable_reason = Some(reason);
        self
    }

    /// 附加上游物理连接结束观测。
    #[must_use]
    pub fn with_connection_observation(
        mut self,
        observation: ProviderConnectionObservation,
    ) -> Self {
        self.failure_observation_mut().connection = Some(observation);
        self
    }

    fn failure_observation_mut(&mut self) -> &mut ProviderErrorFailureObservation {
        self.failure_observation
            .get_or_insert_with(|| Box::new(ProviderErrorFailureObservation::default()))
    }

    /// 标记 Provider 已证明本次失败可在下游提交前安全重放。
    ///
    /// 证明可以来自明确的“未执行”拒绝，也可以来自同 attempt 已交付、可恢复
    /// 同一业务请求的状态检查点；后者仍须由 Core 的同账号恢复门禁单独校验。
    #[must_use]
    pub const fn with_replay_safe(mut self) -> Self {
        self.replay_safe = true;
        self
    }

    /// 原地标记 Provider 已证明本次失败可从已交付的状态检查点安全恢复。
    pub const fn set_replay_safe(&mut self) {
        self.replay_safe = true;
    }

    /// 允许 Core 仅在客户端尚未收到任何事件时执行一次受预算约束的换号恢复。
    ///
    /// 该标记不证明上游未执行，也不等价于 [`Self::with_replay_safe`]；它只表达
    /// Provider 选择了“客户端无感恢复优先”的传输策略。Core 在 continuation、
    /// 指定账号或下游已经进入提交状态时必须忽略它。
    #[must_use]
    pub const fn with_pre_delivery_retry(mut self) -> Self {
        self.pre_delivery_retry = Some(PreDeliveryRetry::AccountRotation);
        self
    }

    /// 允许 Core 在首个客户端事件前固定原账号并改用 Provider 备用传输。
    ///
    /// 与普通换号恢复相同，Core 仍会拒绝 continuation、指定账号、预算耗尽或
    /// 下游已提交后的隐藏重放。
    #[must_use]
    pub const fn with_pre_delivery_transport_fallback(mut self) -> Self {
        self.set_pre_delivery_transport_fallback();
        self
    }

    /// 原地设置同账号备用传输恢复，保留当前错误携带的原客户端响应。
    pub const fn set_pre_delivery_transport_fallback(&mut self) {
        self.pre_delivery_retry = Some(PreDeliveryRetry::SameAccountTransportFallback);
    }

    /// 允许 Core 在首个客户端事件前固定原账号并重试当前 Provider 传输。
    ///
    /// 重试预算和退避策略由 Provider 所属协议定义；Core 只负责同账号钉选、
    /// deadline/取消边界以及下游 commit barrier。
    #[must_use]
    pub const fn with_pre_delivery_transport_retry(
        mut self,
        retry_index: NonZeroU32,
        delay: Duration,
    ) -> Self {
        self.set_pre_delivery_transport_retry(retry_index, delay);
        self
    }

    /// 原地设置同账号当前传输重试，保留当前错误携带的原客户端响应。
    pub const fn set_pre_delivery_transport_retry(
        &mut self,
        retry_index: NonZeroU32,
        delay: Duration,
    ) {
        self.pre_delivery_retry =
            Some(PreDeliveryRetry::SameAccountTransportRetry { retry_index, delay });
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

    /// 附加 Adapter 已确认可安全持久化和展示的诊断摘要。
    #[must_use]
    pub fn with_diagnostic(mut self, diagnostic: ProviderDiagnostic) -> Self {
        self.diagnostic = Some(Box::new(diagnostic));
        self
    }

    /// 附加将由运维错误详情原样展示的上游错误返回。
    #[must_use]
    pub fn with_raw_upstream_error(mut self, error: RawUpstreamError) -> Self {
        self.raw_upstream_error = Some(Box::new(error));
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

    /// 返回 Provider 明确选择的 previous-response 恢复边界。
    #[must_use]
    pub const fn continuation_recovery_disposition(
        &self,
    ) -> Option<ContinuationRecoveryDisposition> {
        self.continuation_recovery_disposition
    }

    #[must_use]
    pub fn continuation_unavailable_reason(&self) -> Option<&'static str> {
        match self.failure_observation.as_deref() {
            Some(observation) => observation.continuation_unavailable_reason,
            None => None,
        }
    }

    #[must_use]
    pub fn connection_observation(&self) -> Option<&ProviderConnectionObservation> {
        self.failure_observation
            .as_deref()
            .and_then(|observation| observation.connection.as_ref())
    }

    /// 返回 Provider 是否已证明本次失败可安全重放。
    #[must_use]
    pub const fn replay_is_safe(&self) -> bool {
        self.replay_safe
    }

    /// 返回 Provider 是否允许在首个客户端事件前隐藏恢复传输失败。
    #[must_use]
    pub const fn allows_pre_delivery_retry(&self) -> bool {
        self.pre_delivery_retry.is_some()
    }

    /// 返回下游提交前由 Provider 选择的隐藏恢复方式。
    #[must_use]
    pub const fn pre_delivery_retry(&self) -> Option<PreDeliveryRetry> {
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

    /// 返回可安全持久化和展示的 Provider 诊断摘要。
    #[must_use]
    pub fn diagnostic(&self) -> Option<&ProviderDiagnostic> {
        self.diagnostic.as_deref()
    }

    /// 返回将由运维错误详情原样展示的上游错误返回。
    #[must_use]
    pub fn raw_upstream_error(&self) -> Option<&RawUpstreamError> {
        self.raw_upstream_error.as_deref()
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
            continuation_recovery_disposition: self.continuation_recovery_disposition,
            failure_observation: self.failure_observation.clone(),
            replay_safe: self.replay_safe,
            pre_delivery_retry: self.pre_delivery_retry,
            credential_recovery_required: self.credential_recovery_required,
            retry_same_account: self.retry_same_account,
            sensitive_context_redacted: self.sensitive_context_redacted,
            diagnostic: self.diagnostic.clone(),
            raw_upstream_error: self.raw_upstream_error.clone(),
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
            .field(
                "continuation_recovery_disposition",
                &self.continuation_recovery_disposition,
            )
            .field(
                "continuation_unavailable_reason",
                &self.continuation_unavailable_reason(),
            )
            .field("connection_observation", &self.connection_observation())
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
            .field("diagnostic", &self.diagnostic.as_ref().map(|_| "<present>"))
            .field(
                "raw_upstream_error",
                &self.raw_upstream_error.as_ref().map(|_| "<present>"),
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
    diagnostic: Option<ProviderDiagnostic>,
    client_visible_upstream_error: Option<ClientVisibleUpstreamError>,
}

impl GatewayError {
    /// 使用静态安全消息创建错误。
    #[must_use]
    pub const fn new(kind: GatewayErrorKind, message: &'static str) -> Self {
        Self {
            kind,
            message,
            diagnostic: None,
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
            ProviderErrorKind::ContinuationRecoveryRequired => Self::new(
                GatewayErrorKind::InvalidRequest,
                "conversation continuation must be rebuilt",
            ),
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
        let gateway = match error.diagnostic() {
            Some(diagnostic) => gateway.with_diagnostic(diagnostic.clone()),
            None => gateway,
        };
        match error.client_visible_upstream_error() {
            Some(upstream) => gateway.with_client_visible_upstream_error(upstream.clone()),
            None => gateway,
        }
    }

    /// 附加只供运维观测使用的脱敏 Provider 摘要。
    #[must_use]
    pub fn with_diagnostic(mut self, diagnostic: ProviderDiagnostic) -> Self {
        self.diagnostic = Some(diagnostic);
        self
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

    /// 返回可安全持久化和展示的 Provider 诊断摘要。
    #[must_use]
    pub fn diagnostic(&self) -> Option<&ProviderDiagnostic> {
        self.diagnostic.as_ref()
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
            .field("diagnostic", &self.diagnostic.as_ref().map(|_| "<present>"))
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
