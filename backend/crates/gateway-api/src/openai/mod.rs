//! OpenAI 客户端协议 adapter。

pub mod auth;
mod endpoint;
pub mod error;
pub mod images;
pub mod models;
pub mod responses;
pub mod router;
pub mod search;
pub(crate) mod service;

/// 客户端可读的 ID 必须能按现有请求记录检索；不以无关入口 ID 补位。
fn with_model_request_id(
    mut response: axum::response::Response,
    request_id: &gateway_core::engine::ModelRequestId,
) -> axum::response::Response {
    use axum::http::HeaderValue;

    let Ok(gateway_id) = HeaderValue::from_str(request_id.as_str()) else {
        return response;
    };
    let headers = response.headers_mut();
    let usable = |value: &HeaderValue| value.to_str().is_ok_and(|id| !id.trim().is_empty());
    if !headers.get("x-request-id").is_some_and(usable) {
        // 只有别名时沿用同一个上游值，避免外层 middleware 再填入入口 ID。
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
