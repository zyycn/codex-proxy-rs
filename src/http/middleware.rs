use axum::{extract::Request, http::HeaderValue, middleware::Next, response::Response};
use uuid::Uuid;

pub const REQUEST_ID_HEADER: &str = "x-request-id";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestId(String);

impl RequestId {
    pub fn generate() -> Self {
        Self(format!("req_{}", Uuid::new_v4()))
    }

    pub fn from_header(value: &HeaderValue) -> Option<Self> {
        let value = value.to_str().ok()?.trim();
        if value.is_empty() {
            return None;
        }
        Some(Self(value.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub async fn attach_request_id(mut request: Request, next: Next) -> Response {
    let request_id = request
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(RequestId::from_header)
        .unwrap_or_else(RequestId::generate);

    // 中文注释：同一个 requestId 同时进入日志、响应头和 admin body，方便跨层排查。
    request.extensions_mut().insert(request_id.clone());
    let mut response = next.run(request).await;
    if let Ok(value) = HeaderValue::from_str(request_id.as_str()) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
    response
}
