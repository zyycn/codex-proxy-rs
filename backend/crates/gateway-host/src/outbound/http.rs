//! 受管 HTTP 客户端，约束出站网络、请求时限与响应读取

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use bytes::Bytes;
use gateway_core::{account::OutboundProxy, upstream::UpstreamSendState};
use http_body_util::{BodyExt as _, Full};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use reqwest::{
    Method, Url,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::Instant,
};

use super::{
    connector::{self, Connector},
    network::{self, DnsResolver, NetworkPolicy, SystemResolver},
};

type ManagedClient = Client<hyper_rustls::HttpsConnector<Connector>, Full<Bytes>>;

#[derive(Clone, PartialEq, Eq, Hash)]
struct ClientKey {
    origin: String,
    addresses: Vec<std::net::SocketAddr>,
    proxy: Option<OutboundProxy>,
    scope: Option<String>,
    websocket: bool,
}

pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, Vec<u8>)>,
    pub body: Vec<u8>,
}

pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, Vec<u8>)>,
    pub body: HttpBody,
}

pub struct HttpBody {
    response: hyper::body::Incoming,
    pending: Bytes,
    received: usize,
    deadline: Instant,
    _permit: OwnedSemaphorePermit,
}

#[derive(Debug, thiserror::Error)]
#[error("managed HTTP operation failed: {reason}")]
pub struct HttpError {
    reason: &'static str,
    kind: HttpErrorKind,
    pub send_state: UpstreamSendState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpErrorKind {
    InvalidRequest,
    AddressDenied,
    Dns,
    Timeout,
    Transport,
    Capacity,
    Response,
}

impl HttpError {
    pub const fn kind(&self) -> HttpErrorKind {
        self.kind
    }

    /// 固定诊断分类，不包含网址、请求头、正文或底层错误中的凭据
    pub const fn reason(&self) -> &'static str {
        self.reason
    }

    pub(super) fn invalid(reason: &'static str) -> Self {
        Self {
            reason,
            kind: HttpErrorKind::InvalidRequest,
            send_state: UpstreamSendState::NotSent,
        }
    }
    pub(super) fn with_kind(mut self, kind: HttpErrorKind) -> Self {
        self.kind = kind;
        self
    }

    pub(super) fn sent(reason: &'static str) -> Self {
        Self {
            reason,
            kind: HttpErrorKind::Response,
            send_state: UpstreamSendState::Sent,
        }
    }
}

pub struct HttpClient {
    clients: Mutex<HashMap<ClientKey, ManagedClient>>,
    requests: Arc<Semaphore>,
    resolver: Arc<dyn DnsResolver>,
    tls: Arc<rustls::ClientConfig>,
}

impl HttpClient {
    pub fn new() -> Result<Self, HttpError> {
        Self::with_root_certificates(vec![])
    }

    pub fn with_root_certificates(additional: Vec<Vec<u8>>) -> Result<Self, HttpError> {
        Ok(Self {
            clients: Mutex::new(HashMap::new()),
            requests: Arc::new(Semaphore::new(128)),
            resolver: Arc::new(SystemResolver),
            tls: connector::tls(additional)?,
        })
    }

    #[must_use]
    pub fn with_resolver(mut self, resolver: Arc<dyn DnsResolver>) -> Self {
        self.resolver = resolver;
        self
    }

    fn client(&self, key: ClientKey) -> ManagedClient {
        let mut clients = self
            .clients
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(client) = clients.get(&key) {
            return client.clone();
        }
        let connector = Connector::new(key.addresses.clone(), key.proxy.clone(), self.tls.clone());
        let connector = connector::https(connector, self.tls.as_ref().clone(), !key.websocket);
        let client = Client::builder(TokioExecutor::new())
            .pool_max_idle_per_host(4)
            .http1_max_buf_size(64 * 1024)
            .retry_canceled_requests(false)
            .build(connector);
        // 淘汰缓存不影响已借出的 Client 或在途 body
        if clients.len() >= 64 {
            clients.clear();
        }
        clients.insert(key, client.clone());
        client
    }

