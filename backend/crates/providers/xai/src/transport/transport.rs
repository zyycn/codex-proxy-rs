use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use futures::Stream;
use gateway_core::engine::UpstreamSendState;
use gateway_core::error::{ClientVisibleUpstreamError, OpaqueUpstreamValue};
use gateway_core::event::UpstreamHttpVersion;
use url::Url;
use zeroize::Zeroizing;

use super::{GrokHeader, GrokSessionBinding};

/// 交给注入的 HTTP SSE transport 的自持有请求。
pub struct GrokInferenceRequest {
    endpoint: Url,
    headers: Vec<GrokHeader>,
    body: Zeroizing<Vec<u8>>,
    binding: GrokSessionBinding,
}

impl GrokInferenceRequest {
    pub fn new(
        endpoint: Url,
        headers: Vec<GrokHeader>,
        body: Vec<u8>,
        binding: GrokSessionBinding,
    ) -> Self {
        Self {
            endpoint,
            headers,
            body: Zeroizing::new(body),
            binding,
        }
    }

    /// 返回严格限定的官方 Responses 端点。
    #[must_use]
    pub const fn endpoint(&self) -> &Url {
        &self.endpoint
    }

    /// 返回类型化 header；适配层不得记录敏感值。
    #[must_use]
    pub fn headers(&self) -> &[GrokHeader] {
        &self.headers
    }

    /// 返回序列化后的类型化 Responses body。
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// 返回假名化的 proxy/egress 查找绑定。
    #[must_use]
    pub const fn binding(&self) -> &GrokSessionBinding {
        &self.binding
    }
}

impl fmt::Debug for GrokInferenceRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrokInferenceRequest")
            .field("endpoint", &self.endpoint)
            .field("headers", &self.headers)
            .field(
                "body",
                &format_args!("<{} bytes redacted>", self.body.len()),
            )
            .field("binding", &self.binding)
            .finish()
    }
}

/// POST 被接受后返回的原始 SSE 字节块流。
pub type GrokInferenceChunkStream =
    Pin<Box<dyn Stream<Item = Result<bytes::Bytes, GrokInferenceTransportError>> + Send + 'static>>;

/// 账号隔离的推理 client 是否已在缓存中。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrokInferenceClientCacheStatus {
    /// 绑定复用了已有的账号隔离 client。
    Hit,
    /// 请求首次查缓存时该绑定不存在。
    Miss,
}

impl GrokInferenceClientCacheStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::Miss => "miss",
        }
    }
}

/// 提供本次请求所用地址的解析器。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrokInferenceDnsSource {
    /// 系统解析器返回了完全公网的地址集合。
    System,
    /// 系统结果不可用，由受信 DoH 回退提供地址。
    TrustedDoh,
}

impl GrokInferenceDnsSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::TrustedDoh => "trusted_doh",
        }
    }
}

/// 建立上游连接期间观测到的 DNS 工作量。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrokInferenceDnsObservation {
    source: GrokInferenceDnsSource,
    duration_ms: u64,
}

impl GrokInferenceDnsObservation {
    #[must_use]
    pub const fn new(source: GrokInferenceDnsSource, duration_ms: u64) -> Self {
        Self {
            source,
            duration_ms,
        }
    }

    #[must_use]
    pub const fn source(self) -> GrokInferenceDnsSource {
        self.source
    }

    #[must_use]
    pub const fn duration_ms(self) -> u64 {
        self.duration_ms
    }
}

/// 单次推理请求的低基数 transport 耗时与连接池事实。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GrokInferenceTransportMetrics {
    headers_ms: Option<u64>,
    client_cache_status: Option<GrokInferenceClientCacheStatus>,
    dns: Option<GrokInferenceDnsObservation>,
}

impl GrokInferenceTransportMetrics {
    #[must_use]
    pub const fn with_headers_ms(mut self, headers_ms: u64) -> Self {
        self.headers_ms = Some(headers_ms);
        self
    }

    #[must_use]
    pub const fn with_client_cache_status(
        mut self,
        status: GrokInferenceClientCacheStatus,
    ) -> Self {
        self.client_cache_status = Some(status);
        self
    }

    #[must_use]
    pub const fn with_dns(mut self, dns: GrokInferenceDnsObservation) -> Self {
        self.dns = Some(dns);
        self
    }

    #[must_use]
    pub const fn headers_ms(self) -> Option<u64> {
        self.headers_ms
    }

    #[must_use]
    pub const fn client_cache_status(self) -> Option<GrokInferenceClientCacheStatus> {
        self.client_cache_status
    }

    #[must_use]
    pub const fn dns(self) -> Option<GrokInferenceDnsObservation> {
        self.dns
    }
}

