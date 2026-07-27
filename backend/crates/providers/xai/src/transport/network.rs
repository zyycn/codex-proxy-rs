//! 关闭 redirect、proxy 与业务重试的生产 reqwest transport。

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use futures::StreamExt;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE, ETAG, RETRY_AFTER};
use reqwest::redirect::Policy;
use reqwest::{Client, Response, StatusCode};
use serde::Deserialize;
use serde_json::Value;
use url::Url;

use crate::credential::discovery::MAX_OAUTH_RESPONSE_BYTES;
use crate::{
    GrokBillingRequest, GrokBillingTransport, GrokBillingTransportError,
    GrokBillingTransportErrorKind, GrokBillingTransportFuture, GrokBillingTransportResponse,
    GrokInferenceClientCacheStatus, GrokInferenceDnsObservation, GrokInferenceDnsSource,
    GrokInferenceRequest, GrokInferenceResponse, GrokInferenceTransport,
    GrokInferenceTransportError, GrokInferenceTransportErrorKind, GrokInferenceTransportFuture,
    GrokInferenceTransportMetrics, GrokModelCatalogRequest, GrokModelCatalogTransport,
    GrokModelCatalogTransportError, GrokModelCatalogTransportErrorKind,
    GrokModelCatalogTransportFuture, GrokModelCatalogTransportResponse, GrokSessionBinding,
    HttpMethod, MAX_GROK_BILLING_BYTES, MAX_GROK_MODEL_CATALOG_BYTES, OAuthHttpRequest,
    OAuthHttpResponse, OAuthHttpTransport, TransportFailure, TransportFailureKind, TransportFuture,
};
use gateway_core::engine::UpstreamSendState;
use gateway_core::error::SafeUpstreamValue;
use gateway_core::event::UpstreamHttpVersion;

pub(crate) const OFFICIAL_OAUTH_HOST: &str = "auth.x.ai";
const OFFICIAL_INFERENCE_HOST: &str = "cli-chat-proxy.grok.com";
const OFFICIAL_INFERENCE_PATH: &str = "/v1/responses";
const OFFICIAL_MODEL_CATALOG_PATH: &str = "/v1/models";
const OFFICIAL_BILLING_PATH: &str = "/v1/billing";
pub(crate) const OFFICIAL_JWKS_PATH: &str = "/.well-known/jwks.json";
pub(crate) const OFFICIAL_USERINFO_PATH: &str = "/oauth2/userinfo";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const OAUTH_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(90);
const POOL_MAX_IDLE_PER_HOST: usize = 2;
const HTTP2_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(15);
const HTTP2_KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
const MAX_INFERENCE_BODY_BYTES: usize = 256 * 1024 * 1024;
const MAX_RETRY_AFTER_SECONDS: u64 = 120;
const TRUSTED_DOH_HOST: &str = "dns.google";
const TRUSTED_DOH_URL: &str = "https://dns.google/resolve";
const MAX_DOH_RESPONSE_BYTES: usize = 64 * 1024;
const DNS_RECORD_A: u16 = 1;
const DNS_RECORD_AAAA: u16 = 28;
const TRUSTED_DOH_BOOTSTRAP: [SocketAddr; 2] = [
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 443),
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 4, 4)), 443),
];

tokio::task_local! {
    static REQUEST_DNS_OBSERVER: Arc<RequestDnsObserver>;
}

/// 构建严格 reqwest transport 失败。
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum GrokReqwestTransportBuildError {
    /// Reqwest TLS/client 初始化失败。
    #[error("Grok reqwest transport initialization failed")]
    ClientInitialization,
}

/// 固定官方 host 的 DNS 解析路径；只有系统结果全部为公网地址时才直接使用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrokDnsResolutionPlan {
    /// 使用系统 resolver 返回的全部公网地址。
    System,
    /// 系统解析失败、为空或包含非公网地址，改用固定 bootstrap 的可信 DoH。
    TrustedDoh,
}

/// xAI 官方 host 的 DNS rebinding 防护策略。
#[derive(Debug, Clone, Copy)]
pub struct GrokDnsResolutionPolicy {
    allowed_host: &'static str,
}

impl GrokDnsResolutionPolicy {
    /// OAuth、JWKS 与 user-info 官方 host 策略。
    #[must_use]
    pub const fn official_oauth() -> Self {
        Self {
            allowed_host: OFFICIAL_OAUTH_HOST,
        }
    }

    /// 推理与模型目录官方 host 策略。
    #[must_use]
    pub const fn official_inference() -> Self {
        Self {
            allowed_host: OFFICIAL_INFERENCE_HOST,
        }
    }

    /// 决定系统解析结果可直接使用还是必须走可信 DoH。
    ///
    /// # Errors
    ///
    /// 请求 host 不等于本策略固定的官方 host 时拒绝，且不会触发 fallback。
    pub fn plan_system_resolution(
        self,
        requested_host: &str,
        addresses: &[IpAddr],
    ) -> Result<GrokDnsResolutionPlan, GrokDnsResolutionError> {
        self.ensure_host(requested_host)?;
        Ok(
            if !addresses.is_empty() && addresses.iter().copied().all(is_public_ip) {
                GrokDnsResolutionPlan::System
            } else {
                GrokDnsResolutionPlan::TrustedDoh
            },
        )
    }

