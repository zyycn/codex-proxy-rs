//! 模型数据面的客户端入口、鉴权与协议交付

pub mod auth;
pub mod body;
mod chat_completions;
pub mod error;
pub(crate) mod genai;
pub mod generation;
pub mod images;
pub(crate) mod live;
mod messages;
pub(crate) mod middleware;
pub mod models;
mod openai_endpoints;
mod peer_link;
mod provider_endpoints;
pub mod responses;
pub mod router;
pub mod search;
pub(crate) mod service;
mod usage;

/// 客户端可读的 ID 必须能按现有请求记录检索；不以无关入口 ID 补位
pub(crate) fn with_model_request_id(
    mut response: axum::response::Response,
    request_id: &gateway_core::engine::ModelRequestId,
) -> axum::response::Response {
    use axum::http::HeaderValue;

    let Ok(gateway_id) = HeaderValue::from_str(request_id.as_str()) else {
        return response;
    };
    response
        .extensions_mut()
        .insert(crate::client_error::ExecutionObserved);
    let headers = response.headers_mut();
    let usable = |value: &HeaderValue| value.to_str().is_ok_and(|id| !id.trim().is_empty());
    if !headers.get("x-request-id").is_some_and(usable) {
        // 只有别名时沿用同一个上游值，避免外层 middleware 再填入入口 ID
        let client_id = headers
            .get("x-oai-request-id")
            .filter(|value| usable(value))
            .unwrap_or(&gateway_id)
            .clone();
        headers.insert("x-request-id", client_id);
    }
    headers.insert("x-gateway-request-id", gateway_id);
    response
}
