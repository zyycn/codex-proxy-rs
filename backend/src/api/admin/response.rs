//! 管理端响应封装与 From 转换。

use crate::infra::json::{NumberedPage, total_pages};
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

pub(crate) const ADMIN_OK_CODE: u32 = 200;
pub(crate) const ADMIN_OK_MESSAGE: &str = "OK";
const ADMIN_MALFORMED_JSON_CODE: u32 = 40000;
const ADMIN_BAD_REQUEST_CODE: u32 = 40001;
const ADMIN_INVALID_TIME_RANGE_CODE: u32 = 40002;
const ADMIN_INVALID_MODEL_SOURCE_CODE: u32 = 40003;
const ADMIN_INVALID_ENDPOINT_SOURCE_CODE: u32 = 40004;
const ADMIN_SESSION_REQUIRED_CODE: u32 = 40101;
const ADMIN_INVALID_CREDENTIALS_CODE: u32 = 40102;
const ADMIN_INVALID_API_KEY_CODE: u32 = 40103;
const ADMIN_TOO_MANY_LOGIN_ATTEMPTS_CODE: u32 = 42901;
const ADMIN_NOT_FOUND_CODE: u32 = 40401;
const ADMIN_CONFLICT_CODE: u32 = 40901;
const ADMIN_SETTINGS_PERSIST_CODE: u32 = 50000;
const ADMIN_INTERNAL_CODE: u32 = 50001;
const ADMIN_USAGE_RECORD_ACCOUNTS_CODE: u32 = 50002;
const ADMIN_BAD_GATEWAY_CODE: u32 = 50201;

/// 管理端错误。
pub struct AdminError {
    status: StatusCode,
    code: u32,
    message: String,
}

impl AdminError {
    fn new(status: StatusCode, code: u32, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, ADMIN_BAD_REQUEST_CODE, message)
    }

    pub fn malformed_json(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, ADMIN_MALFORMED_JSON_CODE, message)
    }

    pub fn invalid_time_range(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            ADMIN_INVALID_TIME_RANGE_CODE,
            message,
        )
    }

    pub fn invalid_model_source(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            ADMIN_INVALID_MODEL_SOURCE_CODE,
            message,
        )
    }

    pub fn invalid_endpoint_source(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            ADMIN_INVALID_ENDPOINT_SOURCE_CODE,
            message,
        )
    }

    pub fn admin_session_required() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            ADMIN_SESSION_REQUIRED_CODE,
            "Admin session required",
        )
    }

    pub fn invalid_admin_credentials() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            ADMIN_INVALID_CREDENTIALS_CODE,
            "Invalid admin credentials",
        )
    }

    pub fn invalid_admin_api_key() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            ADMIN_INVALID_API_KEY_CODE,
            "Invalid admin API key",
        )
    }

    pub fn too_many_login_attempts() -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            ADMIN_TOO_MANY_LOGIN_ATTEMPTS_CODE,
            "Too many admin login attempts",
        )
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, ADMIN_NOT_FOUND_CODE, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, ADMIN_CONFLICT_CODE, message)
    }

    pub fn settings_persist_failed(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            ADMIN_SETTINGS_PERSIST_CODE,
            message,
        )
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            ADMIN_INTERNAL_CODE,
            message,
        )
    }

    pub fn usage_record_accounts_failed(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            ADMIN_USAGE_RECORD_ACCOUNTS_CODE,
            message,
        )
    }

    pub fn bad_gateway(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_GATEWAY, ADMIN_BAD_GATEWAY_CODE, message)
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

impl fmt::Display for AdminError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
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
    fn new(code: u32, message: impl Into<String>, data: T) -> Self {
        Self {
            code,
            message: message.into(),
            data,
        }
    }
    pub fn ok(data: T) -> Self {
        Self::new(ADMIN_OK_CODE, ADMIN_OK_MESSAGE, data)
    }
}

/// 页码分页元数据。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PageMeta {
    pub(crate) page: u32,
    pub(crate) page_size: u32,
    pub(crate) total: u64,
    pub(crate) total_pages: u32,
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

/// 批量删除响应数据。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BatchDeleteData {
    pub(crate) deleted: u32,
    pub(crate) not_found: Vec<String>,
}

impl<T> AdminPageEnvelope<T> {
    pub fn ok(page: NumberedPage<T>) -> Self {
        let NumberedPage {
            items,
            total,
            page,
            page_size,
        } = page;
        Self {
            code: ADMIN_OK_CODE,
            message: ADMIN_OK_MESSAGE.into(),
            data: PageData {
                items,
                page: PageMeta {
                    page,
                    page_size,
                    total,
                    total_pages: total_pages(total, page_size),
                },
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EditableUpdateRequest {
    id: String,
    label: Option<Option<String>>,
    status: Option<String>,
}

pub(crate) struct ParsedEditableUpdate {
    pub id: String,
    pub label: Option<Option<String>>,
    pub status: Option<String>,
}

pub(crate) struct EditableUpdateMessages<'a> {
    pub object_required: &'a str,
    pub invalid: &'a str,
    pub empty_update: &'a str,
    pub unknown_field_editable: bool,
}

pub(crate) fn parse_editable_update(
    payload: &Value,
    messages: EditableUpdateMessages<'_>,
) -> Result<ParsedEditableUpdate, AdminError> {
    let object = payload
        .as_object()
        .ok_or_else(|| AdminError::bad_request(messages.object_required))?;
    if messages.unknown_field_editable {
        for field in object.keys() {
            if !matches!(field.as_str(), "id" | "label" | "status") {
                return Err(AdminError::bad_request(format!("{field} is not editable")));
            }
        }
    }

    let update = serde_json::from_value::<EditableUpdateRequest>(payload.clone())
        .map_err(|_| AdminError::bad_request(messages.invalid))?;
    if is_blank(&update.id) || update.status.as_deref().is_some_and(is_blank) {
        return Err(AdminError::bad_request(messages.invalid));
    }
    if update.label.is_none() && update.status.is_none() {
        return Err(AdminError::bad_request(messages.empty_update));
    }

    Ok(ParsedEditableUpdate {
        id: update.id,
        label: update.label,
        status: update.status,
    })
}

fn is_blank(value: &str) -> bool {
    value.trim().is_empty()
}