    /// 验证可信 DoH 返回的整个地址集合；任一非公网地址会拒绝全部结果。
    ///
    /// # Errors
    ///
    /// Host 不匹配、结果为空或任一地址非公网时拒绝。
    pub fn validate_trusted_doh_resolution(
        self,
        requested_host: &str,
        addresses: &[IpAddr],
    ) -> Result<(), GrokDnsResolutionError> {
        self.ensure_host(requested_host)?;
        if addresses.is_empty()
            || addresses
                .iter()
                .copied()
                .any(|address| !is_public_ip(address))
        {
            return Err(GrokDnsResolutionError);
        }
        Ok(())
    }

    fn ensure_host(self, requested_host: &str) -> Result<(), GrokDnsResolutionError> {
        if requested_host.eq_ignore_ascii_case(self.allowed_host) {
            Ok(())
        } else {
            Err(GrokDnsResolutionError)
        }
    }
}

/// DNS policy 低基数错误；不保留请求 host、地址或 resolver 正文。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("Grok official DNS resolution was rejected")]
pub struct GrokDnsResolutionError;

/// HTTP client 构造与端点校验合为一个可注入的安全边界。
pub trait GrokEndpointPolicy: fmt::Debug + Send + Sync {
    fn build_oauth_client(
        &self,
        timeout: Option<Duration>,
    ) -> Result<Client, GrokReqwestTransportBuildError>;
    fn build_inference_client(
        &self,
        timeout: Option<Duration>,
    ) -> Result<Client, GrokReqwestTransportBuildError>;
    fn validate_oauth(&self, url: &Url) -> bool;
    fn validate_inference(&self, url: &Url) -> bool;
    fn validate_model_catalog(&self, url: &Url) -> bool;
    fn route_billing(&self, url: &Url) -> Option<Url>;
    fn validate_jwks(&self, url: &Url) -> bool;
    fn validate_userinfo(&self, url: &Url) -> bool;
}

#[derive(Debug, Default)]
pub struct OfficialGrokEndpointPolicy;

impl GrokEndpointPolicy for OfficialGrokEndpointPolicy {
    fn build_oauth_client(
        &self,
        timeout: Option<Duration>,
    ) -> Result<Client, GrokReqwestTransportBuildError> {
        build_official_client(GrokDnsResolutionPolicy::official_oauth(), timeout)
    }

    fn build_inference_client(
        &self,
        timeout: Option<Duration>,
    ) -> Result<Client, GrokReqwestTransportBuildError> {
        build_official_client(GrokDnsResolutionPolicy::official_inference(), timeout)
    }

    fn validate_oauth(&self, url: &Url) -> bool {
        valid_official_url(url, OFFICIAL_OAUTH_HOST, None)
    }

    fn validate_inference(&self, url: &Url) -> bool {
        valid_official_url(url, OFFICIAL_INFERENCE_HOST, Some(OFFICIAL_INFERENCE_PATH))
    }

    fn validate_model_catalog(&self, url: &Url) -> bool {
        valid_official_url(
            url,
            OFFICIAL_INFERENCE_HOST,
            Some(OFFICIAL_MODEL_CATALOG_PATH),
        )
    }

    fn route_billing(&self, url: &Url) -> Option<Url> {
        valid_billing_url(url, OFFICIAL_INFERENCE_HOST).then(|| url.clone())
    }

    fn validate_jwks(&self, url: &Url) -> bool {
        valid_official_url(url, OFFICIAL_OAUTH_HOST, Some(OFFICIAL_JWKS_PATH))
    }

    fn validate_userinfo(&self, url: &Url) -> bool {
        valid_official_url(url, OFFICIAL_OAUTH_HOST, Some(OFFICIAL_USERINFO_PATH))
    }
}

/// 官方 OAuth HTTP transport。只允许 `auth.x.ai:443`。
pub struct ReqwestOAuthTransport {
    client: Client,
    endpoint_policy: Arc<dyn GrokEndpointPolicy>,
}

impl ReqwestOAuthTransport {
    /// 使用系统原生根证书构建生产 transport。
    ///
    /// # Errors
    ///
    /// TLS client 初始化失败时返回错误。
    pub fn new(
        endpoint_policy: Arc<dyn GrokEndpointPolicy>,
    ) -> Result<Self, GrokReqwestTransportBuildError> {
        let client = endpoint_policy.build_oauth_client(Some(OAUTH_REQUEST_TIMEOUT))?;
        Ok(Self {
            client,
            endpoint_policy,
        })
    }
}

impl fmt::Debug for ReqwestOAuthTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReqwestOAuthTransport")
            .field("client", &"reqwest::Client")
            .field("endpoint_policy", &self.endpoint_policy)
            .finish()
    }
}

impl OAuthHttpTransport for ReqwestOAuthTransport {
    fn execute(&self, request: OAuthHttpRequest) -> TransportFuture<'_> {
        let client = self.client.clone();
        let endpoint_policy = self.endpoint_policy.clone();
        Box::pin(async move {
            if !endpoint_policy.validate_oauth(request.url()) {
                return Err(TransportFailure::new(TransportFailureKind::NotSent));
            }
            let mut builder = match request.method() {
                HttpMethod::Get => client.get(request.url().clone()),
                HttpMethod::Post => client.post(request.url().clone()),
            };
            for header in request.headers() {
                builder = builder.header(header.name(), header.value());
            }
            if request.method() == HttpMethod::Post {
                let form = request
                    .form()
                    .iter()
                    .map(|field| (field.name(), field.value().expose()))
                    .collect::<Vec<_>>();
                builder = builder.form(&form);
            }
            let response = builder.send().await.map_err(classify_oauth_reqwest_error)?;
            let status = response.status().as_u16();
            let body = match collect_bounded(response, MAX_OAUTH_RESPONSE_BYTES).await {
                Ok(BoundedBody::Body(body)) => body,
                Ok(BoundedBody::TooLarge) => vec![0_u8; MAX_OAUTH_RESPONSE_BYTES + 1],
                Err(_) => {
                    return Err(TransportFailure::new(TransportFailureKind::Ambiguous));
                }
            };
            Ok(OAuthHttpResponse::new(status, body))
        })
    }
}

