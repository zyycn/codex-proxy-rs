use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use futures::Stream;
use gateway_core::engine::UpstreamSendState;
use gateway_core::error::SafeUpstreamValue;
use gateway_core::event::UpstreamHttpVersion;
use url::Url;
use zeroize::Zeroizing;

use super::{GrokHeader, GrokSessionBinding};

/// Owned request handed to the injected HTTP SSE transport.
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

    /// Returns the strict official Responses endpoint.
    #[must_use]
    pub const fn endpoint(&self) -> &Url {
        &self.endpoint
    }

    /// Returns typed headers; adapters must not log sensitive values.
    #[must_use]
    pub fn headers(&self) -> &[GrokHeader] {
        &self.headers
    }

    /// Returns the serialized typed Responses body.
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Returns the pseudonymous proxy/egress lookup binding.
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

/// Stream of raw SSE byte chunks returned after the POST is accepted.
pub type GrokInferenceChunkStream =
    Pin<Box<dyn Stream<Item = Result<Vec<u8>, GrokInferenceTransportError>> + Send + 'static>>;

/// Accepted inference response. Non-success HTTP responses must be returned as
/// [`GrokInferenceTransportError`] instead.
pub struct GrokInferenceResponse {
    body: GrokInferenceChunkStream,
    http_version: UpstreamHttpVersion,
    status_code: u16,
    request_id: Option<SafeUpstreamValue>,
}

impl GrokInferenceResponse {
    /// Wraps one accepted SSE response body.
    #[must_use]
    pub fn new(
        body: GrokInferenceChunkStream,
        http_version: UpstreamHttpVersion,
        status_code: u16,
        request_id: Option<SafeUpstreamValue>,
    ) -> Self {
        Self {
            body,
            http_version,
            status_code,
            request_id,
        }
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
    pub const fn request_id(&self) -> Option<&SafeUpstreamValue> {
        self.request_id.as_ref()
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
            .field("body", &"[SSE STREAM]")
            .finish()
    }
}

/// Secret-free transport failure category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrokInferenceTransportErrorKind {
    /// Canonical request semantics were rejected by the official proxy.
    InvalidRequest,
    /// The official proxy does not support a requested capability.
    Unsupported,
    /// Access token was rejected.
    Unauthorized,
    /// Session lacks model or feature entitlement.
    PermissionDenied,
    /// Per-session or provider rate limit.
    RateLimited,
    /// Account credits or quota are exhausted.
    QuotaExhausted,
    /// Deadline or transport timeout.
    Timeout,
    /// Network/TLS/connection failure.
    Transport,
    /// HTTP/SSE response violates the expected contract.
    Protocol,
    /// Official CLI proxy is unavailable.
    Unavailable,
    /// Caller cancellation observed by the transport.
    Cancelled,
}

/// Classified transport error that never contains an upstream response body.
#[derive(Clone, PartialEq, Eq)]
pub struct GrokInferenceTransportError {
    kind: GrokInferenceTransportErrorKind,
    send_state: UpstreamSendState,
    status: Option<u16>,
    retry_after: Option<Duration>,
    http_version: Option<UpstreamHttpVersion>,
    request_id: Option<SafeUpstreamValue>,
    upstream_code: Option<SafeUpstreamValue>,
    credential_recovery_required: bool,
    sensitive_context_redacted: bool,
}

impl GrokInferenceTransportError {
    /// Creates a classified error with the transport's conservative send state.
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
            credential_recovery_required: false,
            sensitive_context_redacted: false,
        }
    }

    /// Attaches a valid HTTP status.
    #[must_use]
    pub fn with_status(mut self, status: u16) -> Self {
        if (100..=599).contains(&status) {
            self.status = Some(status);
        }
        self
    }

    /// Attaches a bounded retry delay parsed by the transport.
    #[must_use]
    pub const fn with_retry_after(mut self, retry_after: Duration) -> Self {
        self.retry_after = Some(retry_after);
        self
    }

    #[must_use]
    pub fn with_response_facts(
        mut self,
        http_version: UpstreamHttpVersion,
        request_id: Option<SafeUpstreamValue>,
    ) -> Self {
        self.http_version = Some(http_version);
        self.request_id = request_id;
        self
    }

    /// 附着从错误 JSON 中提取并清洗后的稳定机器码。
    #[must_use]
    pub fn with_upstream_code(mut self, code: SafeUpstreamValue) -> Self {
        self.upstream_code = Some(code);
        self
    }

    #[must_use]
    pub const fn with_credential_recovery(mut self) -> Self {
        self.credential_recovery_required = true;
        self
    }

    /// Discards a possibly sensitive upstream body while retaining that fact.
    #[must_use]
    pub fn redact_sensitive_context(mut self, _value: impl AsRef<str>) -> Self {
        self.sensitive_context_redacted = true;
        self
    }

    /// Returns the stable transport category.
    #[must_use]
    pub const fn kind(&self) -> GrokInferenceTransportErrorKind {
        self.kind
    }

    /// Returns the conservative payload send state.
    #[must_use]
    pub const fn send_state(&self) -> UpstreamSendState {
        self.send_state
    }

    /// Returns the sanitized HTTP status.
    #[must_use]
    pub const fn status(&self) -> Option<u16> {
        self.status
    }

    /// Returns the optional retry delay.
    #[must_use]
    pub const fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }

    #[must_use]
    pub const fn http_version(&self) -> Option<UpstreamHttpVersion> {
        self.http_version
    }

    #[must_use]
    pub const fn request_id(&self) -> Option<&SafeUpstreamValue> {
        self.request_id.as_ref()
    }

    #[must_use]
    pub const fn upstream_code(&self) -> Option<&SafeUpstreamValue> {
        self.upstream_code.as_ref()
    }

    #[must_use]
    pub const fn requires_credential_recovery(&self) -> bool {
        self.credential_recovery_required
    }

    /// Reports whether a sensitive body was discarded.
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

/// Future returned by the inference transport.
pub type GrokInferenceTransportFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<GrokInferenceResponse, GrokInferenceTransportError>> + Send + 'a,
    >,
>;

/// Runtime HTTP SSE port. Implementations must issue exactly one POST, use the
/// supplied session binding for egress affinity, and never retry, switch
/// credentials, or fall back to another endpoint.
pub trait GrokInferenceTransport: Send + Sync {
    /// Starts one official CLI proxy request.
    fn execute(&self, request: GrokInferenceRequest) -> GrokInferenceTransportFuture<'_>;
}
