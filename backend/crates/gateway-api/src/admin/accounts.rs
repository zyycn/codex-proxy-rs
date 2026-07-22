//! 管理端账号目录、查询校验与安全响应 wire。

use std::{collections::BTreeSet, convert::Infallible, fmt};

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
use chrono::{DateTime, FixedOffset, Utc};
use futures::{Stream, StreamExt as _};
use gateway_admin::model::{
    AdminError as AdminServiceError, AdminErrorKind, PageSize, Revision,
    accounts::{
        AccountAvailability, AccountConnectionTestEvent as DomainConnectionTestEvent, AccountCost,
        AccountListQuery, AccountModelUsage, AccountSort, AccountSortField,
        AccountStatus as DomainAccountStatus, AccountUsage, SortDirection,
    },
    provider_credentials::{
        AccountDirectoryItem, AccountDirectoryPage, AccountExportBundle, AccountRefreshResult,
        ProviderDocument, ProviderModels, ProviderQuota, ProviderQuotaWindow,
    },
};
use gateway_core::{
    engine::credential::ProviderAccountId,
    routing::{ProviderKind, UpstreamModelId},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    AdminAuth, AdminEnvelope, AdminError, AdminResponse, AdminSessionState, PageMeta,
    WireValidationError,
};

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
    /// 解析并校验全部 wire 字段，生成 Admin 查询命令。
    pub fn validate(self) -> Result<AccountListQuery, WireValidationError> {
        let page = self.page.unwrap_or(1);
        if page == 0 {
            return Err(WireValidationError::new("page"));
        }
        let page_size = self.page_size.unwrap_or(DEFAULT_PAGE_SIZE);
        if page_size == 0 || page_size > MAX_PAGE_SIZE {
            return Err(WireValidationError::new("pageSize"));
        }
        let provider_kind = parse_provider(self.provider.as_deref().unwrap_or("all"))?;
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
                parse_account_status(value).ok_or_else(|| WireValidationError::new("status"))?,
            ),
        };
        let sort = match (self.sort_by.as_deref(), self.sort_direction.as_deref()) {
            (None, None) => None,
            (Some(field), Some(direction)) => Some(AccountSort {
                field: parse_sort_field(field).ok_or_else(|| WireValidationError::new("sortBy"))?,
                direction: parse_sort_direction(direction)
                    .ok_or_else(|| WireValidationError::new("sortDirection"))?,
            }),
            _ => return Err(WireValidationError::new("sort")),
        };
        Ok(AccountListQuery {
            page,
            page_size: PageSize::new(
                u16::try_from(page_size).map_err(|_| WireValidationError::new("pageSize"))?,
            )
            .map_err(|_| WireValidationError::new("pageSize"))?,
            provider_kind,
            search,
            status,
            sort,
        })
    }
}

fn parse_provider(value: &str) -> Result<Option<ProviderKind>, WireValidationError> {
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("all") {
        return Ok(None);
    }
    ProviderKind::new(value.to_owned())
        .map(Some)
        .map_err(|_| WireValidationError::new("provider"))
}

fn parse_account_status(value: &str) -> Option<DomainAccountStatus> {
    match value.trim().to_ascii_lowercase().as_str() {
        "active" => Some(DomainAccountStatus::Active),
        "expired" => Some(DomainAccountStatus::Expired),
        "quota_exhausted" => Some(DomainAccountStatus::QuotaExhausted),
        "disabled" => Some(DomainAccountStatus::Disabled),
        "banned" => Some(DomainAccountStatus::Banned),
        _ => None,
    }
}

fn parse_sort_field(value: &str) -> Option<AccountSortField> {
    match value.trim() {
        "email" => Some(AccountSortField::Email),
        "status" => Some(AccountSortField::Status),
        "planType" => Some(AccountSortField::PlanType),
        "usage" => Some(AccountSortField::Usage),
        "lastUsedAt" => Some(AccountSortField::LastUsedAt),
        "expiresAt" => Some(AccountSortField::ExpiresAt),
        _ => None,
    }
}