/// 官方 Grok Responses HTTP SSE transport。
pub struct ReqwestGrokInferenceTransport {
    clients: Mutex<BoundInferenceClients>,
    endpoint_policy: Arc<dyn GrokEndpointPolicy>,
}

impl ReqwestGrokInferenceTransport {
    /// 单个进程缓存的账号隔离推理连接池上限。
    pub const MAX_CACHED_ACCOUNT_CLIENTS: usize = 64;

    /// 构建只允许官方 CLI proxy 的生产 transport。
    ///
    /// # Errors
    ///
    /// TLS client 初始化失败时返回错误。
    pub fn new(
        endpoint_policy: Arc<dyn GrokEndpointPolicy>,
    ) -> Result<Self, GrokReqwestTransportBuildError> {
        let unbound_client = endpoint_policy.build_inference_client(None)?;
        Ok(Self {
            clients: Mutex::new(BoundInferenceClients::new(unbound_client)),
            endpoint_policy,
        })
    }

    fn client_for(
        &self,
        binding: &GrokSessionBinding,
    ) -> Result<(Client, GrokInferenceClientCacheStatus), GrokInferenceTransportError> {
        let unbound_client = {
            let mut clients = self
                .clients
                .lock()
                .map_err(|_| inference_client_pool_unavailable())?;
            if let Some(client) = clients.get(binding) {
                return Ok((client, GrokInferenceClientCacheStatus::Hit));
            }
            clients.take_unbound()
        };

        let client = match unbound_client {
            Some(client) => client,
            None => self
                .endpoint_policy
                .build_inference_client(None)
                .map_err(|_| inference_client_pool_unavailable_for_cache_miss())?,
        };
        let client = self
            .clients
            .lock()
            .map_err(|_| inference_client_pool_unavailable_for_cache_miss())?
            .insert_if_absent(binding, client);
        Ok((client, GrokInferenceClientCacheStatus::Miss))
    }
}

impl fmt::Debug for ReqwestGrokInferenceTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReqwestGrokInferenceTransport")
            .field("clients", &"account-isolated reqwest::Client pools")
            .field("endpoint_policy", &self.endpoint_policy)
            .finish()
    }
}

impl GrokInferenceTransport for ReqwestGrokInferenceTransport {
    fn execute(&self, request: GrokInferenceRequest) -> GrokInferenceTransportFuture<'_> {
        Box::pin(async move {
            if !self.endpoint_policy.validate_inference(request.endpoint()) {
                return Err(GrokInferenceTransportError::new(
                    GrokInferenceTransportErrorKind::Protocol,
                    UpstreamSendState::NotSent,
                ));
            }
            let (client, client_cache_status) = self.client_for(request.binding())?;
            let mut builder = client
                .post(request.endpoint().clone())
                .body(request.body().to_vec());
            for header in request.headers() {
                builder = builder.header(header.name(), header.value().expose());
            }
            let dns_observer = Arc::new(RequestDnsObserver::default());
            let headers_started_at = Instant::now();
            let response = REQUEST_DNS_OBSERVER
                .scope(Arc::clone(&dns_observer), builder.send())
                .await
                .map_err(|error| {
                    classify_inference_reqwest_error(error).with_transport_metrics(
                        inference_transport_metrics(client_cache_status, None, &dns_observer),
                    )
                })?;
            let transport_metrics = inference_transport_metrics(
                client_cache_status,
                Some(elapsed_millis(headers_started_at.elapsed())),
                &dns_observer,
            );
            if !response.status().is_success() {
                return Err(classify_inference_status(response)
                    .await
                    .with_transport_metrics(transport_metrics));
            }
            let http_version = upstream_http_version(response.version());
            let status_code = response.status().as_u16();
            let request_id = upstream_request_id(&response);
            let body = response
                .bytes_stream()
                .scan(0_usize, move |observed, chunk| {
                    let item = match chunk {
                        Ok(chunk)
                            if observed
                                .checked_add(chunk.len())
                                .is_some_and(|total| total <= MAX_INFERENCE_BODY_BYTES) =>
                        {
                            *observed += chunk.len();
                            // Bytes 直传，长流不再逐 chunk 复制到 Vec。
                            Ok(chunk)
                        }
                        Ok(_) => Err(GrokInferenceTransportError::new(
                            GrokInferenceTransportErrorKind::Protocol,
                            UpstreamSendState::Sent,
                        )
                        .with_transport_metrics(transport_metrics)),
                        Err(error) => Err(classify_inference_stream_error(&error)
                            .with_transport_metrics(transport_metrics)),
                    };
                    std::future::ready(Some(item))
                });
            Ok(
                GrokInferenceResponse::new(Box::pin(body), http_version, status_code, request_id)
                    .with_transport_metrics(transport_metrics),
            )
        })
    }
}

struct BoundInferenceClients {
    by_binding: HashMap<GrokSessionBinding, Client>,
    least_recently_used: VecDeque<GrokSessionBinding>,
    unbound_client: Option<Client>,
}