/// 已被接受的推理响应。非成功 HTTP 响应必须改为返回
/// [`GrokInferenceTransportError`]。
pub struct GrokInferenceResponse {
    body: GrokInferenceChunkStream,
    http_version: UpstreamHttpVersion,
    status_code: u16,
    request_id: Option<OpaqueUpstreamValue>,
    transport_metrics: GrokInferenceTransportMetrics,
}

impl GrokInferenceResponse {
    /// 包装一个已被接受的 SSE 响应体。
    #[must_use]
    pub fn new(
        body: GrokInferenceChunkStream,
        http_version: UpstreamHttpVersion,
        status_code: u16,
        request_id: Option<OpaqueUpstreamValue>,
    ) -> Self {
        Self {
            body,
            http_version,
            status_code,
            request_id,
            transport_metrics: GrokInferenceTransportMetrics::default(),
        }
    }

    #[must_use]
    pub const fn with_transport_metrics(
        mut self,
        transport_metrics: GrokInferenceTransportMetrics,
    ) -> Self {
        self.transport_metrics = transport_metrics;
        self
    }

    #[must_use]
    pub const fn http_version(&self) -> UpstreamHttpVersion {
        self.http_version
    }

    #[must_use]
    pub const fn status_code(&self) -> u16 {
        self.status_code
    }

    #[must_use]
    pub const fn request_id(&self) -> Option<&OpaqueUpstreamValue> {
        self.request_id.as_ref()
    }

    #[must_use]
    pub const fn transport_metrics(&self) -> GrokInferenceTransportMetrics {
        self.transport_metrics
    }

    pub fn into_body(self) -> GrokInferenceChunkStream {
        self.body
    }
}

impl fmt::Debug for GrokInferenceResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrokInferenceResponse")
            .field("http_version", &self.http_version)
            .field("status_code", &self.status_code)
            .field("request_id", &self.request_id)
            .field("transport_metrics", &self.transport_metrics)
            .field("body", &"[SSE STREAM]")
            .finish()
    }
}

/// 不含密钥的 transport 失败分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrokInferenceTransportErrorKind {
    /// 官方代理拒绝了 canonical 请求语义。
    InvalidRequest,
    /// 官方代理不支持请求的某项能力。
    Unsupported,
    /// access token 被拒绝。
    Unauthorized,
    /// 会话缺少模型或特性授权。
    PermissionDenied,
    /// 会话级或 Provider 级限流。
    RateLimited,
    /// 账号付费额度或 spending limit 耗尽。
    QuotaExhausted,
    /// 账号订阅级免费额度耗尽。
    FreeQuotaExhausted,
    /// 上游声明指定模型的免费用量耗尽；selector 会收敛为账号级可恢复额度状态。
    ModelQuotaExhausted,
    /// 当前账号只缺少指定模型的访问权限。
    ModelAccessDenied,
    /// 上游要求付费，但没有给出可证明账号额度耗尽的稳定信号。
    PaymentRequired,
    /// 当前请求被内容安全策略拒绝，不代表账号或模型不可用。
    SafetyRejected,
    /// 截止时间或 transport 超时。
    Timeout,
    /// 网络/TLS/连接失败。
    Transport,
    /// HTTP/SSE 响应违反预期契约。
    Protocol,
    /// 官方 CLI 代理不可用。
    Unavailable,
    /// transport 观测到调用方取消。
    Cancelled,
}

/// 已分类的 transport 错误，绝不包含上游响应体。
#[derive(Clone, PartialEq, Eq)]
pub struct GrokInferenceTransportError {
    kind: GrokInferenceTransportErrorKind,
    send_state: UpstreamSendState,
    status: Option<u16>,
    retry_after: Option<Duration>,
    http_version: Option<UpstreamHttpVersion>,
    request_id: Option<OpaqueUpstreamValue>,
    upstream_code: Option<Box<OpaqueUpstreamValue>>,
    client_visible_upstream_error: Option<Box<ClientVisibleUpstreamError>>,
    transport_metrics: GrokInferenceTransportMetrics,
    credential_recovery_required: bool,
    sensitive_context_redacted: bool,
}

impl GrokInferenceTransportError {
    /// 以 transport 保守判定的发送状态创建已分类错误。
    #[must_use]
    pub const fn new(kind: GrokInferenceTransportErrorKind, send_state: UpstreamSendState) -> Self {
        Self {
            kind,
            send_state,
            status: None,
            retry_after: None,
            http_version: None,
            request_id: None,
            upstream_code: None,
            client_visible_upstream_error: None,
            transport_metrics: GrokInferenceTransportMetrics {
                headers_ms: None,
                client_cache_status: None,
                dns: None,
            },
            credential_recovery_required: false,
            sensitive_context_redacted: false,
        }
    }

    /// 附着合法的 HTTP 状态码。
    #[must_use]
    pub fn with_status(mut self, status: u16) -> Self {
        if (100..=599).contains(&status) {
            self.status = Some(status);
        }
        self
    }