    pub async fn open(
        &self,
        request: HttpRequest,
        proxy: Option<&OutboundProxy>,
        network: &NetworkPolicy,
        timeout: Duration,
    ) -> Result<HttpResponse, HttpError> {
        self.open_scoped(request, proxy, network, timeout, None)
            .await
    }

    /// 为同一代理端点上的不同管理身份隔离连接池；scope 不得包含 secret
    pub async fn open_scoped(
        &self,
        request: HttpRequest,
        proxy: Option<&OutboundProxy>,
        network: &NetworkPolicy,
        timeout: Duration,
        scope: Option<&str>,
    ) -> Result<HttpResponse, HttpError> {
        let response = self
            .send(request, proxy, network, timeout, scope, None)
            .await?;
        Ok(response.into_http())
    }

    pub(super) async fn send(
        &self,
        request: HttpRequest,
        proxy: Option<&OutboundProxy>,
        network: &NetworkPolicy,
        timeout: Duration,
        scope: Option<&str>,
        websocket_key: Option<&str>,
    ) -> Result<PendingResponse, HttpError> {
        if request.body.len() > 16 * 1024 * 1024
            || request.headers.len() > 128
            || timeout.is_zero()
            || timeout > Duration::from_secs(120)
            || scope.is_some_and(|scope| {
                scope.is_empty() || scope.len() > 256 || scope.chars().any(char::is_control)
            })
        {
            return Err(HttpError::invalid("request limits"));
        }
        let url = Url::parse(&request.url).map_err(|_| HttpError::invalid("URL"))?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || url.host_str().is_none()
            || url.port_or_known_default().is_none_or(|port| port == 0)
        {
            return Err(HttpError::invalid("URL"));
        }
        let method = Method::from_bytes(request.method.as_bytes())
            .map_err(|_| HttpError::invalid("method"))?;
        if matches!(method, Method::CONNECT | Method::TRACE) {
            return Err(HttpError::invalid("method"));
        }
        let mut headers = HeaderMap::new();
        let mut header_bytes = 0;
        for (name, value) in request.headers {
            header_bytes += name.len() + value.len();
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| HttpError::invalid("header"))?;
            if header_bytes > 32 * 1024
                || matches!(
                    name.as_str(),
                    "host"
                        | "connection"
                        | "content-length"
                        | "transfer-encoding"
                        | "proxy-authorization"
                        | "upgrade"
                        | "te"
                        | "trailer"
                )
            {
                return Err(HttpError::invalid("header"));
            }
            headers.append(
                name,
                HeaderValue::from_bytes(&value).map_err(|_| HttpError::invalid("header"))?,
            );
        }
        let permit = self
            .requests
            .clone()
            .try_acquire_owned()
            .map_err(|_| HttpError::invalid("capacity").with_kind(HttpErrorKind::Capacity))?;
        let deadline = Instant::now() + timeout;
        let addresses =
            tokio::time::timeout_at(deadline, network::resolve(&self.resolver, &url, network))
                .await
                .map_err(|_| {
                    HttpError::invalid("DNS deadline").with_kind(HttpErrorKind::Timeout)
                })??;
        let client = self.client(ClientKey {
            origin: url.origin().ascii_serialization(),
            addresses: addresses.clone(),
            proxy: proxy.cloned(),
            scope: scope.map(str::to_owned),
            websocket: websocket_key.is_some(),
        });
        let mut uri: http::Uri = url
            .as_str()
            .parse()
            .map_err(|_| HttpError::invalid("URL"))?;
        if url.scheme() == "http"
            && let Some(proxy) = proxy.filter(|proxy| proxy.expose_url().starts_with("http"))
        {
            // 正向代理接收数字地址的绝对 URI；Host 保留原始域名，禁止代理再次解析目标
            let authority = uri
                .authority()
                .ok_or_else(|| HttpError::invalid("host"))?
                .as_str();
            headers.insert(
                http::header::HOST,
                HeaderValue::from_str(authority).map_err(|_| HttpError::invalid("host"))?,
            );
            let address = addresses
                .first()
                .ok_or_else(|| HttpError::invalid("address"))?;
            let mut parts = uri.into_parts();
            parts.authority = Some(
                address
                    .to_string()
                    .parse()
                    .map_err(|_| HttpError::invalid("address"))?,
            );
            uri = http::Uri::from_parts(parts).map_err(|_| HttpError::invalid("URL"))?;
            let proxy = Url::parse(proxy.expose_url()).map_err(|_| HttpError::invalid("proxy"))?;
            if let Some((username, password)) = connector::proxy_credentials(&proxy)? {
                headers.insert(
                    http::header::PROXY_AUTHORIZATION,
                    connector::basic_auth(&username, &password)?,
                );
            }
        }
        let mut outgoing = http::Request::new(Full::new(Bytes::from(request.body)));
        if let Some(key) = websocket_key {
            headers.insert(
                http::header::CONNECTION,
                HeaderValue::from_static("Upgrade"),
            );
            headers.insert(http::header::UPGRADE, HeaderValue::from_static("websocket"));
            headers.insert("sec-websocket-version", HeaderValue::from_static("13"));
            headers.insert(
                "sec-websocket-key",
                HeaderValue::from_str(key).map_err(|_| HttpError::invalid("WebSocket key"))?,
            );
        }
        *outgoing.method_mut() = method;
        *outgoing.uri_mut() = uri;
        *outgoing.headers_mut() = headers;
        let response = tokio::time::timeout_at(deadline, client.request(outgoing))
            .await
            .map_err(|_| HttpError {
                reason: "request deadline",
                kind: HttpErrorKind::Timeout,
                send_state: UpstreamSendState::Ambiguous,
            })?
            .map_err(|error| HttpError {
                reason: "transport",
                kind: HttpErrorKind::Transport,
                send_state: if error.is_connect() {
                    UpstreamSendState::NotSent
                } else {
                    UpstreamSendState::Ambiguous
                },
            })?;
        let status = response.status().as_u16();
        let headers: Vec<_> = response
            .headers()
            .iter()
            .map(|(name, value)| (name.to_string(), value.as_bytes().to_vec()))
            .collect();
        if headers
            .iter()
            .map(|(name, value)| name.len() + value.len())
            .sum::<usize>()
            > 32 * 1024
        {
            return Err(HttpError::sent("response headers"));
        }
        Ok(PendingResponse {
            status,
            headers,
            response,
            permit,
            deadline,
        })
    }
}

