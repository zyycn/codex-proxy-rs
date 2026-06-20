//! Codex 上游适配器。

pub mod client;
pub mod fingerprint;
pub mod websocket;

pub use client::{
    endpoint_request_path, endpoint_url, primary_usage_request_path, usage_endpoint_urls,
};
