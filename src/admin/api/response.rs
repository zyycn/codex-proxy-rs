use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

use crate::utils::pagination::Page;

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
        // Body codes drive frontend branches; HTTP status remains the transport truth.
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageMeta {
    pub limit: u32,
    pub next_cursor: Option<String>,
}

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

    pub fn ok(page: Page<T>, limit: u32, request_id: impl Into<String>) -> Self {
        Self::new(200, "OK", page, limit, request_id)
    }
}

#[derive(Debug, Clone)]
pub struct AdminResponse<T> {
    pub status: StatusCode,
    pub body: T,
}

impl<T> AdminResponse<T> {
    pub fn new(status: StatusCode, body: T) -> Self {
        Self { status, body }
    }
}

#[derive(Debug, Clone)]
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
