//! 请求上下文中间件。

use std::net::{IpAddr, SocketAddr};
use std::sync::LazyLock;

use axum::{
    extract::{Request, connect_info::ConnectInfo},
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

/// 私有 / 回环 / 唯一本地地址网段，用于从 `X-Forwarded-For` 中跳过内网跳板 IP。
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

/// 由连接来源和转发头解析出的客户端 IP。
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
    attach_request_context(&mut request);
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

fn attach_request_context(request: &mut Request) {
    attach_client_ip_from_connection(request);
}

fn attach_client_ip_from_connection(request: &mut Request) {
    let peer_ip = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    let client_ip = peer_ip.map(|peer_ip| client_ip_from_peer(peer_ip, request.headers()));

    let Some(client_ip) = client_ip else {
        return;
    };
    request.extensions_mut().insert(ClientIp(client_ip));
}

fn client_ip_from_peer(peer_ip: IpAddr, headers: &HeaderMap) -> String {
    auto_forwarded_client_ip(headers).unwrap_or_else(|| peer_ip.to_string())
}

/// 解析转发头中的真实客户端 IP。
///
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
