//! 请求上下文中间件。

use std::net::{IpAddr, SocketAddr};
use std::sync::LazyLock;

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
const CF_CONNECTING_IP_HEADER: &str = "cf-connecting-ip";
const REAL_IP_HEADER: &str = "x-real-ip";
const X_FORWARDED_FOR_HEADER: &str = "x-forwarded-for";

/// 私有 / 回环 / 唯一本地地址网段，用于在自动模式下从 `X-Forwarded-For` 中跳过内网跳板 IP。
static PRIVATE_NETWORKS: LazyLock<Vec<IpNet>> = LazyLock::new(|| {
    [
        "10.0.0.0/8",
        "172.16.0.0/12",
        "192.168.0.0/16",
        "127.0.0.0/8",
        "::1/128",
        "fc00::/7",
    ]
    .iter()
    .filter_map(|entry| entry.parse().ok())
    .collect()
});

fn is_private_ip(ip: IpAddr) -> bool {
    PRIVATE_NETWORKS.iter().any(|network| network.contains(&ip))
}

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

    /// 是否未配置任何可信代理（决定是否启用自动真实 IP 探测）。
    fn is_empty(&self) -> bool {
        self.networks.is_empty()
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
    if trusted_proxies.is_empty() {
        // 未配置可信代理：自动模式，按 CF-Connecting-IP → X-Real-IP →
        // X-Forwarded-For（首个公网 IP）→ peer IP 的优先级解析真实客户端 IP。
        return auto_forwarded_client_ip(headers).unwrap_or_else(|| peer_ip.to_string());
    }
    if trusted_proxies.is_trusted(peer_ip) {
        // 已配置可信代理且直连来源可信：只采信转发头。
        trusted_forwarded_client_ip(headers).unwrap_or_else(|| peer_ip.to_string())
    } else {
        // 已配置可信代理但直连来源不可信：忽略转发头，使用 peer IP。
        peer_ip.to_string()
    }
}

/// 自动模式下解析转发头中的真实客户端 IP。
///
/// 未配置可信代理时使用，对齐常见反代（Cloudflare / Nginx）默认行为：
/// `CF-Connecting-IP` → `X-Real-IP` → `X-Forwarded-For` 首个公网 IP。
fn auto_forwarded_client_ip(headers: &HeaderMap) -> Option<String> {
    if let Some(cf_ip) = header_string(headers, CF_CONNECTING_IP_HEADER) {
        return Some(cf_ip);
    }
    if let Some(real_ip) = header_string(headers, REAL_IP_HEADER) {
        return Some(real_ip);
    }
    forwarded_for_public_ip(headers).or_else(|| first_forwarded_for_value(headers))
}

/// 返回 `X-Forwarded-For` 链中第一个公网 IP（跳过内网跳板）。
fn forwarded_for_public_ip(headers: &HeaderMap) -> Option<String> {
    let value = header_string(headers, X_FORWARDED_FOR_HEADER)?;
    value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .find(|entry| match entry.parse::<IpAddr>() {
            Ok(ip) => !is_private_ip(ip),
            Err(_) => false,
        })
        .map(ToString::to_string)
}

fn trusted_forwarded_client_ip(headers: &HeaderMap) -> Option<String> {
    header_string(headers, CF_CONNECTING_IP_HEADER)
        .or_else(|| header_string(headers, REAL_IP_HEADER))
        .or_else(|| forwarded_for_public_ip(headers))
        .or_else(|| first_forwarded_for_value(headers))
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
    if let Ok(network) = entry.parse::<IpNet>() {
        return Ok(network);
    }
    entry
        .parse::<IpAddr>()
        .map(IpNet::from)
        .map_err(|_| invalid_trusted_proxy_entry(entry))
}

fn invalid_trusted_proxy_entry(entry: &str) -> TrustedProxyConfigError {
    TrustedProxyConfigError::InvalidEntry {
        entry: entry.to_string(),
    }
}