pub(super) struct PendingResponse {
    pub(super) status: u16,
    pub(super) headers: Vec<(String, Vec<u8>)>,
    pub(super) response: http::Response<hyper::body::Incoming>,
    pub(super) permit: OwnedSemaphorePermit,
    pub(super) deadline: Instant,
}

impl PendingResponse {
    pub(super) fn into_http(self) -> HttpResponse {
        HttpResponse {
            status: self.status,
            headers: self.headers,
            body: HttpBody {
                response: self.response.into_body(),
                pending: Bytes::new(),
                received: 0,
                deadline: self.deadline,
                _permit: self.permit,
            },
        }
    }
}

impl HttpBody {
    pub async fn read(&mut self, maximum: usize) -> Result<Option<Bytes>, HttpError> {
        if maximum == 0 || maximum > 64 * 1024 {
            return Err(HttpError::sent("read size"));
        }
        loop {
            // 已缓冲的数据和就绪帧也受同一期限约束，慢读者不能无限续用 HTTP 资源
            if Instant::now() >= self.deadline {
                return Err(HttpError::sent("response deadline").with_kind(HttpErrorKind::Timeout));
            }
            if !self.pending.is_empty() {
                return Ok(Some(self.pending.split_to(maximum.min(self.pending.len()))));
            }
            self.pending = match tokio::time::timeout_at(self.deadline, self.response.frame()).await
            {
                Ok(Some(Ok(frame))) => match frame.into_data() {
                    Ok(chunk) => chunk,
                    Err(_) => continue,
                },
                Ok(None) => return Ok(None),
                Ok(Some(Err(_))) => return Err(HttpError::sent("response body")),
                Err(_) => {
                    return Err(
                        HttpError::sent("response deadline").with_kind(HttpErrorKind::Timeout)
                    );
                }
            };
            self.received = self.received.saturating_add(self.pending.len());
            if self.received > 32 * 1024 * 1024 {
                return Err(HttpError::sent("response size"));
            }
        }
    }
}