impl BoundInferenceClients {
    fn new(unbound_client: Client) -> Self {
        Self {
            by_binding: HashMap::with_capacity(
                ReqwestGrokInferenceTransport::MAX_CACHED_ACCOUNT_CLIENTS,
            ),
            least_recently_used: VecDeque::with_capacity(
                ReqwestGrokInferenceTransport::MAX_CACHED_ACCOUNT_CLIENTS,
            ),
            unbound_client: Some(unbound_client),
        }
    }

    fn get(&mut self, binding: &GrokSessionBinding) -> Option<Client> {
        if let Some(client) = self.by_binding.get(binding).cloned() {
            self.record_use(binding);
            return Some(client);
        }
        None
    }

    fn take_unbound(&mut self) -> Option<Client> {
        self.unbound_client.take()
    }

    fn insert_if_absent(&mut self, binding: &GrokSessionBinding, client: Client) -> Client {
        if let Some(existing) = self.get(binding) {
            return existing;
        }
        self.insert(binding.clone(), client.clone());
        client
    }

    fn insert(&mut self, binding: GrokSessionBinding, client: Client) {
        if self.by_binding.len() == ReqwestGrokInferenceTransport::MAX_CACHED_ACCOUNT_CLIENTS
            && let Some(expired_binding) = self.least_recently_used.pop_front()
        {
            self.by_binding.remove(&expired_binding);
        }
        self.least_recently_used.push_back(binding.clone());
        self.by_binding.insert(binding, client);
    }

    fn record_use(&mut self, binding: &GrokSessionBinding) {
        self.least_recently_used
            .retain(|candidate| candidate != binding);
        self.least_recently_used.push_back(binding.clone());
    }
}

fn inference_client_pool_unavailable() -> GrokInferenceTransportError {
    GrokInferenceTransportError::new(
        GrokInferenceTransportErrorKind::Unavailable,
        UpstreamSendState::NotSent,
    )
}

fn inference_client_pool_unavailable_for_cache_miss() -> GrokInferenceTransportError {
    inference_client_pool_unavailable().with_transport_metrics(
        GrokInferenceTransportMetrics::default()
            .with_client_cache_status(GrokInferenceClientCacheStatus::Miss),
    )
}

fn inference_transport_metrics(
    client_cache_status: GrokInferenceClientCacheStatus,
    headers_ms: Option<u64>,
    dns_observer: &RequestDnsObserver,
) -> GrokInferenceTransportMetrics {
    let mut metrics =
        GrokInferenceTransportMetrics::default().with_client_cache_status(client_cache_status);
    if let Some(headers_ms) = headers_ms {
        metrics = metrics.with_headers_ms(headers_ms);
    }
    if let Some(dns) = dns_observer.observation() {
        metrics = metrics.with_dns(dns);
    }
    metrics
}

/// 官方 Grok CLI proxy 模型目录 GET transport。
pub struct ReqwestGrokModelCatalogTransport {
    client: Client,
    endpoint_policy: Arc<dyn GrokEndpointPolicy>,
}

impl ReqwestGrokModelCatalogTransport {
    /// 构建只允许官方 CLI proxy `/v1/models` 的生产 transport。
    ///
    /// # Errors
    ///
    /// TLS client 初始化失败时返回错误。
    pub fn new(
        endpoint_policy: Arc<dyn GrokEndpointPolicy>,
    ) -> Result<Self, GrokReqwestTransportBuildError> {
        let client = endpoint_policy.build_inference_client(Some(OAUTH_REQUEST_TIMEOUT))?;
        Ok(Self {
            client,
            endpoint_policy,
        })
    }
}

impl fmt::Debug for ReqwestGrokModelCatalogTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReqwestGrokModelCatalogTransport")
            .field("client", &"reqwest::Client")
            .field("endpoint_policy", &self.endpoint_policy)
            .finish()
    }
}

impl GrokModelCatalogTransport for ReqwestGrokModelCatalogTransport {
    fn execute(&self, request: GrokModelCatalogRequest) -> GrokModelCatalogTransportFuture<'_> {
        let client = self.client.clone();
        let endpoint_policy = self.endpoint_policy.clone();
        Box::pin(async move {
            if !endpoint_policy.validate_model_catalog(request.endpoint()) {
                return Err(GrokModelCatalogTransportError::new(
                    GrokModelCatalogTransportErrorKind::Protocol,
                ));
            }
            let mut builder = client.get(request.endpoint().clone());
            for header in request.headers() {
                builder = builder.header(header.name(), header.value().expose());
            }
            let response = builder
                .send()
                .await
                .map_err(classify_model_catalog_reqwest_error)?;
            if !response.status().is_success() {
                return Err(classify_model_catalog_status(response).await);
            }
            if !is_json_response(&response) {
                return Err(GrokModelCatalogTransportError::new(
                    GrokModelCatalogTransportErrorKind::Protocol,
                ));
            }
            let etag = response
                .headers()
                .get(ETAG)
                .map(|value| {
                    value.to_str().map(str::to_owned).map_err(|_| {
                        GrokModelCatalogTransportError::new(
                            GrokModelCatalogTransportErrorKind::Protocol,
                        )
                    })
                })
                .transpose()?;
            let body = match collect_bounded(response, MAX_GROK_MODEL_CATALOG_BYTES).await {
                Ok(BoundedBody::Body(body)) => body,
                Ok(BoundedBody::TooLarge) => {
                    return Err(GrokModelCatalogTransportError::new(
                        GrokModelCatalogTransportErrorKind::Protocol,
                    ));
                }
                Err(error) => return Err(classify_model_catalog_reqwest_error(error)),
            };
            Ok(GrokModelCatalogTransportResponse::new(body, etag))
        })
    }
}

