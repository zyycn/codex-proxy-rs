//! 请求上下文中间件。

use std::net::{IpAddr, SocketAddr};

use axum::{
    extract::{connect_info::ConnectInfo, Request, State},
    http::{HeaderMap, HeaderValue},
    middleware::Next,
    response::Response,
};
use ipnet::IpNet;
use uuid::Uuid;

/// 请求 ID 头。
pub const REQUEST_ID_HEADER: &str = "x-request-id";
const MAX_REQUEST_ID_LEN: usize = 128;
const REAL_IP_HEADER: &str = "x-real-ip";
const X_FORWARDED_FOR_HEADER: &str = "x-forwarded-for";

/// 可信反向代理配置。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustedProxyConfig {
    networks: Vec<IpNet>,
}

impl TrustedProxyConfig {
    /// 从 IP 或 CIDR 字符串构造可信代理配置。
    pub fn from_entries(entries: &[String]) -> Result<Self, TrustedProxyConfigError> {
        let networks = entries
            .iter()
            .map(|entry| parse_trusted_proxy_entry(entry))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { networks })
    }

    fn is_trusted(&self, ip: IpAddr) -> bool {
        self.networks.iter().any(|network| network.contains(&ip))
    }
}

/// 可信代理配置错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TrustedProxyConfigError {
    /// 配置项不是 IP 地址或 CIDR。
    #[error("invalid trusted proxy entry `{entry}`; expected an IP address or CIDR network")]
    InvalidEntry { entry: String },
}

/// 请求 ID。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestId(String);

impl RequestId {
    /// 生成新的请求 ID。
    pub fn generate() -> Self {
        Self(format!("req_{}", Uuid::new_v4()))
    }

    /// 从 HTTP 头解析请求 ID。
    pub fn from_header(value: &HeaderValue) -> Option<Self> {
        let value = value.to_str().ok()?.trim();
        if !valid_request_id(value) {
            return None;
        }
        Some(Self(value.to_string()))
    }

    /// 返回字符串形式。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 由连接来源和可信代理链解析出的客户端 IP。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientIp(String);

impl ClientIp {
    /// 返回 IP 字符串。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn valid_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REQUEST_ID_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

/// 为请求附加请求 ID，并在响应头中回写。
pub async fn attach_request_id(mut request: Request, next: Next) -> Response {
    attach_request_context(&TrustedProxyConfig::default(), &mut request);
    run_with_request_id(request, next).await
}

/// 为请求附加请求上下文，并按可信代理配置解析真实客户端 IP。
pub async fn attach_request_id_with_proxy_config(
    State(trusted_proxies): State<TrustedProxyConfig>,
    mut request: Request,
    next: Next,
) -> Response {
    attach_request_context(&trusted_proxies, &mut request);
    run_with_request_id(request, next).await
}

async fn run_with_request_id(mut request: Request, next: Next) -> Response {
    let request_id = request
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(RequestId::from_header)
        .unwrap_or_else(RequestId::generate);

    request.extensions_mut().insert(request_id.clone());
    let mut response = next.run(request).await;
    if let Ok(value) = HeaderValue::from_str(request_id.as_str()) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
    response
}

fn attach_request_context(trusted_proxies: &TrustedProxyConfig, request: &mut Request) {
    attach_client_ip_from_connection(trusted_proxies, request);
}

fn attach_client_ip_from_connection(trusted_proxies: &TrustedProxyConfig, request: &mut Request) {
    let peer_ip = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    let client_ip =
        peer_ip.map(|peer_ip| client_ip_from_peer(peer_ip, request.headers(), trusted_proxies));

    let Some(client_ip) = client_ip else {
        return;
    };
    request.extensions_mut().insert(ClientIp(client_ip));
}

fn client_ip_from_peer(
    peer_ip: IpAddr,
    headers: &HeaderMap,
    trusted_proxies: &TrustedProxyConfig,
) -> String {
    if trusted_proxies.is_trusted(peer_ip) {
        trusted_forwarded_client_ip(headers).unwrap_or_else(|| peer_ip.to_string())
    } else {
        peer_ip.to_string()
    }
}

fn trusted_forwarded_client_ip(headers: &HeaderMap) -> Option<String> {
    first_forwarded_for_value(headers).or_else(|| header_string(headers, REAL_IP_HEADER))
}

fn header_string(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn first_forwarded_for_value(headers: &HeaderMap) -> Option<String> {
    let value = header_string(headers, X_FORWARDED_FOR_HEADER)?;
    value
        .split(',')
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn parse_trusted_proxy_entry(entry: &str) -> Result<IpNet, TrustedProxyConfigError> {
    let entry = entry.trim();
    if entry.is_empty() {
        return Err(invalid_trusted_proxy_entry(entry));
    }
    if let Ok(network) = entry.parse::<IpNet>() {
        return Ok(network);
    }
    let ip = entry
        .parse::<IpAddr>()
        .map_err(|_| invalid_trusted_proxy_entry(entry))?;
    IpNet::new(
        ip,
        match ip {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        },
    )
    .map_err(|_| invalid_trusted_proxy_entry(entry))
}

fn invalid_trusted_proxy_entry(entry: &str) -> TrustedProxyConfigError {
    TrustedProxyConfigError::InvalidEntry {
        entry: entry.to_string(),
    }
}
