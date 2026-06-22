//! 请求 ID 中间件。

use axum::{extract::Request, http::HeaderValue, middleware::Next, response::Response};
use uuid::Uuid;

/// 请求 ID 头。
pub const REQUEST_ID_HEADER: &str = "x-request-id";

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
        if value.is_empty() {
            return None;
        }
        Some(Self(value.to_string()))
    }

    /// 返回字符串形式。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 为请求附加请求 ID，并在响应头中回写。
pub async fn attach_request_id(mut request: Request, next: Next) -> Response {
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