fn parse_sort_direction(value: &str) -> Option<SortDirection> {
    match value.trim().to_ascii_lowercase().as_str() {
        "asc" => Some(SortDirection::Asc),
        "desc" => Some(SortDirection::Desc),
        _ => None,
    }
}

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
    pub image_input_tokens: Option<u64>,
    pub image_input_tokens_display: String,
    pub image_output_tokens: Option<u64>,
    pub image_output_tokens_display: String,
    pub image_request_count: Option<u64>,
    pub image_request_count_display: String,
    pub image_request_failed_count: Option<u64>,
    pub image_request_failed_count_display: String,
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
    pub image_input_tokens: Option<u64>,
    pub image_input_tokens_display: String,
    pub image_output_tokens: Option<u64>,
    pub image_output_tokens_display: String,
    pub image_request_count: u64,
    pub image_request_count_display: String,
    pub image_request_failed_count: u64,
    pub image_request_failed_count_display: String,
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

    fn into_id(self) -> Result<ProviderAccountId, WireValidationError> {
        self.validate()?;
        ProviderAccountId::new(self.id).map_err(|_| WireValidationError::new("id"))
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
    pub fn into_ids(self) -> Result<Vec<ProviderAccountId>, WireValidationError> {
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
        let unique = ids.iter().collect::<BTreeSet<_>>();
        if unique.len() != ids.len() {
            return Err(WireValidationError::new("ids"));
        }
        ids.into_iter()
            .map(|id| ProviderAccountId::new(id).map_err(|_| WireValidationError::new("ids")))
            .collect()
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

    fn into_id(self) -> Result<ProviderAccountId, WireValidationError> {
        self.validate()?;
        ProviderAccountId::new(self.id).map_err(|_| WireValidationError::new("id"))
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

    fn into_command(self) -> Result<(Revision, ProviderAccountId), WireValidationError> {
        self.validate()?;
        Ok((
            Revision::new(self.expected_config_revision)
                .map_err(|_| WireValidationError::new("expectedConfigRevision"))?,
            ProviderAccountId::new(self.id).map_err(|_| WireValidationError::new("id"))?,
        ))
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

    fn into_command(self) -> Result<(ProviderAccountId, UpstreamModelId), WireValidationError> {
        self.validate()?;
        Ok((
            ProviderAccountId::new(self.id).map_err(|_| WireValidationError::new("id"))?,
            UpstreamModelId::new(self.model_id).map_err(|_| WireValidationError::new("modelId"))?,
        ))
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

    fn from_result(bundle: AccountExportBundle) -> Self {
        let documents = bundle
            .documents
            .into_iter()
            .map(|document| {
                serde_json::json!({
                    "provider": document.provider_kind.to_string(),
                    "document": provider_document_value(document.document),
                })
            })
            .collect::<Vec<_>>();
        Self::new(serde_json::json!({
            "exportedAt": bundle.exported_at.to_rfc3339(),
            "documents": documents,
        }))
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

impl From<DomainConnectionTestEvent> for AccountConnectionTestEvent {
    fn from(event: DomainConnectionTestEvent) -> Self {
        let data = match event {
            DomainConnectionTestEvent::Started { model } => serde_json::json!({
                "type": "test_start",
                "model": model,
                "text": "正在连接上游 Responses"
            }),
            DomainConnectionTestEvent::Request {
                model,
                input_text,
                stream,
                store,
            } => serde_json::json!({
                "type": "request",
                "payload": {
                    "model": model,
                    "input": [{
                        "role": "user",
                        "content": [{ "type": "input_text", "text": input_text }]
                    }],
                    "stream": stream,
                    "store": store
                }
            }),
            DomainConnectionTestEvent::Content { text } => {
                serde_json::json!({ "type": "content", "text": text })
            }
            DomainConnectionTestEvent::Completed { account_status } => serde_json::json!({
                "type": "test_complete",
                "success": true,
                "accountStatus": account_status_name(account_status)
            }),
            DomainConnectionTestEvent::Failed {
                message,
                account_status,
            } => serde_json::json!({
                "type": "error",
                "error": message,
                "accountStatus": account_status_name(account_status)
            }),
        };
        Self { data }
    }
}

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

/// 构造固定 GET 账号目录路由。
pub fn router<S>() -> Router<S>
where
    S: AdminSessionState + Clone + Send + Sync + 'static,
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
    S: AdminSessionState + Send + Sync,
{
    let command = query.validate().map_err(map_wire_error)?;
    let page = command.page;
    let page_size = command.page_size.get();
    let result = state
        .admin_services()
        .accounts()
        .list(command)
        .await
        .map_err(map_service_error)?;
    let data = account_page_data(result, page, page_size, Utc::now());
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn export_accounts<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Query(query): Query<AccountExportQuery>,
) -> Result<Response, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let ids = query.into_ids().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .export(&auth.context().mutation_context(), ids)
        .await
        .map_err(map_service_error)?;
    let data = AccountExportData::from_result(result);
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
    S: AdminSessionState + Send + Sync,
{
    let (expected_config_revision, account_id) = request.into_command().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .refresh(
            &auth.context().mutation_context(),
            expected_config_revision,
            account_id,
        )
        .await
        .map_err(map_service_error)?;
    let data = account_refresh_data(result, Utc::now());
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn account_quota<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    Query(query): Query<AccountIdQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let account_id = query.into_id().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .quota(&account_id, false)
        .await
        .map_err(map_service_error)?;
    let data = AccountQuotaData {
        account: account_view(result, Utc::now()),
    };
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn refresh_account_quota<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    Json(request): Json<AccountActionRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let account_id = request.into_id().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .quota(&account_id, true)
        .await
        .map_err(map_service_error)?;
    let data = AccountQuotaData {
        account: account_view(result, Utc::now()),
    };
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn account_models<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    Query(query): Query<AccountIdQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let account_id = query.into_id().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .models(&account_id, false)
        .await
        .map_err(map_service_error)?;
    let data = account_models_data(result);
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn refresh_account_models<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    Json(request): Json<AccountActionRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let account_id = request.into_id().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .models(&account_id, true)
        .await
        .map_err(map_service_error)?;
    let data = account_models_data(result);
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn test_account_connection<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    Query(query): Query<AccountTestQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let (account_id, upstream_model) = query.into_command().map_err(map_wire_error)?;
    let stream = state
        .admin_services()
        .accounts()
        .test_connection(account_id, upstream_model)
        .await
        .map_err(map_service_error)?
        .map(|event| {
            let event = AccountConnectionTestEvent::from(event);
            let data = serde_json::to_string(&event.data).unwrap_or_else(|_| "{}".to_owned());
            Ok(Event::default().data(data))
        });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

fn account_page_data(
    result: AccountDirectoryPage,
    page: u32,
    page_size: u16,
    now: DateTime<Utc>,
) -> AccountPageData {
    let total_pages = if result.total == 0 {
        0
    } else {
        u32::try_from(result.total.div_ceil(u64::from(page_size))).unwrap_or(u32::MAX)
    };
    AccountPageData {
        config_revision: result.config_revision.get(),
        items: result
            .items
            .into_iter()
            .map(|item| account_view(item, now))
            .collect(),
        page: PageMeta::new(page, u32::from(page_size), result.total, total_pages),
        summary: AccountSummaryView {
            total: result.summary.total,
            active: result.summary.active,
            quota_exhausted: result.summary.quota_exhausted,
            attention: result.summary.attention,
        },
    }
}

fn account_refresh_data(result: AccountRefreshResult, now: DateTime<Utc>) -> AccountRefreshData {
    AccountRefreshData {
        config_revision: result.config_revision.get(),
        account: account_view(result.account, now),
    }
}

fn account_models_data(result: ProviderModels) -> AccountModelsData {
    AccountModelsData {
        models: result
            .models
            .into_iter()
            .map(|model| {
                let id = model.id.to_string();
                AccountModelView {
                    label: id.clone(),
                    id,
                }
            })
            .collect(),
    }
}

fn account_view(item: AccountDirectoryItem, now: DateTime<Utc>) -> AccountView {
    let AccountDirectoryItem {
        account,
        provider_instance_name,
        status,
        usage,
        quota,
    } = item;
    let status = account_status_name(status).to_owned();
    let expires_at = china_rfc3339(&account.access_token_expires_at);
    let added_at = china_rfc3339(&account.created_at);
    let updated_at = china_rfc3339(&account.updated_at);
    let (quota, refresh_token_expires_at) = account_quota_view(quota, now);
    AccountView {
        id: account.id.clone(),
        name: account.name,
        provider: account.provider_kind.to_string(),
        provider_instance_id: account.provider_instance_id.to_string(),
        provider_instance_name,
        resource_ref: account.id,
        email: account.email,
        account_id: account.upstream_account_id,
        user_id: Some(account.upstream_user_id),
        label: None,
        plan_type: account.plan_type,
        has_refresh_token: account.has_refresh_token,
        status: status.clone(),
        display_status: status,
        token_refreshing: false,
        availability: account_availability_name(account.availability).to_owned(),
        enabled: account.enabled,
        credential_revision: account.credential_revision.get(),
        state_revision: None,
        access_token_expires_at: Some(expires_at),
        access_token_expires_at_display: Some(china_datetime(&account.access_token_expires_at)),
        refresh_token_expires_at,
        next_refresh_at: account.next_refresh_at.map(|value| china_rfc3339(&value)),
        added_at,
        added_at_display: china_datetime(&account.created_at),
        updated_at,
        updated_at_display: china_datetime(&account.updated_at),
        quota,
        usage: account_usage_view(usage, now),
    }
}

fn account_quota_view(
    quota: ProviderQuota,
    now: DateTime<Utc>,
) -> (AccountQuotaView, Option<String>) {
    let refresh_token_expires_at = quota
        .refresh_token_expires_at
        .map(|value| china_rfc3339(&value));
    let refreshed_at_display = quota
        .observed_at
        .map_or_else(|| "—".to_owned(), |value| relative_time(value, now));
    let windows = quota.windows.into_iter().map(quota_window_view).collect();
    (
        AccountQuotaView {
            refreshed_at_display,
            windows,
        },
        refresh_token_expires_at,
    )
}

fn quota_window_view(window: ProviderQuotaWindow) -> AccountQuotaWindowView {
    let ProviderQuotaWindow {
        key,
        group,
        label,
        source: _,
        window_seconds,
        used_percent,
        reset_at,
        local_usage,
        provider_data: _,
    } = window;
    AccountQuotaWindowView {
        label_display: label,
        key,
        group,
        window_seconds,
        used_percent,
        used_percent_display: used_percent
            .map_or_else(|| "—".to_owned(), |value| format!("{value:.1}%")),
        local_usage: local_usage.as_ref().map(quota_local_usage),
        reset_at_display: reset_at.map_or_else(|| "—".to_owned(), |value| china_datetime(&value)),
    }
}

fn quota_local_usage(usage: &AccountUsage) -> Value {
    let total_tokens = usage.total_tokens.unwrap_or_default();
    serde_json::json!({
        "requestCount": usage.request_count,
        "requestCountDisplay": format_number(usage.request_count),
        "inputTokens": usage.input_tokens.unwrap_or_default(),
        "inputTokensDisplay": display_optional_tokens(usage.input_tokens),
        "outputTokens": usage.output_tokens.unwrap_or_default(),
        "outputTokensDisplay": display_optional_tokens(usage.output_tokens),
        "cachedTokens": usage.cached_tokens.unwrap_or_default(),
        "cachedTokensDisplay": display_optional_tokens(usage.cached_tokens),
        "imageInputTokens": usage.image_input_tokens.unwrap_or_default(),
        "imageInputTokensDisplay": display_optional_tokens(usage.image_input_tokens),
        "imageOutputTokens": usage.image_output_tokens.unwrap_or_default(),
        "imageOutputTokensDisplay": display_optional_tokens(usage.image_output_tokens),
        "imageRequestCount": usage.image_request_count,
        "imageRequestFailedCount": usage.image_request_failed_count,
        "totalTokens": total_tokens,
        "totalTokensDisplay": format_tokens(total_tokens),
    })
}

fn account_usage_view(usage: Option<AccountUsage>, now: DateTime<Utc>) -> AccountUsageView {
    let Some(usage) = usage else {
        return empty_account_usage();
    };
    let known_count = usage
        .cost_coverage
        .provider_reported_count
        .saturating_add(usage.cost_coverage.calculated_count);
    let cost_estimate_status = if known_count == 0 {
        "unknown"
    } else if usage.cost_coverage.unavailable_count > 0 {
        "partial"
    } else {
        "known"
    };
    AccountUsageView {
        request_count: Some(usage.request_count),
        request_count_display: format_number(usage.request_count),
        input_tokens: usage.input_tokens,
        input_tokens_display: display_optional_tokens(usage.input_tokens),
        output_tokens: usage.output_tokens,
        output_tokens_display: display_optional_tokens(usage.output_tokens),
        cached_tokens: usage.cached_tokens,
        cached_tokens_display: display_optional_tokens(usage.cached_tokens),
        image_input_tokens: usage.image_input_tokens,
        image_input_tokens_display: display_optional_tokens(usage.image_input_tokens),
        image_output_tokens: usage.image_output_tokens,
        image_output_tokens_display: display_optional_tokens(usage.image_output_tokens),
        image_request_count: Some(usage.image_request_count),
        image_request_count_display: format_number(usage.image_request_count),
        image_request_failed_count: Some(usage.image_request_failed_count),
        image_request_failed_count_display: format_number(usage.image_request_failed_count),
        total_tokens: usage.total_tokens,
        total_tokens_display: display_optional_tokens(usage.total_tokens),
        created_tokens: usage.cache_write_tokens,
        created_tokens_display: display_optional_tokens(usage.cache_write_tokens),
        read_tokens: usage.cached_tokens,
        read_tokens_display: display_optional_tokens(usage.cached_tokens),
        last_used_at: usage.last_used_at.map(|value| china_rfc3339(&value)),
        last_used_at_display: usage
            .last_used_at
            .map_or_else(|| "—".to_owned(), |value| relative_time(value, now)),
        cost_estimate_status: cost_estimate_status.to_owned(),
        known_cost_count: Some(known_count),
        partial_cost_count: Some(u64::from(cost_estimate_status == "partial")),
        unknown_cost_count: Some(usage.cost_coverage.unavailable_count),
        costs: usage.costs.iter().map(account_currency_cost_view).collect(),
        models: usage
            .models
            .into_iter()
            .map(|model| account_model_usage_view(model, now))
            .collect(),
    }
}

fn account_model_usage_view(usage: AccountModelUsage, now: DateTime<Utc>) -> ModelUsageView {
    let known_count = usage
        .cost_coverage
        .provider_reported_count
        .saturating_add(usage.cost_coverage.calculated_count);
    let cost_estimate_status = if known_count == 0 {
        "unknown"
    } else if usage.cost_coverage.unavailable_count > 0 {
        "partial"
    } else {
        "known"
    };
    let usd = usage
        .costs
        .iter()
        .find(|cost| cost.currency.eq_ignore_ascii_case("USD"));
    ModelUsageView {
        model: usage.model,
        request_count: usage.request_count,
        request_count_display: format_number(usage.request_count),
        success_rate: (usage.request_count > 0)
            .then(|| usage.success_count as f64 * 100.0 / usage.request_count as f64),
        success_rate_display: if usage.request_count == 0 {
            "—".to_owned()
        } else {
            format!(
                "{:.1}%",
                usage.success_count as f64 * 100.0 / usage.request_count as f64
            )
        },
        input_tokens: usage.input_tokens,
        input_tokens_display: display_optional_tokens(usage.input_tokens),
        output_tokens: usage.output_tokens,
        output_tokens_display: display_optional_tokens(usage.output_tokens),
        cached_tokens: usage.cached_tokens,
        cached_tokens_display: display_optional_tokens(usage.cached_tokens),
        image_input_tokens: usage.image_input_tokens,
        image_input_tokens_display: display_optional_tokens(usage.image_input_tokens),
        image_output_tokens: usage.image_output_tokens,
        image_output_tokens_display: display_optional_tokens(usage.image_output_tokens),
        image_request_count: usage.image_request_count,
        image_request_count_display: format_number(usage.image_request_count),
        image_request_failed_count: usage.image_request_failed_count,
        image_request_failed_count_display: format_number(usage.image_request_failed_count),
        total_tokens: usage.total_tokens,
        total_tokens_display: display_optional_tokens(usage.total_tokens),
        billing_amount_usd: usd.map(|cost| cost.amount.as_str().to_owned()),
        billing_amount_usd_display: usd.map_or_else(
            || "—".to_owned(),
            |cost| format!("${}", cost.amount.as_str()),
        ),
        cost_estimate_status: cost_estimate_status.to_owned(),
        known_cost_count: known_count,
        partial_cost_count: u64::from(cost_estimate_status == "partial"),
        unknown_cost_count: usage.cost_coverage.unavailable_count,
        costs: usage.costs.iter().map(account_currency_cost_view).collect(),
        last_used_at: china_rfc3339(&usage.last_used_at),
        last_used_at_display: relative_time(usage.last_used_at, now),
    }
}

fn account_currency_cost_view(cost: &AccountCost) -> CurrencyCostView {
    CurrencyCostView {
        currency: cost.currency.clone(),
        estimated_amount: cost.amount.as_str().to_owned(),
        estimated_amount_display: format!("{} {}", cost.currency, cost.amount.as_str()),
    }
}

fn display_optional_tokens(value: Option<u64>) -> String {
    value.map_or_else(|| "—".to_owned(), format_tokens)
}

fn empty_account_usage() -> AccountUsageView {
    AccountUsageView {
        request_count: None,
        request_count_display: "—".to_owned(),
        input_tokens: None,
        input_tokens_display: "—".to_owned(),
        output_tokens: None,
        output_tokens_display: "—".to_owned(),
        cached_tokens: None,
        cached_tokens_display: "—".to_owned(),
        image_input_tokens: None,
        image_input_tokens_display: "—".to_owned(),
        image_output_tokens: None,
        image_output_tokens_display: "—".to_owned(),
        image_request_count: None,
        image_request_count_display: "—".to_owned(),
        image_request_failed_count: None,
        image_request_failed_count_display: "—".to_owned(),
        total_tokens: None,
        total_tokens_display: "—".to_owned(),
        created_tokens: None,
        created_tokens_display: "—".to_owned(),
        read_tokens: None,
        read_tokens_display: "—".to_owned(),
        last_used_at: None,
        last_used_at_display: "—".to_owned(),
        cost_estimate_status: "unavailable".to_owned(),
        known_cost_count: None,
        partial_cost_count: None,
        unknown_cost_count: None,
        costs: Vec::new(),
        models: Vec::new(),
    }
}

fn provider_document_value(document: ProviderDocument) -> Value {
    Value::Object(document.into_provider_data().into_inner())
}

fn account_status_name(status: DomainAccountStatus) -> &'static str {
    match status {
        DomainAccountStatus::Active => "active",
        DomainAccountStatus::Expired => "expired",
        DomainAccountStatus::QuotaExhausted => "quota_exhausted",
        DomainAccountStatus::Disabled => "disabled",
        DomainAccountStatus::Banned => "banned",
        DomainAccountStatus::Attention => "attention",
    }
}

fn account_availability_name(availability: AccountAvailability) -> &'static str {
    match availability {
        AccountAvailability::Unknown => "unknown",
        AccountAvailability::Ready => "ready",
        AccountAvailability::Cooldown => "cooldown",
        AccountAvailability::QuotaExhausted => "quota_exhausted",
        AccountAvailability::Expired => "expired",
        AccountAvailability::Banned => "banned",
        AccountAvailability::Invalid => "invalid",
    }
}

fn format_number(value: u64) -> String {
    let text = value.to_string();
    let mut output = String::with_capacity(text.len() + text.len() / 3);
    for (index, character) in text.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            output.push(',');
        }
        output.push(character);
    }
    output.chars().rev().collect()
}

fn format_tokens(value: u64) -> String {
    if value < 1_000 {
        return format_number(value);
    }
    for (suffix, threshold) in [
        ("P", 1_000_000_000_000_000_u64),
        ("T", 1_000_000_000_000_u64),
        ("B", 1_000_000_000_u64),
        ("M", 1_000_000_u64),
        ("K", 1_000_u64),
    ] {
        if value >= threshold {
            let scaled = value as f64 / threshold as f64;
            return format!("{scaled:.1}{suffix}").replace(".0", "");
        }
    }
    format_number(value)
}

fn relative_time(value: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let elapsed = now.signed_duration_since(value);
    if elapsed.num_seconds() < 0 {
        return china_datetime(&value);
    }
    if elapsed.num_seconds() < 60 {
        return "刚刚".to_owned();
    }
    if elapsed.num_minutes() < 60 {
        return format!("{} 分钟前", elapsed.num_minutes());
    }
    if elapsed.num_hours() < 24 {
        return format!("{} 小时前", elapsed.num_hours());
    }
    format!("{} 天前", elapsed.num_days())
}

fn china_offset() -> FixedOffset {
    FixedOffset::east_opt(8 * 60 * 60).expect("UTC+8 is a valid fixed offset")
}

fn china_rfc3339(value: &DateTime<Utc>) -> String {
    value.with_timezone(&china_offset()).to_rfc3339()
}

fn china_datetime(value: &DateTime<Utc>) -> String {
    value
        .with_timezone(&china_offset())
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

fn map_wire_error(error: WireValidationError) -> AdminError {
    AdminError::bad_request(format!("Invalid {}", error.field()))
}

fn map_service_error(error: AdminServiceError) -> AdminError {
    match error.kind() {
        AdminErrorKind::Invalid => AdminError::bad_request(error.to_string()),
        AdminErrorKind::Unauthorized => AdminError::admin_session_required(),
        AdminErrorKind::NotFound => AdminError::not_found(error.to_string()),
        AdminErrorKind::Conflict => AdminError::conflict(error.to_string()),
        AdminErrorKind::RateLimited => AdminError::too_many_login_attempts(),
        AdminErrorKind::BadGateway => AdminError::bad_gateway(error.to_string()),
        AdminErrorKind::Unavailable => {
            AdminError::service_unavailable("Account directory unavailable")
        }
        AdminErrorKind::Internal => AdminError::internal(error.to_string()),
    }
}