impl GrokBillingTransport for ReqwestGrokModelCatalogTransport {
    fn execute(&self, request: GrokBillingRequest) -> GrokBillingTransportFuture<'_> {
        let client = self.client.clone();
        let endpoint_policy = self.endpoint_policy.clone();
        Box::pin(async move {
            let endpoint = endpoint_policy
                .route_billing(request.endpoint())
                .ok_or_else(|| {
                    GrokBillingTransportError::new(GrokBillingTransportErrorKind::Protocol)
                })?;
            let mut builder = client.get(endpoint);
            for header in request.headers() {
                builder = builder.header(header.name(), header.value().expose());
            }
            let response = builder
                .send()
                .await
                .map_err(classify_billing_reqwest_error)?;
            if !response.status().is_success() {
                return Err(classify_billing_status(response).await);
            }
            if !is_json_response(&response) {
                return Err(GrokBillingTransportError::new(
                    GrokBillingTransportErrorKind::Protocol,
                ));
            }
            let body = match collect_bounded(response, MAX_GROK_BILLING_BYTES).await {
                Ok(BoundedBody::Body(body)) => body,
                Ok(BoundedBody::TooLarge) => {
                    return Err(GrokBillingTransportError::new(
                        GrokBillingTransportErrorKind::Protocol,
                    ));
                }
                Err(error) => return Err(classify_billing_reqwest_error(error)),
            };
            Ok(GrokBillingTransportResponse::new(body))
        })
    }
}

fn build_official_client(
    dns_policy: GrokDnsResolutionPolicy,
    timeout: Option<Duration>,
) -> Result<Client, GrokReqwestTransportBuildError> {
    let mut builder = Client::builder()
        .redirect(Policy::none())
        .no_proxy()
        .connect_timeout(CONNECT_TIMEOUT)
        .pool_idle_timeout(POOL_IDLE_TIMEOUT)
        .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST)
        .http2_adaptive_window(true)
        .http2_keep_alive_interval(HTTP2_KEEP_ALIVE_INTERVAL)
        .http2_keep_alive_timeout(HTTP2_KEEP_ALIVE_TIMEOUT)
        .http2_keep_alive_while_idle(true)
        .tcp_nodelay(true)
        .https_only(true)
        .dns_resolver(Arc::new(StrictDnsResolver::new(dns_policy)?));
    if let Some(timeout) = timeout {
        builder = builder.timeout(timeout);
    }
    builder
        .build()
        .map_err(|_| GrokReqwestTransportBuildError::ClientInitialization)
}

fn valid_official_url(url: &Url, host: &str, path: Option<&str>) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some(host)
        && url.port_or_known_default() == Some(443)
        && path.is_none_or(|path| url.path() == path)
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
}

fn valid_billing_url(url: &Url, host: &str) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some(host)
        && url.port_or_known_default() == Some(443)
        && url.path() == OFFICIAL_BILLING_PATH
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && url.query_pairs().count() == 1
        && url
            .query_pairs()
            .next()
            .is_some_and(|(key, value)| key == "format" && value == "credits")
}

#[derive(Debug)]
struct StrictDnsResolver {
    policy: GrokDnsResolutionPolicy,
    trusted_doh: TrustedDohResolver,
}

#[derive(Debug, Default)]
struct RequestDnsObserver {
    observation: OnceLock<GrokInferenceDnsObservation>,
}

impl RequestDnsObserver {
    fn record(&self, source: GrokInferenceDnsSource, duration: Duration) {
        let _ = self.observation.set(GrokInferenceDnsObservation::new(
            source,
            elapsed_millis(duration),
        ));
    }

    fn observation(&self) -> Option<GrokInferenceDnsObservation> {
        self.observation.get().copied()
    }
}

impl StrictDnsResolver {
    fn new(policy: GrokDnsResolutionPolicy) -> Result<Self, GrokReqwestTransportBuildError> {
        Ok(Self {
            policy,
            trusted_doh: TrustedDohResolver::new()?,
        })
    }
}

impl Resolve for StrictDnsResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let requested_host = name.as_str().to_owned();
        if self
            .policy
            .plan_system_resolution(&requested_host, &[])
            .is_err()
        {
            return Box::pin(async { Err(safe_dns_error("DNS host is not allowlisted")) });
        }
        let policy = self.policy;
        let trusted_doh = self.trusted_doh.clone();
        let observer = REQUEST_DNS_OBSERVER.try_with(Arc::clone).ok();
        Box::pin(async move {
            let started_at = Instant::now();
            let system_addresses = match tokio::net::lookup_host((requested_host.as_str(), 0)).await
            {
                Ok(addresses) => addresses.collect::<Vec<_>>(),
                Err(_) => Vec::new(),
            };
            let system_ips = system_addresses
                .iter()
                .map(SocketAddr::ip)
                .collect::<Vec<_>>();
            let (source, result) = match policy
                .plan_system_resolution(&requested_host, &system_ips)
                .map_err(|_| safe_dns_error("DNS resolution rejected"))?
            {
                GrokDnsResolutionPlan::System => (
                    GrokInferenceDnsSource::System,
                    Ok(Box::new(system_addresses.into_iter()) as Addrs),
                ),
                GrokDnsResolutionPlan::TrustedDoh => {
                    let result = match trusted_doh.resolve(&requested_host).await {
                        Ok(addresses) => policy
                            .validate_trusted_doh_resolution(&requested_host, &addresses)
                            .map_err(|_| safe_dns_error("trusted DNS result rejected"))
                            .map(|()| {
                                Box::new(
                                    addresses
                                        .into_iter()
                                        .map(|address| SocketAddr::new(address, 0)),
                                ) as Addrs
                            }),
                        Err(error) => Err(error),
                    };
                    (GrokInferenceDnsSource::TrustedDoh, result)
                }
            };
            if let Some(observer) = observer {
                observer.record(source, started_at.elapsed());
            }
            result
        })
    }
}

