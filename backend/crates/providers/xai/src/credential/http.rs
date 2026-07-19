use std::fmt;
use std::future::Future;
use std::pin::Pin;

use url::Url;
use zeroize::Zeroizing;

use crate::SecretValue;

/// HTTP methods needed by the OAuth protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    /// Read-only discovery request.
    Get,
    /// Form-encoded OAuth request.
    Post,
}

/// A request header whose value is public protocol metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpHeader {
    name: &'static str,
    value: String,
}

impl HttpHeader {
    /// Creates a public request header.
    #[must_use]
    pub fn new(name: &'static str, value: impl Into<String>) -> Self {
        Self {
            name,
            value: value.into(),
        }
    }

    /// Returns the header name.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the public header value.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }
}

/// A public or sensitive form field value.
#[derive(Clone)]
pub enum FormValue {
    /// Non-sensitive protocol metadata.
    Public(String),
    /// Authorization code, verifier, or token.
    Secret(SecretValue),
}

impl FormValue {
    /// Exposes a value at the HTTP encoder boundary.
    #[must_use]
    pub fn expose(&self) -> &str {
        match self {
            Self::Public(value) => value,
            Self::Secret(value) => value.expose(),
        }
    }

    /// Reports whether debug and telemetry layers must redact this value.
    #[must_use]
    pub const fn is_secret(&self) -> bool {
        matches!(self, Self::Secret(_))
    }
}

impl fmt::Debug for FormValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Public(value) => formatter.debug_tuple("Public").field(value).finish(),
            Self::Secret(_) => formatter.write_str("Secret([REDACTED])"),
        }
    }
}

/// One `application/x-www-form-urlencoded` field.
#[derive(Debug, Clone)]
pub struct FormField {
    name: &'static str,
    value: FormValue,
}

impl FormField {
    /// Creates a public field.
    #[must_use]
    pub fn public(name: &'static str, value: impl Into<String>) -> Self {
        Self {
            name,
            value: FormValue::Public(value.into()),
        }
    }

    /// Creates a sensitive field.
    #[must_use]
    pub fn secret(name: &'static str, value: SecretValue) -> Self {
        Self {
            name,
            value: FormValue::Secret(value),
        }
    }

    /// Returns the form field name.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the form field value.
    #[must_use]
    pub fn value(&self) -> &FormValue {
        &self.value
    }
}

/// A transport-neutral OAuth HTTP request.
#[derive(Debug, Clone)]
pub struct OAuthHttpRequest {
    method: HttpMethod,
    url: Url,
    headers: Vec<HttpHeader>,
    form: Vec<FormField>,
}

impl OAuthHttpRequest {
    pub fn get(url: Url) -> Self {
        Self {
            method: HttpMethod::Get,
            url,
            headers: Vec::new(),
            form: Vec::new(),
        }
    }

    pub fn post(url: Url, headers: Vec<HttpHeader>, form: Vec<FormField>) -> Self {
        Self {
            method: HttpMethod::Post,
            url,
            headers,
            form,
        }
    }

    /// Returns the request method.
    #[must_use]
    pub const fn method(&self) -> HttpMethod {
        self.method
    }

    /// Returns the validated destination URL.
    #[must_use]
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// Returns public request headers.
    #[must_use]
    pub fn headers(&self) -> &[HttpHeader] {
        &self.headers
    }

    /// Returns form fields. Transport implementations must not log them.
    #[must_use]
    pub fn form(&self) -> &[FormField] {
        &self.form
    }
}

/// A response body that is zeroized and omitted from debug output.
pub struct OAuthHttpResponse {
    status: u16,
    body: Zeroizing<Vec<u8>>,
}

impl OAuthHttpResponse {
    /// Constructs a response from a status code and raw body.
    #[must_use]
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            body: Zeroizing::new(body.into()),
        }
    }

    /// Returns the HTTP status code.
    #[must_use]
    pub const fn status(&self) -> u16 {
        self.status
    }

    pub(crate) fn body(&self) -> &[u8] {
        self.body.as_slice()
    }
}

impl fmt::Debug for OAuthHttpResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OAuthHttpResponse")
            .field("status", &self.status)
            .field("body", &"[REDACTED]")
            .field("body_len", &self.body.len())
            .finish()
    }
}

/// Whether an HTTP failure definitely occurred before sending or may have
/// consumed a one-time authorization artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportFailureKind {
    /// No request bytes were sent, so a coordinator may safely retry.
    NotSent,
    /// The server may have consumed a code or rotating refresh token.
    Ambiguous,
    /// A timeout occurred and send state is unknown.
    Timeout,
    /// TLS establishment or validation failed before an accepted response.
    Tls,
}

/// A deliberately low-cardinality, secret-free transport error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportFailure {
    kind: TransportFailureKind,
}

impl TransportFailure {
    /// Creates a transport failure without retaining a potentially sensitive
    /// third-party error message.
    #[must_use]
    pub const fn new(kind: TransportFailureKind) -> Self {
        Self { kind }
    }

    /// Returns the send-state classification.
    #[must_use]
    pub const fn kind(self) -> TransportFailureKind {
        self.kind
    }
}

/// Future returned by an OAuth HTTP transport.
pub type TransportFuture<'a> =
    Pin<Box<dyn Future<Output = Result<OAuthHttpResponse, TransportFailure>> + Send + 'a>>;

/// Replaceable HTTP boundary for OAuth protocol requests.
///
/// Implementations must enforce TLS, bounded response bodies, and proxy/IP
/// affinity outside this crate. They must never log form values or response
/// bodies.
pub trait OAuthHttpTransport: Send + Sync {
    /// Executes one request without hidden retries.
    fn execute(&self, request: OAuthHttpRequest) -> TransportFuture<'_>;
}
