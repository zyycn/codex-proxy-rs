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
    /// 不可逆上游操作的执行结果未知。
    pub const UPSTREAM_RESULT_UNKNOWN: Self = Self(50202);
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

#[derive(Debug, Clone, Copy)]
struct AdminErrorSpec {
    status: StatusCode,
    code: AdminErrorCode,
    message: &'static str,
}

impl AdminErrorSpec {
    const fn new(status: StatusCode, code: AdminErrorCode, message: &'static str) -> Self {
        Self {
            status,
            code,
            message,
        }
    }
}

const MALFORMED_JSON: AdminErrorSpec = AdminErrorSpec::new(
    StatusCode::BAD_REQUEST,
    AdminErrorCode::MALFORMED_JSON,
    "请求体不是合法 JSON",
);
const BAD_REQUEST: AdminErrorSpec = AdminErrorSpec::new(
    StatusCode::BAD_REQUEST,
    AdminErrorCode::BAD_REQUEST,
    "请求参数不合法",
);
const INVALID_TIME_RANGE: AdminErrorSpec = AdminErrorSpec::new(
    StatusCode::BAD_REQUEST,
    AdminErrorCode::INVALID_TIME_RANGE,
    "时间范围不合法",
);
const SESSION_REQUIRED: AdminErrorSpec = AdminErrorSpec::new(
    StatusCode::UNAUTHORIZED,
    AdminErrorCode::SESSION_REQUIRED,
    "需要管理员登录",
);
const INVALID_CREDENTIALS: AdminErrorSpec = AdminErrorSpec::new(
    StatusCode::UNAUTHORIZED,
    AdminErrorCode::INVALID_CREDENTIALS,
    "管理员用户名或密码错误",
);
const INVALID_API_KEY: AdminErrorSpec = AdminErrorSpec::new(
    StatusCode::UNAUTHORIZED,
    AdminErrorCode::INVALID_API_KEY,
    "管理 API Key 无效",
);
const ADMIN_ROUTE_NOT_FOUND: AdminErrorSpec = AdminErrorSpec::new(
    StatusCode::NOT_FOUND,
    AdminErrorCode::NOT_FOUND,
    "管理接口不存在",
);
const METHOD_NOT_ALLOWED: AdminErrorSpec = AdminErrorSpec::new(
    StatusCode::METHOD_NOT_ALLOWED,
    AdminErrorCode::BAD_REQUEST,
    "请求方法不受支持",
);
const TOO_MANY_LOGIN_ATTEMPTS: AdminErrorSpec = AdminErrorSpec::new(
    StatusCode::TOO_MANY_REQUESTS,
    AdminErrorCode::TOO_MANY_LOGIN_ATTEMPTS,
    "登录尝试过多，请稍后重试",
);
const INTERNAL: AdminErrorSpec = AdminErrorSpec::new(
    StatusCode::INTERNAL_SERVER_ERROR,
    AdminErrorCode::INTERNAL,
    "服务内部错误",
);
const BAD_GATEWAY: AdminErrorSpec = AdminErrorSpec::new(
    StatusCode::BAD_GATEWAY,
    AdminErrorCode::BAD_GATEWAY,
    "上游服务请求失败",
);
const UPSTREAM_RESULT_UNKNOWN: AdminErrorSpec = AdminErrorSpec::new(
    StatusCode::BAD_GATEWAY,
    AdminErrorCode::UPSTREAM_RESULT_UNKNOWN,
    "上游执行结果未知，请刷新状态后再决定是否重试",
);
const SERVICE_UNAVAILABLE: AdminErrorSpec = AdminErrorSpec::new(
    StatusCode::SERVICE_UNAVAILABLE,
    AdminErrorCode::SERVICE_UNAVAILABLE,
    "依赖服务暂不可用",
);

impl AdminError {
    fn new(status: StatusCode, code: AdminErrorCode, message: impl Into<String>) -> Self {
        Self {
            status,
            body: AdminErrorBody::new(code, message),
        }
    }

    fn from_spec(spec: AdminErrorSpec) -> Self {
        Self::new(spec.status, spec.code, spec.message)
    }

    pub fn malformed_json() -> Self {
        Self::from_spec(MALFORMED_JSON)
    }

    pub fn invalid_request(status: StatusCode, message: &'static str) -> Self {
        Self::new(status, BAD_REQUEST.code, message)
    }

    pub fn invalid_time_range() -> Self {
        Self::from_spec(INVALID_TIME_RANGE)
    }

    pub fn admin_route_not_found() -> Self {
        Self::from_spec(ADMIN_ROUTE_NOT_FOUND)
    }

    pub fn method_not_allowed() -> Self {
        Self::from_spec(METHOD_NOT_ALLOWED)
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(BAD_REQUEST.status, BAD_REQUEST.code, message)
    }

    pub fn admin_session_required() -> Self {
        Self::from_spec(SESSION_REQUIRED)
    }

    pub fn invalid_admin_credentials() -> Self {
        Self::from_spec(INVALID_CREDENTIALS)
    }

    pub fn invalid_admin_api_key() -> Self {
        Self::from_spec(INVALID_API_KEY)
    }

    pub fn too_many_login_attempts() -> Self {
        Self::from_spec(TOO_MANY_LOGIN_ATTEMPTS)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, AdminErrorCode::CONFLICT, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, AdminErrorCode::NOT_FOUND, message)
    }

    pub fn internal() -> Self {
        Self::from_spec(INTERNAL)
    }

    pub fn bad_gateway() -> Self {
        Self::from_spec(BAD_GATEWAY)
    }

    pub fn upstream_result_unknown() -> Self {
        Self::from_spec(UPSTREAM_RESULT_UNKNOWN)
    }

    pub fn service_unavailable() -> Self {
        Self::from_spec(SERVICE_UNAVAILABLE)
    }
}

/// 把管理用例的稳定错误分类映射到既有 HTTP 错误 contract。
pub(crate) fn map_admin_service_error(error: gateway_admin::model::AdminError) -> AdminError {
    use gateway_admin::model::AdminErrorKind;

    match error.kind() {
        AdminErrorKind::Invalid => AdminError::bad_request(error.message()),
        AdminErrorKind::Unauthorized => AdminError::admin_session_required(),
        AdminErrorKind::NotFound => AdminError::not_found(error.message()),
        AdminErrorKind::Conflict => AdminError::conflict(error.message()),
        AdminErrorKind::RateLimited => AdminError::too_many_login_attempts(),
        AdminErrorKind::BadGateway => AdminError::bad_gateway(),
        AdminErrorKind::UpstreamResultUnknown => AdminError::upstream_result_unknown(),
        AdminErrorKind::Unavailable => AdminError::service_unavailable(),
        AdminErrorKind::Internal => AdminError::internal(),
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