fn elapsed_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[derive(Clone)]
struct TrustedDohResolver {
    client: Client,
}

impl TrustedDohResolver {
    fn new() -> Result<Self, GrokReqwestTransportBuildError> {
        let client = Client::builder()
            .redirect(Policy::none())
            .no_proxy()
            .https_only(true)
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(OAUTH_REQUEST_TIMEOUT)
            .pool_idle_timeout(POOL_IDLE_TIMEOUT)
            .tcp_nodelay(true)
            .resolve_to_addrs(TRUSTED_DOH_HOST, &TRUSTED_DOH_BOOTSTRAP)
            .build()
            .map_err(|_| GrokReqwestTransportBuildError::ClientInitialization)?;
        Ok(Self { client })
    }

    async fn resolve(
        &self,
        requested_host: &str,
    ) -> Result<Vec<IpAddr>, Box<dyn std::error::Error + Send + Sync>> {
        let response = self
            .client
            .get(TRUSTED_DOH_URL)
            .query(&[("name", requested_host), ("type", "A")])
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|_| safe_dns_error("trusted DNS request failed"))?;
        if !response.status().is_success() || !is_json_response(&response) {
            return Err(safe_dns_error("trusted DNS response rejected"));
        }
        let body = match collect_bounded(response, MAX_DOH_RESPONSE_BYTES)
            .await
            .map_err(|_| safe_dns_error("trusted DNS response failed"))?
        {
            BoundedBody::Body(body) => body,
            BoundedBody::TooLarge => {
                return Err(safe_dns_error("trusted DNS response too large"));
            }
        };
        let response: TrustedDohResponse = serde_json::from_slice(&body)
            .map_err(|_| safe_dns_error("trusted DNS response malformed"))?;
        if response.status != 0 {
            return Err(safe_dns_error("trusted DNS lookup failed"));
        }
        let addresses = response
            .answers
            .into_iter()
            .filter(|answer| matches!(answer.record_type, DNS_RECORD_A | DNS_RECORD_AAAA))
            .map(|answer| {
                answer
                    .data
                    .parse::<IpAddr>()
                    .map_err(|_| safe_dns_error("trusted DNS address malformed"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(addresses)
    }
}

impl fmt::Debug for TrustedDohResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedDohResolver")
            .field("client", &"reqwest::Client")
            .field("bootstrap", &"[PINNED]")
            .finish()
    }
}

#[derive(Deserialize)]
struct TrustedDohResponse {
    #[serde(rename = "Status")]
    status: u16,
    #[serde(rename = "Answer", default)]
    answers: Vec<TrustedDohAnswer>,
}

#[derive(Deserialize)]
struct TrustedDohAnswer {
    #[serde(rename = "type")]
    record_type: u16,
    data: String,
}

fn safe_dns_error(message: &'static str) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(io::Error::other(message))
}

fn is_public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_public_ipv4(address),
        IpAddr::V6(address) => is_public_ipv6(address),
    }
}

fn is_public_ipv4(address: Ipv4Addr) -> bool {
    let [a, b, c, _] = address.octets();
    !(a == 0
        || a == 10
        || a == 127
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 192 && b == 168)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || a >= 224)
}

fn is_public_ipv6(address: Ipv6Addr) -> bool {
    let segments = address.segments();
    (segments[0] & 0xe000) == 0x2000
        && !(segments[0] == 0x2001 && segments[1] == 0x0db8)
        && !(segments[0] == 0x2001 && segments[1] == 0x0002)
}

fn classify_oauth_reqwest_error(error: reqwest::Error) -> TransportFailure {
    let kind = if error.is_builder() || error.is_connect() {
        TransportFailureKind::NotSent
    } else if error.is_timeout() {
        TransportFailureKind::Timeout
    } else {
        TransportFailureKind::Ambiguous
    };
    TransportFailure::new(kind)
}

fn classify_inference_reqwest_error(error: reqwest::Error) -> GrokInferenceTransportError {
    let (kind, send_state) = if error.is_builder() {
        (
            GrokInferenceTransportErrorKind::Protocol,
            UpstreamSendState::NotSent,
        )
    } else if error.is_connect() {
        (
            GrokInferenceTransportErrorKind::Transport,
            UpstreamSendState::NotSent,
        )
    } else if error.is_timeout() {
        (
            GrokInferenceTransportErrorKind::Timeout,
            UpstreamSendState::Ambiguous,
        )
    } else {
        (
            GrokInferenceTransportErrorKind::Transport,
            UpstreamSendState::Ambiguous,
        )
    };
    GrokInferenceTransportError::new(kind, send_state)
}

fn classify_model_catalog_reqwest_error(error: reqwest::Error) -> GrokModelCatalogTransportError {
    let kind = if error.is_builder() {
        GrokModelCatalogTransportErrorKind::Protocol
    } else if error.is_timeout() {
        GrokModelCatalogTransportErrorKind::Timeout
    } else {
        GrokModelCatalogTransportErrorKind::Transport
    };
    GrokModelCatalogTransportError::new(kind)
}