    /// 附着 transport 解析出的有界重试延迟。
    #[must_use]
    pub const fn with_retry_after(mut self, retry_after: Duration) -> Self {
        self.retry_after = Some(retry_after);
        self
    }

    #[must_use]
    pub fn with_response_facts(
        mut self,
        http_version: UpstreamHttpVersion,
        request_id: Option<OpaqueUpstreamValue>,
    ) -> Self {
        self.http_version = Some(http_version);
        self.request_id = request_id;
        self
    }

    /// 附着从错误 JSON 中提取并清洗后的稳定机器码。
    #[must_use]
    pub fn with_upstream_code(mut self, code: OpaqueUpstreamValue) -> Self {
        self.upstream_code = Some(Box::new(code));
        self
    }

    /// 附着仅供原客户端协议展示的结构化上游错误。
    #[must_use]
    pub fn with_client_visible_upstream_error(mut self, error: ClientVisibleUpstreamError) -> Self {
        self.client_visible_upstream_error = Some(Box::new(error));
        self
    }

    #[must_use]
    pub const fn with_transport_metrics(
        mut self,
        transport_metrics: GrokInferenceTransportMetrics,
    ) -> Self {
        self.transport_metrics = transport_metrics;
        self
    }

    #[must_use]
    pub const fn with_credential_recovery(mut self) -> Self {
        self.credential_recovery_required = true;
        self
    }

    /// 丢弃可能敏感的上游响应体，仅保留「已丢弃」这一事实。
    #[must_use]
    pub fn redact_sensitive_context(mut self, _value: impl AsRef<str>) -> Self {
        self.sensitive_context_redacted = true;
        self
    }

    /// 返回稳定的 transport 分类。
    #[must_use]
    pub const fn kind(&self) -> GrokInferenceTransportErrorKind {
        self.kind
    }

    /// 返回保守判定的 payload 发送状态。
    #[must_use]
    pub const fn send_state(&self) -> UpstreamSendState {
        self.send_state
    }

    /// 返回清洗后的 HTTP 状态码。
    #[must_use]
    pub const fn status(&self) -> Option<u16> {
        self.status
    }

    /// 返回可选的重试延迟。
    #[must_use]
    pub const fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }

    #[must_use]
    pub const fn http_version(&self) -> Option<UpstreamHttpVersion> {
        self.http_version
    }

    #[must_use]
    pub const fn request_id(&self) -> Option<&OpaqueUpstreamValue> {
        self.request_id.as_ref()
    }

    #[must_use]
    pub fn upstream_code(&self) -> Option<&OpaqueUpstreamValue> {
        self.upstream_code.as_deref()
    }

    #[must_use]
    pub fn client_visible_upstream_error(&self) -> Option<&ClientVisibleUpstreamError> {
        self.client_visible_upstream_error.as_deref()
    }

    #[must_use]
    pub const fn transport_metrics(&self) -> GrokInferenceTransportMetrics {
        self.transport_metrics
    }

    #[must_use]
    pub const fn requires_credential_recovery(&self) -> bool {
        self.credential_recovery_required
    }

    /// 报告是否丢弃过敏感响应体。
    #[must_use]
    pub const fn sensitive_context_was_redacted(&self) -> bool {
        self.sensitive_context_redacted
    }
}

impl fmt::Debug for GrokInferenceTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrokInferenceTransportError")
            .field("kind", &self.kind)
            .field("send_state", &self.send_state)
            .field("status", &self.status)
            .field("retry_after", &self.retry_after)
            .field("http_version", &self.http_version)
            .field("request_id", &self.request_id)
            .field("upstream_code", &self.upstream_code)
            .field(
                "client_visible_upstream_error",
                &self
                    .client_visible_upstream_error
                    .as_ref()
                    .map(|_| "<present>"),
            )
            .field("transport_metrics", &self.transport_metrics)
            .field(
                "credential_recovery_required",
                &self.credential_recovery_required,
            )
            .field(
                "sensitive_context",
                &self.sensitive_context_redacted.then_some("[REDACTED]"),
            )
            .finish()
    }
}

impl fmt::Display for GrokInferenceTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Grok inference transport failed: {:?}",
            self.kind
        )
    }
}

impl std::error::Error for GrokInferenceTransportError {}

/// 推理 transport 返回的 future。
pub type GrokInferenceTransportFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<GrokInferenceResponse, GrokInferenceTransportError>> + Send + 'a,
    >,
>;

/// 运行时 HTTP SSE 端口。实现必须只发出一次 POST，用传入的 session binding
/// 维持 egress 亲和，且不得重试、切换凭据或回退到其他端点。
pub trait GrokInferenceTransport: Send + Sync {
    /// 发起一次官方 CLI 代理请求。
    fn execute(&self, request: GrokInferenceRequest) -> GrokInferenceTransportFuture<'_>;
}
