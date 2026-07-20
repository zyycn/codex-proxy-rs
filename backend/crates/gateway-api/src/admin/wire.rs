//! 管理端公共 wire、响应信封与脱敏错误。

use std::fmt;

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

/// 管理端 wire 字段校验错误；只携带稳定字段名，不回显输入值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WireValidationError {
    field: &'static str,
}

impl WireValidationError {
    /// 构造字段校验错误。
    #[must_use]
    pub const fn new(field: &'static str) -> Self {
        Self { field }
    }

    /// 返回未通过校验的字段名。
    #[must_use]
    pub const fn field(self) -> &'static str {
        self.field
    }
}

/// 成功响应的稳定业务码。
pub const ADMIN_OK_CODE: u32 = 200;

/// 成功响应的稳定消息。
pub const ADMIN_OK_MESSAGE: &str = "OK";

/// 页码分页元数据。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageMeta {
    page: u32,
    page_size: u32,
    total: u64,
    total_pages: u32,
}

impl PageMeta {
    /// 由分页 owner 已经校验并计算的事实构造 wire metadata。
    #[must_use]
    pub const fn new(page: u32, page_size: u32, total: u64, total_pages: u32) -> Self {
        Self {
            page,
            page_size,
            total,
            total_pages,
        }
    }

    /// 当前页，从 1 开始。
    #[must_use]
    pub const fn page(self) -> u32 {
        self.page
    }

    /// 每页数量。
    #[must_use]
    pub const fn page_size(self) -> u32 {
        self.page_size
    }

    /// 全部记录数量。
    #[must_use]
    pub const fn total(self) -> u64 {
        self.total
    }

    /// 总页数。
    #[must_use]
    pub const fn total_pages(self) -> u32 {
        self.total_pages
    }
}

/// 管理端稳定业务错误码。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AdminErrorCode(u32);

impl AdminErrorCode {
    /// JSON 解析失败。
    pub const MALFORMED_JSON: Self = Self(40000);
    /// 请求参数或状态不合法。
    pub const BAD_REQUEST: Self = Self(40001);
    /// 时间范围不合法。
    pub const INVALID_TIME_RANGE: Self = Self(40002);
    /// 模型来源不合法。
    pub const INVALID_MODEL_SOURCE: Self = Self(40003);
    /// 缺少管理员会话。
    pub const SESSION_REQUIRED: Self = Self(40101);
    /// 管理员登录凭据错误。
    pub const INVALID_CREDENTIALS: Self = Self(40102);
    /// 管理 API Key 错误。
    pub const INVALID_API_KEY: Self = Self(40103);
    /// 资源不存在。
    pub const NOT_FOUND: Self = Self(40401);
    /// 配置 revision 或资源状态冲突。
    pub const CONFLICT: Self = Self(40901);
    /// 管理员登录尝试过多。
    pub const TOO_MANY_LOGIN_ATTEMPTS: Self = Self(42901);
    /// 设置持久化失败。
    pub const SETTINGS_PERSIST: Self = Self(50000);
    /// 未分类内部错误。
    pub const INTERNAL: Self = Self(50001);
    /// 用量记录的账号投影失败。
    pub const USAGE_RECORD_ACCOUNTS: Self = Self(50002);
    /// 上游网关失败。
    pub const BAD_GATEWAY: Self = Self(50201);
    /// 依赖的服务暂不可用。
    pub const SERVICE_UNAVAILABLE: Self = Self(50301);

    /// 返回用于 JSON wire contract 的数值。
    #[must_use]
    pub const fn value(self) -> u32 {
        self.0
    }
}

/// 管理端错误响应正文。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminErrorBody {
    code: AdminErrorCode,
    message: String,
    data: (),
}

impl AdminErrorBody {
    /// 构造不携带业务数据的管理端错误正文。
    #[must_use]
    pub fn new(code: AdminErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: (),
        }
    }

    /// 稳定业务错误码。
    #[must_use]
    pub const fn code(&self) -> AdminErrorCode {
        self.code
    }

    /// 安全的客户端错误消息。
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// 管理端 HTTP 错误。
pub struct AdminError {
    status: StatusCode,
    body: AdminErrorBody,
}