fn classify_billing_reqwest_error(error: reqwest::Error) -> GrokBillingTransportError {
    let kind = if error.is_builder() {
        GrokBillingTransportErrorKind::Protocol
    } else if error.is_timeout() {
        GrokBillingTransportErrorKind::Timeout
    } else {
        GrokBillingTransportErrorKind::Transport
    };
    GrokBillingTransportError::new(kind)
}

fn classify_inference_stream_error(error: &reqwest::Error) -> GrokInferenceTransportError {
    GrokInferenceTransportError::new(
        if error.is_timeout() {
            GrokInferenceTransportErrorKind::Timeout
        } else {
            GrokInferenceTransportErrorKind::Transport
        },
        UpstreamSendState::Sent,
    )
}

async fn classify_inference_status(response: Response) -> GrokInferenceTransportError {
    let status = response.status();
    let retry_after = retry_after(&response);
    let http_version = upstream_http_version(response.version());
    let request_id = upstream_request_id(&response);
    let status_code = status.as_u16();
    let body = match collect_bounded(response, MAX_ERROR_BODY_BYTES).await {
        Ok(BoundedBody::Body(body)) => body,
        Ok(BoundedBody::TooLarge) | Err(_) => Vec::new(),
    };
    let metadata = inference_error_metadata(&body);
    let credential_recovery_required = status == StatusCode::UNAUTHORIZED
        || (status == StatusCode::FORBIDDEN && forbidden_requires_recovery(&metadata));
    let kind = match status {
        StatusCode::BAD_REQUEST
        | StatusCode::NOT_FOUND
        | StatusCode::CONFLICT
        | StatusCode::UNPROCESSABLE_ENTITY => GrokInferenceTransportErrorKind::InvalidRequest,
        StatusCode::UNAUTHORIZED => GrokInferenceTransportErrorKind::Unauthorized,
        StatusCode::PAYMENT_REQUIRED => GrokInferenceTransportErrorKind::QuotaExhausted,
        StatusCode::FORBIDDEN => classify_forbidden(&metadata),
        StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => {
            GrokInferenceTransportErrorKind::Timeout
        }
        StatusCode::TOO_MANY_REQUESTS => {
            if quota_exhausted(&metadata) {
                GrokInferenceTransportErrorKind::QuotaExhausted
            } else {
                GrokInferenceTransportErrorKind::RateLimited
            }
        }
        status if status.is_server_error() => GrokInferenceTransportErrorKind::Unavailable,
        _ => GrokInferenceTransportErrorKind::Protocol,
    };
    let mut error = GrokInferenceTransportError::new(kind, UpstreamSendState::Sent)
        .with_status(status_code)
        .with_response_facts(http_version, request_id)
        .redact_sensitive_context("upstream response body");
    let upstream_code = if status == StatusCode::BAD_REQUEST && reasoning_decode_failed(&metadata) {
        Some("reasoning_decode_failed".to_owned())
    } else {
        metadata.code.as_deref().and_then(normalize_failure_code)
    };
    if let Some(code) = upstream_code.and_then(|code| SafeUpstreamValue::new(code).ok()) {
        error = error.with_upstream_code(code);
    }
    if credential_recovery_required {
        error = error.with_credential_recovery();
    }
    if let Some(retry_after) = retry_after {
        error = error.with_retry_after(retry_after);
    }
    error
}

#[derive(Default)]
struct InferenceErrorMetadata {
    code: Option<String>,
    error_type: Option<String>,
    message: Option<String>,
}

fn inference_error_metadata(body: &[u8]) -> InferenceErrorMetadata {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        let message = String::from_utf8_lossy(body).trim().to_owned();
        return InferenceErrorMetadata {
            message: (!message.is_empty()).then_some(message),
            ..InferenceErrorMetadata::default()
        };
    };
    let Some(root) = value.as_object() else {
        return InferenceErrorMetadata::default();
    };
    let nested = root.get("error").and_then(Value::as_object);
    InferenceErrorMetadata {
        code: nested
            .and_then(|error| first_string(error, &["code", "error_code"]))
            .or_else(|| first_string(root, &["code", "error_code"])),
        error_type: nested
            .and_then(|error| first_string(error, &["type", "error_type"]))
            .or_else(|| first_string(root, &["type", "error_type"])),
        message: nested
            .and_then(|error| first_string(error, &["message", "error"]))
            .or_else(|| first_string(root, &["message", "error"])),
    }
}

