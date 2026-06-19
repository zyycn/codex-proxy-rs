//! Codex 上游适配器。

pub mod client;
pub mod fingerprint;
pub mod models;
pub mod websocket;

pub use client::{
    endpoint_request_path, endpoint_url, primary_usage_request_path, usage_endpoint_urls,
};

/// 构造默认 Codex HTTP 客户端。
pub fn default_http_client() -> client::HttpClient {
    client::HttpClient::new()
}
