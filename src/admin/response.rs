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
    status: StatusCode,
    code: u32,
    message: String,
}

impl AdminError {
    pub fn new(status: StatusCode, code: u32, message: impl Into<String>) -> Self {
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
    code: u32,
    message: String,
    data: T,
}

impl<T> AdminEnvelope<T> {
    pub fn new(code: u32, message: impl Into<String>, data: T) -> Self {
        Self {
            code,
            message: message.into(),
            data,
        }
    }
    pub fn ok(data: T) -> Self {
        Self::new(200, "OK", data)
    }
}

/// 分页元数据。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CursorPageMeta {
    pub(crate) limit: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) next_cursor: Option<String>,
}

/// 页码分页元数据。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NumberedPageMeta {
    pub(crate) page: u32,
    pub(crate) page_size: u32,
    pub(crate) total: u64,
    pub(crate) total_pages: u32,
}

/// 分页元数据。
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub(crate) enum PageMeta {
    Cursor(CursorPageMeta),
    Numbered(NumberedPageMeta),
}

/// 分页响应信封。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminPageEnvelope<T> {
    code: u32,
    message: String,
    data: PageData<T>,
}

/// 分页响应数据。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PageData<T> {
    items: Vec<T>,
    page: PageMeta,
}

impl<T> AdminPageEnvelope<T> {
    pub fn ok(page: Page<T>, limit: u32) -> Self {
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

    pub fn numbered(page: NumberedPage<T>) -> Self {
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
    status: StatusCode,
    body: T,
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