fn first_string(object: &serde_json::Map<String, Value>, fields: &[&str]) -> Option<String> {
    fields
        .iter()
        .find_map(|field| object.get(*field).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn classify_forbidden(metadata: &InferenceErrorMetadata) -> GrokInferenceTransportErrorKind {
    let text = inference_metadata_text(metadata);
    if credential_rejected(&text) {
        GrokInferenceTransportErrorKind::Unauthorized
    } else if quota_exhausted(metadata) {
        GrokInferenceTransportErrorKind::QuotaExhausted
    } else {
        GrokInferenceTransportErrorKind::PermissionDenied
    }
}

fn forbidden_requires_recovery(metadata: &InferenceErrorMetadata) -> bool {
    let text = inference_metadata_text(metadata);
    credential_rejected(&text)
        || text.contains("access to the chat endpoint is denied")
        || text.trim_matches([' ', '.', '!', '\t', '\r', '\n']) == "access denied"
}

fn reasoning_decode_failed(metadata: &InferenceErrorMetadata) -> bool {
    metadata.message.as_deref().is_some_and(|message| {
        message.contains("could not decode the compaction blob")
            || message.contains("could not decrypt the provided encrypted_content")
    })
}

fn quota_exhausted(metadata: &InferenceErrorMetadata) -> bool {
    contains_any(
        &inference_metadata_text(metadata),
        &[
            "subscription:free-usage-exhausted",
            "subscription_free_usage_exhausted",
            "free-usage-exhausted",
            "free_usage_exhausted",
            "used all the included free usage",
            "used all your free usage",
            "personal-team-blocked:spending-limit",
            "personal_team_blocked_spending_limit",
        ],
    )
}

fn inference_metadata_text(metadata: &InferenceErrorMetadata) -> String {
    [
        metadata.code.as_deref().unwrap_or_default(),
        metadata.error_type.as_deref().unwrap_or_default(),
        metadata.message.as_deref().unwrap_or_default(),
    ]
    .join(" ")
    .to_ascii_lowercase()
}

fn credential_rejected(value: &str) -> bool {
    contains_any(
        value,
        &[
            "authentication",
            "unauthorized",
            "invalid token",
            "token expired",
        ],
    )
}

fn contains_any(value: &str, signals: &[&str]) -> bool {
    signals.iter().any(|signal| value.contains(signal))
}

fn normalize_failure_code(value: &str) -> Option<String> {
    let mut normalized = String::with_capacity(value.len().min(48));
    for character in value.trim().to_ascii_lowercase().chars() {
        if character.is_ascii_alphanumeric() {
            normalized.push(character);
        } else if matches!(character, '-' | '_' | '.' | ':') {
            normalized.push('_');
        }
        if normalized.len() >= 48 {
            break;
        }
    }
    let normalized = normalized.trim_matches('_');
    (!normalized.is_empty()).then(|| normalized.to_owned())
}

fn upstream_http_version(version: reqwest::Version) -> UpstreamHttpVersion {
    match version {
        reqwest::Version::HTTP_09 => UpstreamHttpVersion::Http09,
        reqwest::Version::HTTP_10 => UpstreamHttpVersion::Http10,
        reqwest::Version::HTTP_11 => UpstreamHttpVersion::Http11,
        reqwest::Version::HTTP_2 => UpstreamHttpVersion::Http2,
        reqwest::Version::HTTP_3 => UpstreamHttpVersion::Http3,
        _ => UpstreamHttpVersion::Unknown,
    }
}

fn upstream_request_id(response: &Response) -> Option<SafeUpstreamValue> {
    ["x-request-id", "request-id", "cf-ray"]
        .into_iter()
        .find_map(|name| response.headers().get(name))
        .and_then(|value| value.to_str().ok())
        .and_then(|value| SafeUpstreamValue::new(value.to_owned()).ok())
}

async fn classify_model_catalog_status(response: Response) -> GrokModelCatalogTransportError {
    let status = response.status();
    let kind = match status {
        StatusCode::UNAUTHORIZED => GrokModelCatalogTransportErrorKind::Unauthorized,
        StatusCode::FORBIDDEN => GrokModelCatalogTransportErrorKind::PermissionDenied,
        StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => {
            GrokModelCatalogTransportErrorKind::Timeout
        }
        StatusCode::TOO_MANY_REQUESTS => GrokModelCatalogTransportErrorKind::RateLimited,
        status if status.is_server_error() => GrokModelCatalogTransportErrorKind::Unavailable,
        _ => GrokModelCatalogTransportErrorKind::Protocol,
    };
    let status = status.as_u16();
    let _ = collect_bounded(response, MAX_ERROR_BODY_BYTES).await;
    GrokModelCatalogTransportError::new(kind).with_status(status)
}

async fn classify_billing_status(response: Response) -> GrokBillingTransportError {
    let status = response.status();
    let kind = match status {
        StatusCode::UNAUTHORIZED => GrokBillingTransportErrorKind::Unauthorized,
        StatusCode::FORBIDDEN | StatusCode::PAYMENT_REQUIRED => {
            GrokBillingTransportErrorKind::PermissionDenied
        }
        StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => {
            GrokBillingTransportErrorKind::Timeout
        }
        StatusCode::TOO_MANY_REQUESTS => GrokBillingTransportErrorKind::RateLimited,
        status if status.is_server_error() => GrokBillingTransportErrorKind::Unavailable,
        _ => GrokBillingTransportErrorKind::Protocol,
    };
    let status = status.as_u16();
    let _ = collect_bounded(response, MAX_ERROR_BODY_BYTES).await;
    GrokBillingTransportError::new(kind).with_status(status)
}

fn retry_after(response: &Response) -> Option<Duration> {
    response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| (1..=MAX_RETRY_AFTER_SECONDS).contains(seconds))
        .map(Duration::from_secs)
}

fn is_json_response(response: &Response) -> bool {
    response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
}

pub(crate) enum BoundedBody {
    Body(Vec<u8>),
    TooLarge,
}

pub(crate) async fn collect_bounded(
    response: Response,
    max_bytes: usize,
) -> Result<BoundedBody, reqwest::Error> {
    if response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Ok(BoundedBody::TooLarge);
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let Some(next_len) = body.len().checked_add(chunk.len()) else {
            return Ok(BoundedBody::TooLarge);
        };
        if next_len > max_bytes {
            return Ok(BoundedBody::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(BoundedBody::Body(body))
}
