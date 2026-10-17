//! HTTP 入口快照与统一来源地址解析

use axum::{
    extract::ConnectInfo,
    http::{HeaderMap, Request, header::USER_AGENT},
};
use gateway_core::engine::{extensions::RequestContext, middleware::MiddlewareHeader};
use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

pub(crate) fn capture<B>(request: &Request<B>) -> Arc<RequestContext> {
    let peer_addr = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0);
    let (client_ip, _) = request_client_context(request.headers(), peer_addr);
    Arc::new(RequestContext {
        method: request.method().to_string(),
        uri: request.uri().to_string(),
        http_version: format!("{:?}", request.version()),
        headers: request
            .headers()
            .iter()
            .map(|(name, value)| {
                MiddlewareHeader::new(
                    name.as_str(),
                    bytes::Bytes::copy_from_slice(value.as_bytes()),
                )
            })
            .collect(),
        peer_addr,
        client_ip,
    })
}

/// 保留宿主的来源地址解析顺序，插件仍可从入口快照自行解释转发头
pub(crate) fn request_client_context(
    headers: &HeaderMap,
    peer_address: Option<SocketAddr>,
) -> (Option<IpAddr>, Option<String>) {
    let client_ip = ["cf-connecting-ip", "x-real-ip"]
        .into_iter()
        .find_map(|name| header_ip(headers, name))
        .or_else(|| forwarded_client_ip(headers))
        .or_else(|| peer_address.map(|address| address.ip()));
    let user_agent = headers
        .get(USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    (client_ip, user_agent)
}

fn header_ip(headers: &HeaderMap, name: &str) -> Option<IpAddr> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .and_then(|value| value.parse().ok())
}

fn forwarded_client_ip(headers: &HeaderMap) -> Option<IpAddr> {
    let addresses = headers
        .get("x-forwarded-for")?
        .to_str()
        .ok()?
        .split(',')
        .filter_map(|value| value.trim().parse::<IpAddr>().ok())
        .collect::<Vec<_>>();
    addresses
        .iter()
        .copied()
        .find(|address| !is_private_or_loopback(*address))
        .or_else(|| addresses.first().copied())
}

const fn is_private_or_loopback(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => address.is_private() || address.is_loopback(),
        IpAddr::V6(address) => address.is_unique_local() || address.is_loopback(),
    }
}
