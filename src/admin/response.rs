//! 管理端响应封装与 From 转换。

use crate::infra::json::{total_pages, NumberedPage, Page};
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
}

impl AdminError {
    pub fn new(
        status: StatusCode,
        code: u32,
        message: impl Into<String>,
        _request_id: impl Into<String>,
    ) -> Self {
        Self {
            status,
            code,
            message: message.into(),
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
}

impl<T> AdminEnvelope<T> {
    pub fn new(
        code: u32,
        message: impl Into<String>,
        data: T,
        _request_id: impl Into<String>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            data,
        }
    }
    pub fn ok(data: T, request_id: impl Into<String>) -> Self {
        Self::new(200, "OK", data, request_id)
    }
}

/// 分页元数据。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorPageMeta {
    pub limit: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// 页码分页元数据。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NumberedPageMeta {
    pub page: u32,
    pub page_size: u32,
    pub total: u64,
    pub total_pages: u32,
}

/// 分页元数据。
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum PageMeta {
    Cursor(CursorPageMeta),
    Numbered(NumberedPageMeta),
}

/// 分页响应信封。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminPageEnvelope<T> {
    pub code: u32,
    pub message: String,
    pub data: PageData<T>,
}

/// 分页响应数据。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageData<T> {
    pub items: Vec<T>,
    pub page: PageMeta,
}

impl<T> AdminPageEnvelope<T> {
    pub fn ok(page: Page<T>, limit: u32, _request_id: impl Into<String>) -> Self {
        let Page { items, next_cursor } = page;
        Self {
            code: 200,
            message: "OK".into(),
            data: PageData {
                items,
                page: PageMeta::Cursor(CursorPageMeta { limit, next_cursor }),
            },
        }
    }

    pub fn numbered(page: NumberedPage<T>, _request_id: impl Into<String>) -> Self {
        let NumberedPage {
            items,
            total,
            page,
            page_size,
        } = page;
        Self {
            code: 200,
            message: "OK".into(),
            data: PageData {
                items,
                page: PageMeta::Numbered(NumberedPageMeta {
                    page,
                    page_size,
                    total,
                    total_pages: total_pages(total, page_size),
                }),
            },
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
