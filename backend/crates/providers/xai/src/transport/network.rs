//! 关闭 redirect、proxy 与业务重试的生产 reqwest transport。

use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE, ETAG, RETRY_AFTER};
use reqwest::redirect::Policy;
use reqwest::{Client, Response, StatusCode};
use serde::Deserialize;
use url::Url;

use crate::credential::discovery::MAX_OAUTH_RESPONSE_BYTES;
use crate::{
    GrokBillingRequest, GrokBillingTransport, GrokBillingTransportError,
    GrokBillingTransportErrorKind, GrokBillingTransportFuture, GrokBillingTransportResponse,
    GrokInferenceRequest, GrokInferenceResponse, GrokInferenceTransport,
    GrokInferenceTransportError, GrokInferenceTransportErrorKind, GrokInferenceTransportFuture,
    GrokModelCatalogRequest, GrokModelCatalogTransport, GrokModelCatalogTransportError,
    GrokModelCatalogTransportErrorKind, GrokModelCatalogTransportFuture,
    GrokModelCatalogTransportResponse, HttpMethod, MAX_GROK_BILLING_BYTES,
    MAX_GROK_MODEL_CATALOG_BYTES, OAuthHttpRequest, OAuthHttpResponse, OAuthHttpTransport,
    TransportFailure, TransportFailureKind, TransportFuture,
};
use gateway_core::engine::UpstreamSendState;
use gateway_protocol::openai::sse::MAX_SSE_EVENT_BUFFER_BYTES;

pub(crate) const OFFICIAL_OAUTH_HOST: &str = "auth.x.ai";
const OFFICIAL_INFERENCE_HOST: &str = "cli-chat-proxy.grok.com";
const OFFICIAL_INFERENCE_PATH: &str = "/v1/responses";
const OFFICIAL_MODEL_CATALOG_PATH: &str = "/v1/models";
const OFFICIAL_BILLING_PATH: &str = "/v1/billing";
pub(crate) const OFFICIAL_JWKS_PATH: &str = "/.well-known/jwks.json";
pub(crate) const OFFICIAL_USERINFO_PATH: &str = "/oauth2/userinfo";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const OAUTH_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
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

