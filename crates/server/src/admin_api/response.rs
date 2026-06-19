//! 管理端响应封装。

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use codex_proxy_platform::json::Page;
use serde::Serialize;

/// 管理端响应信封。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminEnvelope<T> {
    /// 业务状态码。
    pub code: u32,
    /// 响应消息。
    pub message: String,
    /// 响应数据。
    pub data: T,
    /// 请求 ID。
    pub request_id: String,
}

impl<T> AdminEnvelope<T> {
    /// 构造响应信封。
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

    /// 构造成功响应信封。
    pub fn ok(data: T, request_id: impl Into<String>) -> Self {
        Self::new(200, "OK", data, request_id)
    }
}

/// 分页元数据。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageMeta {
    /// 当前页限制。
    pub limit: u32,
    /// 下一页游标。
    pub next_cursor: Option<String>,
}

/// 管理端分页响应信封。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminPageEnvelope<T> {
    /// 业务状态码。
    pub code: u32,
    /// 响应消息。
    pub message: String,
    /// 当前页数据。
    pub data: Vec<T>,
    /// 分页元数据。
    pub page: PageMeta,
    /// 请求 ID。
    pub request_id: String,
}

impl<T> AdminPageEnvelope<T> {
    /// 构造分页响应信封。
    pub fn new(
        code: u32,
        message: impl Into<String>,
        page: Page<T>,
        limit: u32,
        request_id: impl Into<String>,
    ) -> Self {
        let Page { items, next_cursor } = page;
        Self {
            code,
            message: message.into(),
            data: items,
            page: PageMeta { limit, next_cursor },
            request_id: request_id.into(),
        }
    }

    /// 构造成功分页响应信封。
    pub fn ok(page: Page<T>, limit: u32, request_id: impl Into<String>) -> Self {
        Self::new(200, "OK", page, limit, request_id)
    }
}

/// 管理端响应。
#[derive(Debug, Clone)]
pub struct AdminResponse<T> {
    /// HTTP 状态码。
    pub status: StatusCode,
    /// 响应体。
    pub body: T,
}

impl<T> AdminResponse<T> {
    /// 构造管理端响应。
    pub fn new(status: StatusCode, body: T) -> Self {
        Self { status, body }
    }
}

/// 管理端错误。
#[derive(Debug, Clone)]
pub struct AdminError {
    /// HTTP 状态码。
    pub status: StatusCode,
    /// 业务状态码。
    pub code: u32,
    /// 错误消息。
    pub message: String,
    /// 请求 ID。
    pub request_id: String,
}

impl AdminError {
    /// 构造管理端错误。
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
        AdminResponse::new(
            self.status,
            AdminEnvelope::new(self.code, self.message, (), self.request_id),
        )
        .into_response()
    }
}

impl<T> IntoResponse for AdminResponse<T>
where
    T: Serialize,
{
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}
