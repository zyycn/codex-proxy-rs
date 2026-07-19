//! 管理端账号目录、查询校验与安全响应 wire。

use std::{fmt, pin::Pin};

use futures::Stream;
use serde::Deserialize;
use serde_json::Value;

use gateway_core::routing::ProviderKind;

use crate::admin::WireValidationError;

const DEFAULT_PAGE_SIZE: u32 = 50;
const MAX_PAGE_SIZE: u32 = 200;
const MAX_SEARCH_BYTES: usize = 256;

/// 账号列表查询参数。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListQuery {
    pub page: Option<u32>,
    pub page_size: Option<u32>,
    pub provider: Option<String>,
    pub search: Option<String>,
    pub status: Option<String>,
    pub sort_by: Option<String>,
    pub sort_direction: Option<String>,
}

impl ListQuery {
    /// 解析并校验全部 wire 字段。
    pub fn validate(self) -> Result<ValidatedListQuery, WireValidationError> {
        let page = self.page.unwrap_or(1);
        if page == 0 {
            return Err(WireValidationError::new("page"));
        }
        let page_size = self.page_size.unwrap_or(DEFAULT_PAGE_SIZE);
        if page_size == 0 || page_size > MAX_PAGE_SIZE {
            return Err(WireValidationError::new("pageSize"));
        }
        let provider = ProviderFilter::parse(self.provider.as_deref().unwrap_or("all"))
            .ok_or_else(|| WireValidationError::new("provider"))?;
        let search = self
            .search
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        if search.as_deref().is_some_and(|value| {
            value.len() > MAX_SEARCH_BYTES || value.chars().any(char::is_control)
        }) {
            return Err(WireValidationError::new("search"));
        }
        let status = match self.status.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(value) => Some(
                AccountStatus::parse(value).ok_or_else(|| WireValidationError::new("status"))?,
            ),
        };
        let sort = match (self.sort_by.as_deref(), self.sort_direction.as_deref()) {
            (None, None) => None,
            (Some(field), Some(direction)) => Some(AccountSort {
                field: SortField::parse(field).ok_or_else(|| WireValidationError::new("sortBy"))?,
                direction: SortDirection::parse(direction)
                    .ok_or_else(|| WireValidationError::new("sortDirection"))?,
            }),
            _ => return Err(WireValidationError::new("sort")),
        };
        Ok(ValidatedListQuery {
            page,
            page_size,
            provider,
            search,
            status,
            sort,
        })
    }
}

/// 已校验的账号查询。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedListQuery {
    pub page: u32,
    pub page_size: u32,
    pub provider: ProviderFilter,
    pub search: Option<String>,
    pub status: Option<AccountStatus>,
    pub sort: Option<AccountSort>,
}

/// Provider 过滤值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderFilter {
    All,
    Provider(ProviderKind),
}

impl ProviderFilter {
    fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        if value.is_empty() || value.eq_ignore_ascii_case("all") {
            return Some(Self::All);
        }
        ProviderKind::new(value.to_owned()).ok().map(Self::Provider)
    }
}

/// 账号状态过滤值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountStatus {
    Active,
    Expired,
    QuotaExhausted,
    Disabled,
    Banned,
}

impl AccountStatus {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "active" => Some(Self::Active),
            "expired" => Some(Self::Expired),
            "quota_exhausted" => Some(Self::QuotaExhausted),
            "disabled" => Some(Self::Disabled),
            "banned" => Some(Self::Banned),
            _ => None,
        }
    }
}

/// 账号排序字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortField {
    Email,
    Status,
    PlanType,
    Usage,
    LastUsedAt,
    ExpiresAt,
}

impl SortField {
    fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "email" => Some(Self::Email),
            "status" => Some(Self::Status),
            "planType" => Some(Self::PlanType),
            "usage" => Some(Self::Usage),
            "lastUsedAt" => Some(Self::LastUsedAt),
            "expiresAt" => Some(Self::ExpiresAt),
            _ => None,
        }
    }
}

/// 账号排序方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Asc,
    Desc,
}

impl SortDirection {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "asc" => Some(Self::Asc),
            "desc" => Some(Self::Desc),
            _ => None,
        }
    }
}

/// 已校验的排序组合。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountSort {
    pub field: SortField,
    pub direction: SortDirection,
}

use serde::Serialize;

use crate::admin::PageMeta;

