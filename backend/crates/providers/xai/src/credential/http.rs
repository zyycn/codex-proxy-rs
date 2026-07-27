use std::fmt;
use std::future::Future;
use std::pin::Pin;

use url::Url;
use zeroize::Zeroizing;

use crate::SecretValue;

/// OAuth 协议所需的 HTTP 方法。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    /// 只读的发现请求。
    Get,
    /// form 编码的 OAuth 请求。
    Post,
}

/// 值为公开协议元数据的请求头。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpHeader {
    name: &'static str,
    value: String,
}

impl HttpHeader {
    /// 创建公开请求头。
    #[must_use]
    pub fn new(name: &'static str, value: impl Into<String>) -> Self {
        Self {
            name,
            value: value.into(),
        }
    }

    /// 返回请求头名称。
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// 返回公开的请求头值。
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }
}

/// 公开或敏感的 form 字段值。
#[derive(Clone)]
pub enum FormValue {
    /// 非敏感的协议元数据。
    Public(String),
    /// authorization code、verifier 或 token。
    Secret(SecretValue),
}

impl FormValue {
    /// 在 HTTP 编码边界暴露值。
    #[must_use]
    pub fn expose(&self) -> &str {
        match self {
            Self::Public(value) => value,
            Self::Secret(value) => value.expose(),
        }
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

/// 单个 `application/x-www-form-urlencoded` 字段。
#[derive(Debug, Clone)]
pub struct FormField {
    name: &'static str,
    value: FormValue,
}

impl FormField {
    /// 创建公开字段。
    #[must_use]
    pub fn public(name: &'static str, value: impl Into<String>) -> Self {
        Self {
            name,
            value: FormValue::Public(value.into()),
        }
    }

    /// 创建敏感字段。
    #[must_use]
    pub fn secret(name: &'static str, value: SecretValue) -> Self {
        Self {
            name,
            value: FormValue::Secret(value),
        }
    }

    /// 返回 form 字段名。
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// 返回 form 字段值。
    #[must_use]
    pub fn value(&self) -> &FormValue {
        &self.value
    }
}

/// 与 transport 无关的 OAuth HTTP 请求。
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

    /// 返回请求方法。
    #[must_use]
    pub const fn method(&self) -> HttpMethod {
        self.method
    }

    /// 返回已校验的目标 URL。
    #[must_use]
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// 返回公开请求头。
    #[must_use]
    pub fn headers(&self) -> &[HttpHeader] {
        &self.headers
    }

    /// 返回 form 字段。transport 实现不得记录这些值。
    #[must_use]
    pub fn form(&self) -> &[FormField] {
        &self.form
    }
}

/// 响应体会被清零，且不出现在 debug 输出中。
pub struct OAuthHttpResponse {
    status: u16,
    body: Zeroizing<Vec<u8>>,
}

impl OAuthHttpResponse {
    /// 由状态码与原始响应体构造响应。
    #[must_use]
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            body: Zeroizing::new(body.into()),
        }
    }

    /// 返回 HTTP 状态码。
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

/// 区分 HTTP 失败确定发生在发送前，还是可能已消费一次性授权凭据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportFailureKind {
    /// 未发送任何请求字节，协调方可安全重试。
    NotSent,
    /// 服务端可能已消费 code 或轮换的 refresh token。
    Ambiguous,
    /// 发生超时，发送状态未知。
    Timeout,
    /// 在收到有效响应前 TLS 建立或校验失败。
    Tls,
}

/// 有意保持低基数且不含密钥的 transport 错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportFailure {
    kind: TransportFailureKind,
}

impl TransportFailure {
    /// 创建 transport 失败，不保留可能敏感的第三方错误信息。
    #[must_use]
    pub const fn new(kind: TransportFailureKind) -> Self {
        Self { kind }
    }

    /// 返回发送状态分类。
    #[must_use]
    pub const fn kind(self) -> TransportFailureKind {
        self.kind
    }
}

/// OAuth HTTP transport 返回的 Future。
pub type TransportFuture<'a> =
    Pin<Box<dyn Future<Output = Result<OAuthHttpResponse, TransportFailure>> + Send + 'a>>;

/// OAuth 协议请求的可替换 HTTP 边界。
///
/// 实现方需在本 crate 之外保证 TLS、有界响应体与 proxy/IP 亲和性，
/// 且不得记录 form 值或响应体。
pub trait OAuthHttpTransport: Send + Sync {
    /// 执行一次请求，不做隐式重试。
    fn execute(&self, request: OAuthHttpRequest) -> TransportFuture<'_>;
}