/// 构建严格 reqwest transport 失败。
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum GrokReqwestTransportBuildError {
    /// Reqwest TLS/client 初始化失败。
    #[error("Grok reqwest transport initialization failed")]
    ClientInitialization,
    /// Injected origin 不是数字 loopback HTTP origin。
    #[error("Grok injected transport origin is invalid")]
    InvalidEndpointOrigin,
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

#[derive(Debug, Clone)]
enum EndpointPolicyKind {
    Official { host: &'static str },
    Loopback { origin: LoopbackOrigin },
}

/// Auditable endpoint policy injected into reqwest transports and OIDC verification.
#[derive(Debug, Clone)]
pub struct GrokEndpointPolicy(EndpointPolicyKind);

impl GrokEndpointPolicy {
    /// Restricts requests to the official xAI OAuth origin.
    #[must_use]
    pub const fn official_oauth() -> Self {
        Self(EndpointPolicyKind::Official {
            host: OFFICIAL_OAUTH_HOST,
        })
    }

    /// Restricts requests to the official Grok CLI inference origin.
    #[must_use]
    pub const fn official_inference() -> Self {
        Self(EndpointPolicyKind::Official {
            host: OFFICIAL_INFERENCE_HOST,
        })
    }

    /// Creates an explicitly injected numeric-loopback HTTP origin policy.
    ///
    /// # Errors
    ///
    /// Returns an error for domains, non-loopback addresses, missing ports, or
    /// origins containing credentials, query, fragment, or a non-root path.
    pub fn loopback(origin: &Url) -> Result<Self, GrokReqwestTransportBuildError> {
        Ok(Self(EndpointPolicyKind::Loopback {
            origin: LoopbackOrigin::parse(origin)?,
        }))
    }

    /// Applies the same public-address predicate used by the strict DNS resolver.
    #[must_use]
    pub fn accepts_resolved_address(address: IpAddr) -> bool {
        is_public_ip(address)
    }

    fn validate_oauth(&self, url: &Url) -> bool {
        match &self.0 {
            EndpointPolicyKind::Official { host } => valid_official_url(url, host, None),
            EndpointPolicyKind::Loopback { origin } => origin.matches(url),
        }
    }

    fn validate_inference(&self, url: &Url) -> bool {
        match &self.0 {
            EndpointPolicyKind::Official { host } => {
                valid_official_url(url, host, Some(OFFICIAL_INFERENCE_PATH))
            }
            EndpointPolicyKind::Loopback { origin } => origin.matches(url),
        }
    }

    fn validate_model_catalog(&self, url: &Url) -> bool {
        match &self.0 {
            EndpointPolicyKind::Official { host } => {
                valid_official_url(url, host, Some(OFFICIAL_MODEL_CATALOG_PATH))
            }
            EndpointPolicyKind::Loopback { origin } => {
                origin.matches(url)
                    && url.path() == OFFICIAL_MODEL_CATALOG_PATH
                    && url.query().is_none()
                    && url.fragment().is_none()
            }
        }
    }

    fn validate_billing(&self, url: &Url) -> bool {
        let valid_query = url.query_pairs().count() == 1
            && url
                .query_pairs()
                .next()
                .is_some_and(|(key, value)| key == "format" && value == "credits");
        if !valid_query || url.fragment().is_some() {
            return false;
        }
        match &self.0 {
            EndpointPolicyKind::Official { host } => {
                url.scheme() == "https"
                    && url.host_str() == Some(*host)
                    && url.port_or_known_default() == Some(443)
                    && url.path() == OFFICIAL_BILLING_PATH
                    && url.username().is_empty()
                    && url.password().is_none()
            }
            EndpointPolicyKind::Loopback { origin } => {
                origin.matches(url) && url.path() == OFFICIAL_BILLING_PATH
            }
        }
    }

    pub(crate) fn validate_jwks(&self, url: &Url) -> bool {
        self.validate_exact_oauth_path(url, OFFICIAL_JWKS_PATH)
    }

    pub(crate) fn validate_userinfo(&self, url: &Url) -> bool {
        self.validate_exact_oauth_path(url, OFFICIAL_USERINFO_PATH)
    }

    fn validate_exact_oauth_path(&self, url: &Url, path: &str) -> bool {
        match &self.0 {
            EndpointPolicyKind::Official { host } => valid_official_url(url, host, Some(path)),
            EndpointPolicyKind::Loopback { origin } => {
                origin.matches(url)
                    && url.path() == path
                    && url.query().is_none()
                    && url.fragment().is_none()
            }
        }
    }
}

#[derive(Debug, Clone)]
struct LoopbackOrigin {
    host: IpAddr,
    port: u16,
}

/// 官方 OAuth HTTP transport。只允许 `auth.x.ai:443`。
pub struct ReqwestOAuthTransport {
    client: Client,
    endpoint_policy: GrokEndpointPolicy,
}

impl ReqwestOAuthTransport {
    /// 使用系统原生根证书构建生产 transport。
    ///
    /// # Errors
    ///
    /// TLS client 初始化失败时返回错误。
    pub fn new() -> Result<Self, GrokReqwestTransportBuildError> {
        let endpoint_policy = GrokEndpointPolicy::official_oauth();
        Self::with_endpoint_policy(endpoint_policy)
    }

    /// Builds an OAuth transport with an explicit endpoint policy.
    ///
    /// # Errors
    ///
    /// Returns an error when the reqwest client cannot be initialized.
    pub fn with_endpoint_policy(
        endpoint_policy: GrokEndpointPolicy,
    ) -> Result<Self, GrokReqwestTransportBuildError> {
        let client = build_client(&endpoint_policy, Some(OAUTH_REQUEST_TIMEOUT))?;
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
    client: Client,
    endpoint_policy: GrokEndpointPolicy,
}

impl ReqwestGrokInferenceTransport {
    /// 构建只允许官方 CLI proxy 的生产 transport。
    ///
    /// # Errors
    ///
    /// TLS client 初始化失败时返回错误。
    pub fn new() -> Result<Self, GrokReqwestTransportBuildError> {
        let endpoint_policy = GrokEndpointPolicy::official_inference();
        Self::with_endpoint_policy(endpoint_policy)
    }

    /// Builds an inference transport with an explicit endpoint policy.
    ///
    /// # Errors
    ///
    /// Returns an error when the reqwest client cannot be initialized.
    pub fn with_endpoint_policy(
        endpoint_policy: GrokEndpointPolicy,
    ) -> Result<Self, GrokReqwestTransportBuildError> {
        let client = build_client(&endpoint_policy, None)?;
        Ok(Self {
            client,
            endpoint_policy,
        })
    }
}

impl fmt::Debug for ReqwestGrokInferenceTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReqwestGrokInferenceTransport")
            .field("client", &"reqwest::Client")
            .field("endpoint_policy", &self.endpoint_policy)
            .finish()
    }
}

impl GrokInferenceTransport for ReqwestGrokInferenceTransport {
    fn execute(&self, request: GrokInferenceRequest) -> GrokInferenceTransportFuture<'_> {
        let client = self.client.clone();
        let endpoint_policy = self.endpoint_policy.clone();
        Box::pin(async move {
            if !endpoint_policy.validate_inference(request.endpoint()) {
                return Err(GrokInferenceTransportError::new(
                    GrokInferenceTransportErrorKind::Protocol,
                    UpstreamSendState::NotSent,
                ));
            }
            let mut builder = client
                .post(request.endpoint().clone())
                .body(request.body().to_vec());
            for header in request.headers() {
                builder = builder.header(header.name(), header.value().expose());
            }
            let response = builder
                .send()
                .await
                .map_err(classify_inference_reqwest_error)?;
            if !response.status().is_success() {
                return Err(classify_inference_status(response).await);
            }
            if !is_event_stream(&response) {
                return Err(GrokInferenceTransportError::new(
                    GrokInferenceTransportErrorKind::Protocol,
                    UpstreamSendState::Sent,
                ));
            }
            let body = response.bytes_stream().map(|chunk| match chunk {
                Ok(chunk) if chunk.len() <= MAX_SSE_EVENT_BUFFER_BYTES => Ok(chunk.to_vec()),
                Ok(_) => Err(GrokInferenceTransportError::new(
                    GrokInferenceTransportErrorKind::Protocol,
                    UpstreamSendState::Sent,
                )),
                Err(error) => Err(classify_inference_stream_error(&error)),
            });
            Ok(GrokInferenceResponse::new(Box::pin(body)))
        })
    }
}