/// 账号列表响应数据。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountPageData {
    pub config_revision: u64,
    pub items: Vec<AccountView>,
    pub page: PageMeta,
    pub summary: AccountSummaryView,
}

/// 账号概览计数。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountSummaryView {
    pub total: u64,
    pub active: u64,
    pub quota_exhausted: u64,
    pub attention: u64,
}

/// 一条安全账号视图。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountView {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub provider_instance_id: String,
    pub provider_instance_name: String,
    pub resource_ref: String,
    pub email: Option<String>,
    pub account_id: Option<String>,
    pub user_id: Option<String>,
    pub label: Option<String>,
    pub plan_type: Option<String>,
    pub has_refresh_token: bool,
    pub status: String,
    pub display_status: String,
    pub token_refreshing: bool,
    pub availability: String,
    pub enabled: bool,
    pub credential_revision: u64,
    pub state_revision: Option<u64>,
    pub access_token_expires_at: Option<String>,
    pub access_token_expires_at_display: Option<String>,
    pub refresh_token_expires_at: Option<String>,
    pub next_refresh_at: Option<String>,
    pub added_at: String,
    pub added_at_display: String,
    pub updated_at: String,
    pub updated_at_display: String,
    pub quota: AccountQuotaView,
    pub usage: AccountUsageView,
}

/// Provider quota 安全视图。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountQuotaView {
    pub refreshed_at_display: String,
    pub windows: Vec<AccountQuotaWindowView>,
}

/// 一个 quota 时间窗口。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountQuotaWindowView {
    pub key: String,
    pub group: String,
    pub window_seconds: Option<u64>,
    pub label_display: String,
    pub used_percent: Option<f64>,
    pub used_percent_display: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_usage: Option<serde_json::Value>,
    pub reset_at_display: String,
}

/// 账号观测用量。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountUsageView {
    pub request_count: Option<u64>,
    pub request_count_display: String,
    pub input_tokens: Option<u64>,
    pub input_tokens_display: String,
    pub output_tokens: Option<u64>,
    pub output_tokens_display: String,
    pub cached_tokens: Option<u64>,
    pub cached_tokens_display: String,
    pub total_tokens: Option<u64>,
    pub total_tokens_display: String,
    pub created_tokens: Option<u64>,
    pub created_tokens_display: String,
    pub read_tokens: Option<u64>,
    pub read_tokens_display: String,
    pub last_used_at: Option<String>,
    pub last_used_at_display: String,
    pub cost_estimate_status: String,
    pub known_cost_count: Option<u64>,
    pub partial_cost_count: Option<u64>,
    pub unknown_cost_count: Option<u64>,
    pub costs: Vec<CurrencyCostView>,
    pub models: Vec<ModelUsageView>,
}

/// 凭据在单个上游模型上的观测用量。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsageView {
    pub model: String,
    pub request_count: u64,
    pub request_count_display: String,
    pub success_rate: Option<f64>,
    pub success_rate_display: String,
    pub input_tokens: Option<u64>,
    pub input_tokens_display: String,
    pub output_tokens: Option<u64>,
    pub output_tokens_display: String,
    pub cached_tokens: Option<u64>,
    pub cached_tokens_display: String,
    pub total_tokens: Option<u64>,
    pub total_tokens_display: String,
    pub billing_amount_usd: Option<String>,
    pub billing_amount_usd_display: String,
    pub cost_estimate_status: String,
    pub known_cost_count: u64,
    pub partial_cost_count: u64,
    pub unknown_cost_count: u64,
    pub costs: Vec<CurrencyCostView>,
    pub last_used_at: String,
    pub last_used_at_display: String,
}

/// 单一货币的可查询成本。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrencyCostView {
    pub currency: String,
    pub estimated_amount: String,
    pub estimated_amount_display: String,
}

/// 账号详情类 GET 的固定 ID query。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountIdQuery {
    pub id: String,
}

impl AccountIdQuery {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        require_account_id(&self.id)
    }
}

/// 敏感导出的固定 query；IDs 使用逗号分隔，禁止隐式导出全部账号。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountExportQuery {
    pub ids: String,
    pub confirm: String,
}

impl AccountExportQuery {
    pub fn into_ids(self) -> Result<Vec<String>, WireValidationError> {
        if self.confirm != "export_sensitive_accounts" {
            return Err(WireValidationError::new("confirm"));
        }
        let ids = self
            .ids
            .split(',')
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        if ids.is_empty() || ids.len() > 200 || ids.iter().any(|id| require_account_id(id).is_err())
        {
            return Err(WireValidationError::new("ids"));
        }
        let unique = ids.iter().collect::<std::collections::BTreeSet<_>>();
        if unique.len() != ids.len() {
            return Err(WireValidationError::new("ids"));
        }
        Ok(ids)
    }
}