impl AdminError {
    fn new(status: StatusCode, code: AdminErrorCode, message: impl Into<String>) -> Self {
        Self {
            status,
            body: AdminErrorBody::new(code, message),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            AdminErrorCode::BAD_REQUEST,
            message,
        )
    }

    pub fn admin_session_required() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            AdminErrorCode::SESSION_REQUIRED,
            "Admin session required",
        )
    }

    pub fn invalid_admin_credentials() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            AdminErrorCode::INVALID_CREDENTIALS,
            "Invalid admin credentials",
        )
    }

    pub fn invalid_admin_api_key() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            AdminErrorCode::INVALID_API_KEY,
            "Invalid admin API key",
        )
    }

    pub fn too_many_login_attempts() -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            AdminErrorCode::TOO_MANY_LOGIN_ATTEMPTS,
            "Too many admin login attempts",
        )
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, AdminErrorCode::CONFLICT, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, AdminErrorCode::NOT_FOUND, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminErrorCode::INTERNAL,
            message,
        )
    }

    pub fn bad_gateway(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::BAD_GATEWAY,
            AdminErrorCode::BAD_GATEWAY,
            message,
        )
    }

    pub fn service_unavailable(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            AdminErrorCode::SERVICE_UNAVAILABLE,
            message,
        )
    }
}

/// 把管理用例的稳定错误分类映射到既有 HTTP 错误 contract。
pub(crate) fn map_admin_service_error(
    error: gateway_admin::model::AdminError,
    unavailable_message: &'static str,
) -> AdminError {
    use gateway_admin::model::AdminErrorKind;

    match error.kind() {
        AdminErrorKind::Invalid => AdminError::bad_request(error.to_string()),
        AdminErrorKind::Unauthorized => AdminError::admin_session_required(),
        AdminErrorKind::NotFound => AdminError::not_found(error.to_string()),
        AdminErrorKind::Conflict => AdminError::conflict(error.to_string()),
        AdminErrorKind::RateLimited => AdminError::too_many_login_attempts(),
        AdminErrorKind::BadGateway => AdminError::bad_gateway(error.to_string()),
        AdminErrorKind::Unavailable => AdminError::service_unavailable(unavailable_message),
        AdminErrorKind::Internal => AdminError::internal(error.to_string()),
    }
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

impl fmt::Display for AdminError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.body.message())
    }
}

/// 带独立 HTTP 状态的管理端响应。
pub struct AdminResponse<T: Serialize> {
    status: StatusCode,
    body: T,
}

impl<T: Serialize> AdminResponse<T> {
    #[must_use]
    pub fn new(status: StatusCode, body: T) -> Self {
        Self { status, body }
    }
}

impl<T: Serialize> IntoResponse for AdminResponse<T> {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

/// 管理端响应信封。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminEnvelope<T> {
    code: u32,
    message: String,
    data: T,
}

impl<T> AdminEnvelope<T> {
    /// 构造稳定的成功响应。
    #[must_use]
    pub fn ok(data: T) -> Self {
        Self {
            code: ADMIN_OK_CODE,
            message: ADMIN_OK_MESSAGE.to_owned(),
            data,
        }
    }

    /// 稳定业务码。
    #[must_use]
    pub const fn code(&self) -> u32 {
        self.code
    }

    /// 稳定业务消息。
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// 响应数据。
    #[must_use]
    pub const fn data(&self) -> &T {
        &self.data
    }

    /// 取出响应数据。
    #[must_use]
    pub fn into_data(self) -> T {
        self.data
    }
}

/// 页码分页响应数据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminPageData<T> {
    items: Vec<T>,
    page: PageMeta,
}

impl<T> AdminPageData<T> {
    /// 组合查询 owner 返回的当前页数据与分页事实。
    #[must_use]
    pub const fn new(items: Vec<T>, page: PageMeta) -> Self {
        Self { items, page }
    }

    /// 当前页记录。
    #[must_use]
    pub fn items(&self) -> &[T] {
        &self.items
    }

    /// 分页元数据。
    #[must_use]
    pub const fn page(&self) -> PageMeta {
        self.page
    }

    /// 拆分分页数据。
    #[must_use]
    pub fn into_parts(self) -> (Vec<T>, PageMeta) {
        (self.items, self.page)
    }
}
