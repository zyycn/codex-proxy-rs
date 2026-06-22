//! 管理端响应封装与 From 转换。

use crate::infra::json::Page;
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

/// 管理端错误。
pub struct AdminError {
    pub status: StatusCode,
    pub code: u32,
    pub message: String,
    pub request_id: String,
}

impl AdminError {
    pub fn new(
        status: StatusCode,
        code: u32,
        message: impl Into<String>,
        request_id: impl Into<String>,
    ) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            request_id: request_id.into(),
        }
    }
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({
                "code": self.code,
                "message": self.message,
                "data": null,
                "requestId": self.request_id,
            })),
        )
            .into_response()
    }
}

/// 管理端响应信封。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminEnvelope<T> {
    pub code: u32,
    pub message: String,
    pub data: T,
    pub request_id: String,
}

impl<T> AdminEnvelope<T> {
    pub fn new(
        code: u32,
        message: impl Into<String>,
        data: T,
        request_id: impl Into<String>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            data,
            request_id: request_id.into(),
        }
    }
    pub fn ok(data: T, request_id: impl Into<String>) -> Self {
        Self::new(200, "OK", data, request_id)
    }
}

/// 分页元数据。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageMeta {
    pub limit: u32,
    pub next_cursor: Option<String>,
}

/// 分页响应信封。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminPageEnvelope<T> {
    pub code: u32,
    pub message: String,
    pub data: Vec<T>,
    pub page: PageMeta,
    pub request_id: String,
}

impl<T> AdminPageEnvelope<T> {
    pub fn ok(page: Page<T>, limit: u32, request_id: impl Into<String>) -> Self {
        let Page { items, next_cursor } = page;
        Self {
            code: 200,
            message: "OK".into(),
            data: items,
            page: PageMeta { limit, next_cursor },
            request_id: request_id.into(),
        }
    }
}

/// 管理端响应。
pub struct AdminResponse<T: Serialize> {
    pub status: StatusCode,
    pub body: T,
}

impl<T: Serialize> AdminResponse<T> {
    pub fn new(status: StatusCode, body: T) -> Self {
        Self { status, body }
    }
}

impl<T: Serialize> IntoResponse for AdminResponse<T> {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

/// Helper to create JSON success response.
pub fn ok_json<T: Serialize>(data: T) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(serde_json::json!({"code": 200, "message": "OK", "data": data})),
    )
}

/// Helper to create JSON error response.
pub fn err_json(status: StatusCode, code: u32, message: &str) -> impl IntoResponse {
    (
        status,
        Json(serde_json::json!({"code": code, "message": message})),
    )
}

/// Helper to create JSON page response.
pub fn ok_page<T: Serialize>(page: Page<T>, limit: u32) -> impl IntoResponse {
    let Page { items, next_cursor } = page;
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "code": 200,
            "message": "OK",
            "data": items,
            "page": { "limit": limit, "nextCursor": next_cursor }
        })),
    )
}