/// 账号运行期动作。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountActionRequest {
    pub id: String,
}

impl AccountActionRequest {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        require_account_id(&self.id)
    }
}

/// 手工 OAuth 刷新会变更持久 credential，因此必须携带配置 CAS revision。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountRefreshRequest {
    pub id: String,
    pub expected_config_revision: u64,
}

impl AccountRefreshRequest {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        require_account_id(&self.id)?;
        if self.expected_config_revision == 0 {
            return Err(WireValidationError::new("expectedConfigRevision"));
        }
        Ok(())
    }
}

/// 连接测试 query；测试仍经唯一 Core/Provider 模型请求路径执行。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountTestQuery {
    pub id: String,
    pub model_id: String,
}

impl AccountTestQuery {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        require_account_id(&self.id)?;
        if self.model_id.trim().is_empty() || self.model_id.chars().any(char::is_control) {
            return Err(WireValidationError::new("modelId"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountModelView {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountModelsData {
    pub models: Vec<AccountModelView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountRefreshData {
    pub config_revision: u64,
    pub account: AccountView,
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountQuotaData {
    pub account: AccountView,
}

/// Provider-owned 明文导出文档；Debug 永远不输出内部 JSON。
#[derive(Serialize)]
#[serde(transparent)]
pub struct AccountExportData(Value);

impl AccountExportData {
    #[must_use]
    pub const fn new(value: Value) -> Self {
        Self(value)
    }
}

impl fmt::Debug for AccountExportData {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AccountExportData(<redacted>)")
    }
}

pub struct AccountConnectionTestEvent {
    pub data: Value,
}

impl AccountConnectionTestEvent {
    #[must_use]
    pub fn started(model: impl Into<String>) -> Self {
        Self {
            data: serde_json::json!({
                "type": "test_start",
                "model": model.into(),
                "text": "正在连接上游 Responses"
            }),
        }
    }

    #[must_use]
    pub fn request(payload: Value) -> Self {
        Self {
            data: serde_json::json!({ "type": "request", "payload": payload }),
        }
    }

    #[must_use]
    pub fn content(text: impl Into<String>) -> Self {
        Self {
            data: serde_json::json!({ "type": "content", "text": text.into() }),
        }
    }

    #[must_use]
    pub fn completed(account_status: impl Into<String>) -> Self {
        Self {
            data: serde_json::json!({
                "type": "test_complete",
                "success": true,
                "accountStatus": account_status.into()
            }),
        }
    }

    #[must_use]
    pub fn failed(error: impl Into<String>, account_status: impl Into<String>) -> Self {
        Self {
            data: serde_json::json!({
                "type": "error",
                "error": error.into(),
                "accountStatus": account_status.into()
            }),
        }
    }
}

pub type AccountConnectionTestEventStream =
    Pin<Box<dyn Stream<Item = AccountConnectionTestEvent> + Send + 'static>>;

fn require_account_id(value: &str) -> Result<(), WireValidationError> {
    if value.trim().is_empty()
        || value.len() > 128
        || value.chars().any(char::is_control)
        || !value.starts_with("acct_")
    {
        return Err(WireValidationError::new("id"));
    }
    Ok(())
}

use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderValue, StatusCode, header::CACHE_CONTROL},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use futures::StreamExt as _;
use std::convert::Infallible;

use super::{
    AdminAuth, AdminEnvelope, AdminError, AdminResponse, AdminServiceError, AdminServiceErrorKind,
    AdminSessionState,
};

/// 统一账号目录应用端口。
#[async_trait]
pub trait AccountAdminService: Send + Sync {
    async fn list(&self, query: ValidatedListQuery) -> Result<AccountPageData, AdminServiceError>;
    async fn export(
        &self,
        context: &super::AdminRequestContext,
        ids: Vec<String>,
    ) -> Result<AccountExportData, AdminServiceError>;
    async fn refresh(
        &self,
        context: &super::AdminRequestContext,
        request: AccountRefreshRequest,
    ) -> Result<AccountRefreshData, AdminServiceError>;
    async fn quota(&self, id: String) -> Result<AccountQuotaData, AdminServiceError>;
    async fn refresh_quota(&self, id: String) -> Result<AccountQuotaData, AdminServiceError>;
    async fn models(&self, id: String) -> Result<AccountModelsData, AdminServiceError>;
    async fn refresh_models(&self, id: String) -> Result<AccountModelsData, AdminServiceError>;
    async fn test_connection(
        &self,
        id: String,
        model_id: String,
    ) -> Result<AccountConnectionTestEventStream, AdminServiceError>;
}

/// 账号目录 HTTP module 所需最小 state。
pub trait AccountAdminState: AdminSessionState {
    fn account_admin_service(&self) -> &dyn AccountAdminService;
}

/// 构造固定 GET 账号目录路由。
pub fn router<S>() -> Router<S>
where
    S: AccountAdminState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/admin/accounts", get(list_accounts::<S>))
        .route("/api/admin/accounts/export", get(export_accounts::<S>))
        .route("/api/admin/accounts/refresh", post(refresh_account::<S>))
        .route("/api/admin/accounts/quota", get(account_quota::<S>))
        .route(
            "/api/admin/accounts/quota/refresh",
            post(refresh_account_quota::<S>),
        )
        .route("/api/admin/accounts/models", get(account_models::<S>))
        .route(
            "/api/admin/accounts/models/refresh",
            post(refresh_account_models::<S>),
        )
        .route(
            "/api/admin/accounts/test",
            get(test_account_connection::<S>),
        )
}

async fn list_accounts<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    Query(query): Query<ListQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AccountAdminState + Send + Sync,
{
    let query = query
        .validate()
        .map_err(|error| AdminError::bad_request(format!("Invalid {}", error.field())))?;
    let data = state
        .account_admin_service()
        .list(query)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn export_accounts<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Query(query): Query<AccountExportQuery>,
) -> Result<Response, AdminError>
where
    S: AccountAdminState + Send + Sync,
{
    let ids = query.into_ids().map_err(map_wire_error)?;
    let data = state
        .account_admin_service()
        .export(auth.context(), ids)
        .await
        .map_err(map_service_error)?;
    let mut response = AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn refresh_account<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(request): Json<AccountRefreshRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AccountAdminState + Send + Sync,
{
    request.validate().map_err(map_wire_error)?;
    let data = state
        .account_admin_service()
        .refresh(auth.context(), request)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn account_quota<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    Query(query): Query<AccountIdQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AccountAdminState + Send + Sync,
{
    query.validate().map_err(map_wire_error)?;
    let data = state
        .account_admin_service()
        .quota(query.id)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn refresh_account_quota<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    Json(request): Json<AccountActionRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AccountAdminState + Send + Sync,
{
    request.validate().map_err(map_wire_error)?;
    let data = state
        .account_admin_service()
        .refresh_quota(request.id)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn account_models<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    Query(query): Query<AccountIdQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AccountAdminState + Send + Sync,
{
    query.validate().map_err(map_wire_error)?;
    let data = state
        .account_admin_service()
        .models(query.id)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn refresh_account_models<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    Json(request): Json<AccountActionRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AccountAdminState + Send + Sync,
{
    request.validate().map_err(map_wire_error)?;
    let data = state
        .account_admin_service()
        .refresh_models(request.id)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn test_account_connection<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    Query(query): Query<AccountTestQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AdminError>
where
    S: AccountAdminState + Send + Sync,
{
    query.validate().map_err(map_wire_error)?;
    let stream = state
        .account_admin_service()
        .test_connection(query.id, query.model_id)
        .await
        .map_err(map_service_error)?
        .map(|event| {
            let data = serde_json::to_string(&event.data).unwrap_or_else(|_| "{}".to_owned());
            Ok(Event::default().data(data))
        });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

fn map_wire_error(error: WireValidationError) -> AdminError {
    AdminError::bad_request(format!("Invalid {}", error.field()))
}

fn map_service_error(error: AdminServiceError) -> AdminError {
    match error.kind() {
        AdminServiceErrorKind::Invalid => AdminError::bad_request(error.to_string()),
        AdminServiceErrorKind::NotFound => AdminError::not_found(error.to_string()),
        AdminServiceErrorKind::Conflict => AdminError::conflict(error.to_string()),
        AdminServiceErrorKind::Unavailable => {
            AdminError::service_unavailable("Account directory unavailable")
        }
        AdminServiceErrorKind::Internal => AdminError::internal(error.to_string()),
    }
}