/// 官方 Grok CLI proxy 模型目录 GET transport。
pub struct ReqwestGrokModelCatalogTransport {
    client: Client,
    endpoint_policy: GrokEndpointPolicy,
}

impl ReqwestGrokModelCatalogTransport {
    /// 构建只允许官方 CLI proxy `/v1/models` 的生产 transport。
    ///
    /// # Errors
    ///
    /// TLS client 初始化失败时返回错误。
    pub fn new() -> Result<Self, GrokReqwestTransportBuildError> {
        Self::with_endpoint_policy(GrokEndpointPolicy::official_inference())
    }

    /// 使用显式 endpoint policy 构建测试或生产 transport。
    ///
    /// # Errors
    ///
    /// Reqwest client 无法初始化时返回错误。
    pub fn with_endpoint_policy(
        endpoint_policy: GrokEndpointPolicy,
    ) -> Result<Self, GrokReqwestTransportBuildError> {
        let client = build_client(&endpoint_policy, Some(OAUTH_REQUEST_TIMEOUT))?;
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
            if !endpoint_policy.validate_billing(request.endpoint()) {
                return Err(GrokBillingTransportError::new(
                    GrokBillingTransportErrorKind::Protocol,
                ));
            }
            let mut builder = client.get(request.endpoint().clone());
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

pub(crate) fn build_client(
    policy: &GrokEndpointPolicy,
    timeout: Option<Duration>,
) -> Result<Client, GrokReqwestTransportBuildError> {
    let mut builder = Client::builder()
        .redirect(Policy::none())
        .no_proxy()
        .connect_timeout(CONNECT_TIMEOUT)
        .pool_idle_timeout(POOL_IDLE_TIMEOUT)
        .tcp_nodelay(true);
    match &policy.0 {
        EndpointPolicyKind::Official { host } => {
            let resolver = StrictDnsResolver::new(host)?;
            builder = builder.https_only(true).dns_resolver(Arc::new(resolver));
        }
        EndpointPolicyKind::Loopback { .. } => {
            builder = builder.https_only(false);
        }
    }
    if let Some(timeout) = timeout {
        builder = builder.timeout(timeout);
    }
    builder
        .build()
        .map_err(|_| GrokReqwestTransportBuildError::ClientInitialization)
}

impl LoopbackOrigin {
    fn parse(url: &Url) -> Result<Self, GrokReqwestTransportBuildError> {
        let host = url
            .host()
            .and_then(|host| match host {
                url::Host::Ipv4(address) => Some(IpAddr::V4(address)),
                url::Host::Ipv6(address) => Some(IpAddr::V6(address)),
                url::Host::Domain(_) => None,
            })
            .filter(IpAddr::is_loopback)
            .ok_or(GrokReqwestTransportBuildError::InvalidEndpointOrigin)?;
        if url.scheme() != "http"
            || url.path() != "/"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(GrokReqwestTransportBuildError::InvalidEndpointOrigin);
        }
        let port = url
            .port()
            .ok_or(GrokReqwestTransportBuildError::InvalidEndpointOrigin)?;
        Ok(Self { host, port })
    }

    fn matches(&self, url: &Url) -> bool {
        url.scheme() == "http"
            && url.port() == Some(self.port)
            && url.host().is_some_and(|host| match host {
                url::Host::Ipv4(address) => IpAddr::V4(address) == self.host,
                url::Host::Ipv6(address) => IpAddr::V6(address) == self.host,
                url::Host::Domain(_) => false,
            })
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
    }
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

#[derive(Debug)]
struct StrictDnsResolver {
    policy: GrokDnsResolutionPolicy,
    trusted_doh: TrustedDohResolver,
}

impl StrictDnsResolver {
    fn new(allowed_host: &'static str) -> Result<Self, GrokReqwestTransportBuildError> {
        Ok(Self {
            policy: GrokDnsResolutionPolicy { allowed_host },
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
        Box::pin(async move {
            let system_addresses = match tokio::net::lookup_host((requested_host.as_str(), 0)).await
            {
                Ok(addresses) => addresses.collect::<Vec<_>>(),
                Err(_) => Vec::new(),
            };
            let system_ips = system_addresses
                .iter()
                .map(SocketAddr::ip)
                .collect::<Vec<_>>();
            match policy
                .plan_system_resolution(&requested_host, &system_ips)
                .map_err(|_| safe_dns_error("DNS resolution rejected"))?
            {
                GrokDnsResolutionPlan::System => {
                    Ok(Box::new(system_addresses.into_iter()) as Addrs)
                }
                GrokDnsResolutionPlan::TrustedDoh => {
                    let addresses = trusted_doh.resolve(&requested_host).await?;
                    policy
                        .validate_trusted_doh_resolution(&requested_host, &addresses)
                        .map_err(|_| safe_dns_error("trusted DNS result rejected"))?;
                    Ok(Box::new(
                        addresses
                            .into_iter()
                            .map(|address| SocketAddr::new(address, 0)),
                    ) as Addrs)
                }
            }
        })
    }
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
    let kind = match status {
        StatusCode::BAD_REQUEST
        | StatusCode::NOT_FOUND
        | StatusCode::CONFLICT
        | StatusCode::UNPROCESSABLE_ENTITY => GrokInferenceTransportErrorKind::InvalidRequest,
        StatusCode::UNAUTHORIZED => GrokInferenceTransportErrorKind::Unauthorized,
        StatusCode::PAYMENT_REQUIRED => GrokInferenceTransportErrorKind::QuotaExhausted,
        StatusCode::FORBIDDEN => GrokInferenceTransportErrorKind::PermissionDenied,
        StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => {
            GrokInferenceTransportErrorKind::Timeout
        }
        StatusCode::TOO_MANY_REQUESTS => GrokInferenceTransportErrorKind::RateLimited,
        status if status.is_server_error() => GrokInferenceTransportErrorKind::Unavailable,
        _ => GrokInferenceTransportErrorKind::Protocol,
    };
    let status = status.as_u16();
    let _ = collect_bounded(response, MAX_ERROR_BODY_BYTES).await;
    let mut error = GrokInferenceTransportError::new(kind, UpstreamSendState::Sent)
        .with_status(status)
        .redact_sensitive_context("upstream response body");
    if let Some(retry_after) = retry_after {
        error = error.with_retry_after(retry_after);
    }
    error
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

fn is_event_stream(response: &Response) -> bool {
    response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"))
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
