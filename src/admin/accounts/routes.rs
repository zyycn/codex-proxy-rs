//! 账号管理 HTTP 处理器。

use axum::{
    body::Bytes,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension, Json,
};
use std::collections::HashMap;

use chrono::{DateTime, Utc};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    admin::auth::session::require_admin_session,
    admin::response::{
        AdminEnvelope, AdminError, AdminResponse, CursorPageMeta, NumberedPageMeta, PageMeta,
    },
    admin::{
        accounts::service::{AdminAccountError, AdminAccountMetadata, AdminAccountMetadataUpdate},
        monitoring::{
            billing,
            event_store::AdminLogFilter,
            events::{EventLevel, EventLog},
            service::AdminUsageRecord,
        },
    },
    http::middleware::request_id::RequestId,
    infra::{
        json::{clamp_limit, clamp_page, total_pages, Page},
        time::{china_datetime, china_relative_time, china_rfc3339, china_rfc3339_str},
    },
    runtime::state::AppState,
    upstream::accounts::model::AccountStatus,
    upstream::accounts::store::StoredAccount,
};

const ACCOUNT_STATS_PAGE_LIMIT: u32 = 200;
const FIVE_HOUR_WINDOW_SECONDS: u64 = 18_000;
const WEEK_WINDOW_SECONDS: u64 = 604_800;
const MONTH_WINDOW_SECONDS: u64 = 2_592_000;

// ============================================================================
// Query / Request types
// ============================================================================

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountsQuery {
    cursor: Option<String>,
    limit: Option<u32>,
    page: Option<u32>,
    page_size: Option<u32>,
    search: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateAccountRequest {
    token: Option<String>,
    refresh_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BatchDeleteAccountsRequest {
    ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetAccountCookiesRequest {
    id: String,
    cookies: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountActionRequest {
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountIdQuery {
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountExportQuery {
    ids: Option<String>,
    format: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct HealthCheckRequest {
    ids: Option<Vec<String>>,
    stagger_ms: Option<u64>,
    concurrency: Option<u8>,
}

// ============================================================================
// Response types
// ============================================================================

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminAccountData {
    id: String,
    email: Option<String>,
    account_id: Option<String>,
    user_id: Option<String>,
    label: Option<String>,
    plan_type: Option<String>,
    status: String,
    access_token_expires_at: Option<String>,
    access_token_expires_at_display: Option<String>,
    added_at: String,
    added_at_display: String,
    updated_at: String,
    updated_at_display: String,
    quota: AdminAccountQuotaData,
    usage: AdminAccountUsageData,
}

impl From<AdminAccountMetadata> for AdminAccountData {
    fn from(a: AdminAccountMetadata) -> Self {
        Self::from_parts(a, None, None, Vec::new())
    }
}

impl AdminAccountData {
    fn from_parts(
        a: AdminAccountMetadata,
        usage: Option<&AdminUsageRecord>,
        quota: Option<AdminAccountQuotaData>,
        models: Vec<AdminAccountModelUsageData>,
    ) -> Self {
        let access_token_expires_at = a.access_token_expires_at.as_ref().map(china_rfc3339);
        let access_token_expires_at_display =
            a.access_token_expires_at.as_ref().map(china_datetime);
        Self {
            id: a.id,
            email: a.email,
            account_id: a.account_id,
            user_id: a.user_id,
            label: a.label,
            plan_type: a.plan_type,
            status: account_status_str(a.status).to_string(),
            access_token_expires_at,
            access_token_expires_at_display,
            added_at: china_rfc3339(&a.added_at),
            added_at_display: china_datetime(&a.added_at),
            updated_at: china_rfc3339(&a.updated_at),
            updated_at_display: china_datetime(&a.updated_at),
            quota: quota.unwrap_or_default(),
            usage: AdminAccountUsageData::from_usage(usage, models),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminAccountQuotaData {
    refreshed_at_display: String,
    windows: Vec<AdminAccountQuotaWindowData>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminAccountQuotaWindowData {
    key: String,
    group: String,
    window_seconds: Option<u64>,
    label_display: String,
    used_percent: Option<f64>,
    used_percent_display: String,
    reset_at_display: String,
    window_used_display: String,
}

impl Default for AdminAccountQuotaData {
    fn default() -> Self {
        Self {
            refreshed_at_display: "-".to_string(),
            windows: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminAccountUsageData {
    request_count: i64,
    request_count_display: String,
    empty_response_count: i64,
    input_tokens: i64,
    input_tokens_display: String,
    output_tokens: i64,
    output_tokens_display: String,
    cached_tokens: i64,
    cached_tokens_display: String,
    reasoning_tokens: i64,
    total_tokens: i64,
    total_tokens_display: String,
    image_input_tokens: i64,
    image_output_tokens: i64,
    image_tokens_display: String,
    image_request_count: i64,
    image_request_failed_count: i64,
    created_tokens: i64,
    created_tokens_display: String,
    read_tokens: i64,
    read_tokens_display: String,
    last_used_at: Option<String>,
    last_used_at_display: String,
    models: Vec<AdminAccountModelUsageData>,
}

impl AdminAccountUsageData {
    fn from_usage(
        usage: Option<&AdminUsageRecord>,
        models: Vec<AdminAccountModelUsageData>,
    ) -> Self {
        let request_count = usage.map_or(0, |usage| usage.request_count);
        let empty_response_count = usage.map_or(0, |usage| usage.empty_response_count);
        let input_tokens = usage.map_or(0, |usage| usage.input_tokens);
        let output_tokens = usage.map_or(0, |usage| usage.output_tokens);
        let cached_tokens = usage.map_or(0, |usage| usage.cached_tokens);
        let reasoning_tokens = usage.map_or(0, |usage| usage.reasoning_tokens);
        let total_tokens = usage.map_or(0, |usage| usage.total_tokens);
        let image_input_tokens = usage.map_or(0, |usage| usage.image_input_tokens);
        let image_output_tokens = usage.map_or(0, |usage| usage.image_output_tokens);
        let image_request_count = usage.map_or(0, |usage| usage.image_request_count);
        let image_request_failed_count = usage.map_or(0, |usage| usage.image_request_failed_count);
        let created_tokens = input_tokens.saturating_sub(cached_tokens);
        let read_tokens = cached_tokens;
        let last_used_at = usage.and_then(|usage| usage.last_used_at);

        Self {
            request_count,
            request_count_display: format_count(nonnegative_i64_to_u64(request_count)),
            empty_response_count,
            input_tokens,
            input_tokens_display: format_tokens(nonnegative_i64_to_u64(input_tokens)),
            output_tokens,
            output_tokens_display: format_tokens(nonnegative_i64_to_u64(output_tokens)),
            cached_tokens,
            cached_tokens_display: format_tokens(nonnegative_i64_to_u64(cached_tokens)),
            reasoning_tokens,
            total_tokens,
            total_tokens_display: format_tokens(nonnegative_i64_to_u64(total_tokens)),
            image_input_tokens,
            image_output_tokens,
            image_tokens_display: format_tokens(nonnegative_i64_to_u64(
                image_input_tokens + image_output_tokens,
            )),
            image_request_count,
            image_request_failed_count,
            created_tokens,
            created_tokens_display: format_tokens(nonnegative_i64_to_u64(created_tokens)),
            read_tokens,
            read_tokens_display: format_tokens(nonnegative_i64_to_u64(read_tokens)),
            last_used_at: last_used_at.map(|value| china_rfc3339(&value)),
            last_used_at_display: china_relative_time(last_used_at, Utc::now()),
            models,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminAccountModelUsageData {
    model: String,
    request_count: u64,
    request_count_display: String,
    success_rate: f64,
    success_rate_display: String,
    input_tokens: u64,
    input_tokens_display: String,
    output_tokens: u64,
    output_tokens_display: String,
    cached_tokens: u64,
    cached_tokens_display: String,
    total_tokens: u64,
    total_tokens_display: String,
    total_cost_usd: f64,
    total_cost_usd_display: String,
    last_used_at: Option<String>,
    last_used_at_display: String,
}

#[derive(Debug, Clone, Default)]
struct AccountListStats {
    usage_by_account: HashMap<String, AdminUsageRecord>,
    quota_by_account: HashMap<String, AdminAccountQuotaData>,
    models_by_account: HashMap<String, Vec<AdminAccountModelUsageData>>,
}

impl AccountListStats {
    fn data_for(&self, account: AdminAccountMetadata) -> AdminAccountData {
        let account_id = account.id.clone();
        AdminAccountData::from_parts(
            account,
            self.usage_by_account.get(&account_id),
            self.quota_by_account.get(&account_id).cloned(),
            self.models_by_account
                .get(&account_id)
                .cloned()
                .unwrap_or_default(),
        )
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminAccountSummaryData {
    total: u64,
    active: u64,
    high_usage: u64,
    attention: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminAccountPageData {
    items: Vec<AdminAccountData>,
    page: PageMeta,
    summary: AdminAccountSummaryData,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminAccountPageEnvelope {
    code: u32,
    message: String,
    data: AdminAccountPageData,
}

impl AdminAccountPageEnvelope {
    fn cursor(page: Page<AdminAccountData>, limit: u32, summary: AdminAccountSummaryData) -> Self {
        Self {
            code: 200,
            message: "OK".into(),
            data: AdminAccountPageData {
                items: page.items,
                page: PageMeta::Cursor(CursorPageMeta {
                    limit,
                    next_cursor: page.next_cursor,
                }),
                summary,
            },
        }
    }

    fn numbered(
        page: crate::infra::json::NumberedPage<AdminAccountData>,
        summary: AdminAccountSummaryData,
    ) -> Self {
        Self {
            code: 200,
            message: "OK".into(),
            data: AdminAccountPageData {
                page: PageMeta::Numbered(NumberedPageMeta {
                    page: page.page,
                    page_size: page.page_size,
                    total: page.total,
                    total_pages: total_pages(page.total, page.page_size),
                }),
                items: page.items,
                summary,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchDeleteAccountsData {
    deleted: u32,
    not_found: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchUpdateAccountStatusData {
    updated: u32,
    not_found: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
enum AccountUpdateData {
    Account(Box<AdminAccountData>),
    BatchStatus(BatchUpdateAccountStatusData),
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ResetAccountUsageData {
    id: String,
    reset: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountCookiesData {
    cookies: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminAccountExportData {
    id: String,
    email: Option<String>,
    account_id: Option<String>,
    user_id: Option<String>,
    label: Option<String>,
    plan_type: Option<String>,
    status: String,
    access_token_expires_at: Option<String>,
    added_at: String,
    updated_at: String,
    token: String,
    refresh_token: Option<String>,
}

impl From<StoredAccount> for AdminAccountExportData {
    fn from(a: StoredAccount) -> Self {
        Self {
            id: a.id,
            email: a.email,
            account_id: a.account_id,
            user_id: a.user_id,
            label: a.label,
            plan_type: a.plan_type,
            status: account_status_str(a.status).to_string(),
            access_token_expires_at: a.access_token_expires_at.map(|dt| china_rfc3339(&dt)),
            added_at: china_rfc3339_str(&a.added_at),
            updated_at: china_rfc3339_str(&a.updated_at),
            token: a.access_token.expose_secret().to_string(),
            refresh_token: a.refresh_token.map(|t| t.expose_secret().to_string()),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountExportData {
    source_format: &'static str,
    accounts: Vec<AdminAccountExportData>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountImportData {
    imported: u32,
    skipped: u32,
    source_format: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthCheckData {
    summary: HealthCheckSummary,
    results: Vec<Value>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthCheckSummary {
    total: usize,
    alive: usize,
    dead: usize,
    skipped: usize,
}

// ============================================================================
// Handlers
// ============================================================================

/// `GET /api/admin/accounts`
pub(crate) async fn accounts(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(params): Query<AccountsQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    let limit = clamp_limit(params.page_size.or(params.limit).unwrap_or(50));
    let use_numbered_page = params.page.is_some() || params.page_size.is_some();
    let search = params.search.clone();
    let stats = account_list_stats(&state).await;
    let summary = account_summary_data(&state, &stats, search.as_deref()).await;

    if use_numbered_page {
        return match state
            .services
            .admin_accounts
            .list_page(clamp_page(params.page.unwrap_or(1)), limit, params.search)
            .await
        {
            Ok(page) => {
                let page = crate::infra::json::NumberedPage {
                    items: page
                        .items
                        .into_iter()
                        .map(|item| stats.data_for(item))
                        .collect(),
                    total: page.total,
                    page: page.page,
                    page_size: page.page_size,
                };
                Ok(AdminResponse::new(
                    StatusCode::OK,
                    AdminAccountPageEnvelope::numbered(page, summary),
                ))
            }
            Err(error) => Err(account_error(error, request_id)),
        };
    }

    match state
        .services
        .admin_accounts
        .list(params.cursor, limit)
        .await
    {
        Ok(page) => {
            let page = Page {
                items: page
                    .items
                    .into_iter()
                    .map(|item| stats.data_for(item))
                    .collect(),
                next_cursor: page.next_cursor,
            };
            Ok(AdminResponse::new(
                StatusCode::OK,
                AdminAccountPageEnvelope::cursor(page, limit, summary),
            ))
        }
        Err(error) => Err(account_error(error, request_id)),
    }
}

/// `POST /api/admin/accounts`
pub(crate) async fn create_account(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<CreateAccountRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    match state
        .services
        .admin_accounts
        .create(payload.token, payload.refresh_token)
        .await
    {
        Ok(account) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(AdminAccountData::from(account), request_id),
        )),
        Err(error) => Err(account_error(error, request_id)),
    }
}

/// `POST /api/admin/accounts/refresh`
pub(crate) async fn refresh_account(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<AccountActionRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    let account_id = payload.id;
    require_admin_session(&state, &headers, &request_id).await?;
    match state
        .services
        .admin_accounts
        .refresh_account(&account_id)
        .await
    {
        Ok(account) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(AdminAccountData::from(account), request_id),
        )),
        Err(error) => Err(account_error(error, request_id)),
    }
}

/// `POST /api/admin/accounts/reset-usage`
pub(crate) async fn reset_account_usage(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<AccountActionRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    let account_id = payload.id;
    require_admin_session(&state, &headers, &request_id).await?;
    match state.services.admin_accounts.reset_usage(&account_id).await {
        Ok(account) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(
                ResetAccountUsageData {
                    id: account.id,
                    reset: true,
                },
                request_id,
            ),
        )),
        Err(error) => Err(account_error(error, request_id)),
    }
}

/// `GET /api/admin/accounts/cookies`
pub(crate) async fn get_account_cookies(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<AccountIdQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    let account_id = query.id;
    require_admin_session(&state, &headers, &request_id).await?;
    match state.services.admin_accounts.cookies(&account_id).await {
        Ok(cookies) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(AccountCookiesData { cookies }, request_id),
        )),
        Err(error) => Err(account_error(error, request_id)),
    }
}

/// `POST /api/admin/accounts/cookies`
pub(crate) async fn set_account_cookies(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<SetAccountCookiesRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    let account_id = payload.id;
    require_admin_session(&state, &headers, &request_id).await?;
    match state
        .services
        .admin_accounts
        .set_cookies(&account_id, payload.cookies)
        .await
    {
        Ok(cookies) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(AccountCookiesData { cookies }, request_id),
        )),
        Err(error) => Err(account_error(error, request_id)),
    }
}

/// `GET /api/admin/accounts/quota`
pub(crate) async fn account_quota(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<AccountIdQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    let account_id = query.id;
    require_admin_session(&state, &headers, &request_id).await?;
    match state
        .services
        .admin_accounts
        .account_quota(&account_id)
        .await
    {
        Ok(data) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(data, request_id),
        )),
        Err(AdminAccountError::NotFound) => Err(account_not_found(request_id)),
        Err(AdminAccountError::Inactive(status)) => Err(AdminError::new(
            StatusCode::CONFLICT,
            40901,
            format!(
                "Account is {}, cannot query quota",
                account_status_str(status)
            ),
            request_id,
        )),
        Err(AdminAccountError::FetchQuota(msg)) => Err(AdminError::new(
            StatusCode::BAD_GATEWAY,
            50201,
            format!("Failed to fetch quota from Codex API: {msg}"),
            request_id,
        )),
        Err(e) => Err(account_error(e, request_id)),
    }
}

/// `GET /api/admin/accounts/quota-warnings`
pub(crate) async fn quota_warnings(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    match state.services.admin_accounts.quota_warnings().await {
        Ok(data) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(data, request_id),
        )),
        Err(error) => Err(account_error(error, request_id)),
    }
}

/// `POST /api/admin/accounts/health-check`
pub(crate) async fn health_check_accounts(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    let payload = parse_health_check_request(&body, &request_id)?;
    validate_health_check_request(&payload, &request_id)?;
    require_admin_session(&state, &headers, &request_id).await?;

    let mut req = serde_json::json!({});
    if let Some(ids) = &payload.ids {
        req["ids"] = serde_json::json!(ids);
    }

    match state
        .services
        .admin_accounts
        .health_check_accounts(req)
        .await
    {
        Ok(result) => {
            let summary = HealthCheckSummary {
                total: result
                    .get("summary")
                    .and_then(|s| s.get("total").and_then(Value::as_u64))
                    .unwrap_or(0) as usize,
                alive: result
                    .get("summary")
                    .and_then(|s| s.get("alive").and_then(Value::as_u64))
                    .unwrap_or(0) as usize,
                dead: result
                    .get("summary")
                    .and_then(|s| s.get("dead").and_then(Value::as_u64))
                    .unwrap_or(0) as usize,
                skipped: result
                    .get("summary")
                    .and_then(|s| s.get("skipped").and_then(Value::as_u64))
                    .unwrap_or(0) as usize,
            };
            let results = result
                .get("results")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            Ok(AdminResponse::new(
                StatusCode::OK,
                AdminEnvelope::ok(HealthCheckData { summary, results }, request_id),
            ))
        }
        Err(error) => Err(account_error(error, request_id)),
    }
}

/// `GET /api/admin/accounts/export`
pub(crate) async fn export_accounts(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<AccountExportQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    validate_account_export_format(query.format.as_deref())
        .map_err(|msg| AdminError::new(StatusCode::BAD_REQUEST, 40001, msg, request_id.clone()))?;
    let ids = account_export_ids(query.ids.as_deref());
    match state.services.admin_accounts.export_with_tokens(ids).await {
        Ok(accounts) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(
                AccountExportData {
                    source_format: "native",
                    accounts: accounts
                        .into_iter()
                        .map(AdminAccountExportData::from)
                        .collect(),
                },
                request_id,
            ),
        )),
        Err(error) => Err(account_error(error, request_id)),
    }
}

/// `POST /api/admin/accounts/import`
pub(crate) async fn import_accounts(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    match state.services.admin_accounts.import(payload).await {
        Ok(result) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(
                AccountImportData {
                    imported: result.imported,
                    skipped: result.skipped,
                    source_format: result.source_format.to_string(),
                },
                request_id,
            ),
        )),
        Err(error) => Err(account_error(error, request_id)),
    }
}

/// `POST /api/admin/accounts/delete`
pub(crate) async fn batch_delete_accounts(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<BatchDeleteAccountsRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    match state
        .services
        .admin_accounts
        .batch_delete(payload.ids)
        .await
    {
        Ok(result) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(
                BatchDeleteAccountsData {
                    deleted: result.deleted,
                    not_found: result.not_found,
                },
                request_id,
            ),
        )),
        Err(error) => Err(account_error(error, request_id)),
    }
}

/// `POST /api/admin/accounts/update`
pub(crate) async fn update_account(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    match parse_account_update(payload, &request_id)? {
        ParsedAccountUpdate::Single { id, update } => {
            match state
                .services
                .admin_accounts
                .update_metadata(&id, update)
                .await
            {
                Ok(Some(account)) => Ok(AdminResponse::new(
                    StatusCode::OK,
                    AdminEnvelope::ok(
                        AccountUpdateData::Account(Box::new(account.into())),
                        request_id,
                    ),
                )),
                Ok(None) => Err(account_not_found(request_id)),
                Err(error) => Err(account_error(error, request_id)),
            }
        }
        ParsedAccountUpdate::BatchStatus { ids, status } => match state
            .services
            .admin_accounts
            .batch_update_status(ids, &status)
            .await
        {
            Ok(result) => Ok(AdminResponse::new(
                StatusCode::OK,
                AdminEnvelope::ok(
                    AccountUpdateData::BatchStatus(BatchUpdateAccountStatusData {
                        updated: result.updated,
                        not_found: result.not_found,
                    }),
                    request_id,
                ),
            )),
            Err(error) => Err(account_error(error, request_id)),
        },
    }
}

async fn account_list_stats(state: &AppState) -> AccountListStats {
    let usage_records = list_all_usage_records(state).await;
    let quota_snapshots = state
        .services
        .background_tasks
        .accounts
        .list_quota_snapshots()
        .await
        .unwrap_or_default();
    let logs = list_all_event_logs(state).await;

    AccountListStats {
        usage_by_account: usage_records
            .into_iter()
            .map(|usage| (usage.account_id.clone(), usage))
            .collect(),
        quota_by_account: quota_snapshots
            .into_iter()
            .map(|snapshot| {
                (
                    snapshot.account_id,
                    quota_data(&snapshot.quota_json, snapshot.quota_fetched_at),
                )
            })
            .collect(),
        models_by_account: models_by_account(&logs),
    }
}

async fn account_summary_data(
    state: &AppState,
    stats: &AccountListStats,
    search: Option<&str>,
) -> AdminAccountSummaryData {
    let accounts = list_all_account_metadata(state, search).await;
    let total = accounts.len() as u64;
    let active = accounts
        .iter()
        .filter(|account| account.status == AccountStatus::Active)
        .count() as u64;
    let high_usage = accounts
        .iter()
        .filter(|account| {
            stats
                .quota_by_account
                .get(&account.id)
                .is_some_and(account_quota_has_high_usage)
        })
        .count() as u64;
    let attention = accounts
        .iter()
        .filter(|account| account_summary_needs_attention(account.status))
        .count() as u64;

    AdminAccountSummaryData {
        total,
        active,
        high_usage,
        attention,
    }
}

async fn list_all_account_metadata(
    state: &AppState,
    search: Option<&str>,
) -> Vec<AdminAccountMetadata> {
    let mut page = 1;
    let mut accounts = Vec::new();
    loop {
        let Ok(result) = state
            .services
            .admin_accounts
            .list_page(
                page,
                ACCOUNT_STATS_PAGE_LIMIT,
                search.map(ToString::to_string),
            )
            .await
        else {
            return Vec::new();
        };
        let total = result.total;
        accounts.extend(result.items);
        if accounts.len() as u64 >= total || total == 0 {
            return accounts;
        }
        page = page.saturating_add(1);
    }
}

fn account_quota_has_high_usage(quota: &AdminAccountQuotaData) -> bool {
    quota
        .windows
        .iter()
        .any(|window| window.used_percent.is_some_and(|percent| percent >= 80.0))
}

fn account_summary_needs_attention(status: AccountStatus) -> bool {
    matches!(
        status,
        AccountStatus::Expired | AccountStatus::Disabled | AccountStatus::Banned
    )
}

async fn list_all_usage_records(state: &AppState) -> Vec<AdminUsageRecord> {
    let mut cursor = None;
    let mut records = Vec::new();
    loop {
        let Ok(page) = state
            .services
            .usage
            .list(cursor, ACCOUNT_STATS_PAGE_LIMIT)
            .await
        else {
            return Vec::new();
        };
        records.extend(page.items);
        let Some(next_cursor) = page.next_cursor else {
            return records;
        };
        cursor = Some(next_cursor);
    }
}

async fn list_all_event_logs(state: &AppState) -> Vec<EventLog> {
    let mut cursor = None;
    let mut logs = Vec::new();
    loop {
        let Ok(page) = state
            .services
            .logs
            .list(cursor, ACCOUNT_STATS_PAGE_LIMIT, AdminLogFilter::default())
            .await
        else {
            return Vec::new();
        };
        logs.extend(page.items);
        let Some(next_cursor) = page.next_cursor else {
            return logs;
        };
        cursor = Some(next_cursor);
    }
}

#[derive(Debug, Clone, Default)]
struct ModelUsageAggregate {
    model: String,
    request_count: u64,
    error_count: u64,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    last_used_at: Option<DateTime<Utc>>,
}

fn models_by_account(logs: &[EventLog]) -> HashMap<String, Vec<AdminAccountModelUsageData>> {
    let mut by_account_model = HashMap::<(String, String), ModelUsageAggregate>::new();
    for log in logs {
        let Some(account_id) = log
            .account_id
            .as_ref()
            .filter(|value| !value.trim().is_empty())
        else {
            continue;
        };
        let Some(model) = log.model.as_ref().filter(|value| !value.trim().is_empty()) else {
            continue;
        };
        let aggregate = by_account_model
            .entry((account_id.clone(), model.clone()))
            .or_insert_with(|| ModelUsageAggregate {
                model: model.clone(),
                ..ModelUsageAggregate::default()
            });
        aggregate.request_count += 1;
        if log.level == EventLevel::Error || log.status_code.is_some_and(|status| status >= 400) {
            aggregate.error_count += 1;
        }
        aggregate.input_tokens += metadata_usage_number(&log.metadata, "inputTokens");
        aggregate.output_tokens += metadata_usage_number(&log.metadata, "outputTokens");
        aggregate.cached_tokens += metadata_usage_number(&log.metadata, "cachedTokens");
        aggregate.last_used_at = Some(
            aggregate
                .last_used_at
                .map_or(log.created_at, |last| last.max(log.created_at)),
        );
    }

    let mut by_account = HashMap::<String, Vec<ModelUsageAggregate>>::new();
    for ((account_id, _), aggregate) in by_account_model {
        by_account.entry(account_id).or_default().push(aggregate);
    }

    by_account
        .into_iter()
        .map(|(account_id, mut aggregates)| {
            aggregates.sort_by(|a, b| {
                b.request_count
                    .cmp(&a.request_count)
                    .then_with(|| b.last_used_at.cmp(&a.last_used_at))
            });
            (
                account_id,
                aggregates.into_iter().map(model_usage_data).collect(),
            )
        })
        .collect()
}

fn model_usage_data(usage: ModelUsageAggregate) -> AdminAccountModelUsageData {
    let total_tokens = usage.input_tokens + usage.output_tokens;
    let success_rate = if usage.request_count > 0 {
        ((usage.request_count - usage.error_count) as f64 / usage.request_count as f64 * 1000.0)
            .round()
            / 10.0
    } else {
        0.0
    };
    let total_cost_usd = billing::calculate_cost(
        usage.input_tokens,
        usage.output_tokens,
        usage.cached_tokens,
        &usage.model,
        None,
    );

    AdminAccountModelUsageData {
        model: usage.model,
        request_count: usage.request_count,
        request_count_display: format_count(usage.request_count),
        success_rate,
        success_rate_display: format_percent(success_rate),
        input_tokens: usage.input_tokens,
        input_tokens_display: format_tokens(usage.input_tokens),
        output_tokens: usage.output_tokens,
        output_tokens_display: format_tokens(usage.output_tokens),
        cached_tokens: usage.cached_tokens,
        cached_tokens_display: format_tokens(usage.cached_tokens),
        total_tokens,
        total_tokens_display: format_tokens(total_tokens),
        total_cost_usd,
        total_cost_usd_display: format_cost(total_cost_usd),
        last_used_at: usage.last_used_at.map(|value| china_rfc3339(&value)),
        last_used_at_display: china_relative_time(usage.last_used_at, Utc::now()),
    }
}

fn quota_data(quota_json: &str, fetched_at: Option<DateTime<Utc>>) -> AdminAccountQuotaData {
    let quota = serde_json::from_str::<Value>(quota_json).unwrap_or(Value::Null);
    let windows = quota_windows(&quota);

    AdminAccountQuotaData {
        refreshed_at_display: china_relative_time(fetched_at, Utc::now()),
        windows,
    }
}

fn quota_windows(quota: &Value) -> Vec<AdminAccountQuotaWindowData> {
    let mut windows = Vec::new();

    let has_monthly_limit = push_monthly_quota_window(&mut windows, quota.get("monthly_limit"));
    if let Some(snapshots) = quota.get("snapshots").and_then(Value::as_array) {
        for snapshot in snapshots {
            push_snapshot_quota_windows(&mut windows, snapshot, has_monthly_limit);
        }
    }

    windows.sort_by_key(quota_window_sort_key);
    windows
}

fn push_monthly_quota_window(
    windows: &mut Vec<AdminAccountQuotaWindowData>,
    monthly_limit: Option<&Value>,
) -> bool {
    let Some(monthly_limit) = monthly_limit.filter(|value| !value.is_null()) else {
        return false;
    };
    let used_percent = monthly_limit
        .get("used_percent")
        .and_then(number_value)
        .map(|value| value.clamp(0.0, 100.0));
    let reset_at = monthly_limit
        .get("reset_at")
        .and_then(Value::as_i64)
        .and_then(|value| DateTime::<Utc>::from_timestamp(value, 0));
    let window_seconds = monthly_limit
        .get("window_minutes")
        .and_then(Value::as_u64)
        .and_then(|minutes| minutes.checked_mul(60))
        .or(Some(MONTH_WINDOW_SECONDS));
    if used_percent.is_none() && reset_at.is_none() {
        return false;
    }

    let key = monthly_limit
        .get("key")
        .and_then(Value::as_str)
        .unwrap_or("monthly");
    windows.push(AdminAccountQuotaWindowData {
        key: quota_key_segment(key),
        group: "monthly".to_string(),
        window_seconds,
        label_display: "月限额".to_string(),
        used_percent,
        used_percent_display: used_percent
            .map(format_percent)
            .unwrap_or_else(|| "-".to_string()),
        reset_at_display: reset_at
            .as_ref()
            .map(china_datetime)
            .unwrap_or_else(|| "-".to_string()),
        window_used_display: quota_window_used_display(reset_at, window_seconds),
    });
    true
}

fn push_snapshot_quota_windows(
    windows: &mut Vec<AdminAccountQuotaWindowData>,
    snapshot: &Value,
    skip_core_monthly: bool,
) {
    let source = snapshot
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("quota");
    let source_key = snapshot_source_key(source, snapshot);
    let label_prefix = snapshot_label(snapshot);
    for role in ["primary", "secondary"] {
        let Some(window) = snapshot.get(role).filter(|value| !value.is_null()) else {
            continue;
        };
        let window_seconds = window
            .get("window_minutes")
            .and_then(Value::as_u64)
            .and_then(|minutes| minutes.checked_mul(60));
        if skip_core_monthly
            && source == "core"
            && window_seconds
                .is_some_and(|seconds| quota_window_matches(seconds, MONTH_WINDOW_SECONDS))
        {
            continue;
        }
        push_quota_window(
            windows,
            &source_key,
            role,
            label_prefix.as_deref(),
            Some(window),
        );
    }
}

fn push_quota_window(
    windows: &mut Vec<AdminAccountQuotaWindowData>,
    source_key: &str,
    role: &str,
    label_prefix: Option<&str>,
    window: Option<&Value>,
) {
    let Some(window) = window.filter(|value| !value.is_null()) else {
        return;
    };
    let used_percent = window
        .get("used_percent")
        .and_then(number_value)
        .map(|value| value.clamp(0.0, 100.0));
    let reset_at = window
        .get("reset_at")
        .and_then(Value::as_i64)
        .and_then(|value| DateTime::<Utc>::from_timestamp(value, 0));
    let window_seconds = window
        .get("window_minutes")
        .and_then(Value::as_u64)
        .and_then(|minutes| minutes.checked_mul(60));
    if used_percent.is_none() && reset_at.is_none() && window_seconds.is_none() {
        return;
    }
    let base_label = quota_window_label_display(window_seconds);
    let label_display = label_prefix
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!("{} · {}", value.trim(), base_label))
        .unwrap_or(base_label);

    windows.push(AdminAccountQuotaWindowData {
        key: unique_quota_window_key(windows, source_key, role, window_seconds),
        group: quota_window_group(window_seconds).to_string(),
        window_seconds,
        label_display,
        used_percent,
        used_percent_display: used_percent
            .map(format_percent)
            .unwrap_or_else(|| "-".to_string()),
        reset_at_display: reset_at
            .as_ref()
            .map(china_datetime)
            .unwrap_or_else(|| "-".to_string()),
        window_used_display: quota_window_used_display(reset_at, window_seconds),
    });
}

fn snapshot_source_key(source: &str, snapshot: &Value) -> String {
    let label = snapshot
        .get("limit_name")
        .and_then(Value::as_str)
        .or_else(|| snapshot.get("metered_feature").and_then(Value::as_str))
        .unwrap_or(source);
    format!("{}-{}", quota_key_segment(source), quota_key_segment(label))
}

fn snapshot_label(snapshot: &Value) -> Option<String> {
    let source = snapshot.get("source").and_then(Value::as_str);
    if source == Some("core") {
        return None;
    }
    let label = snapshot
        .get("limit_name")
        .and_then(Value::as_str)
        .or_else(|| snapshot.get("metered_feature").and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    if label.eq_ignore_ascii_case("codex") {
        return None;
    }
    if is_review_limit_label(Some(label)) {
        return Some("代码审查".to_string());
    }
    Some(label.to_string())
}

fn unique_quota_window_key(
    windows: &[AdminAccountQuotaWindowData],
    source_key: &str,
    role: &str,
    window_seconds: Option<u64>,
) -> String {
    let bucket = quota_window_key_part(window_seconds).unwrap_or(role);
    let key = format!("{source_key}-{bucket}");
    if windows.iter().any(|window| window.key == key) {
        format!("{key}-{role}")
    } else {
        key
    }
}

fn quota_window_key_part(window_seconds: Option<u64>) -> Option<&'static str> {
    match window_seconds {
        Some(seconds) if quota_window_matches(seconds, FIVE_HOUR_WINDOW_SECONDS) => {
            Some("five-hour")
        }
        Some(seconds) if quota_window_matches(seconds, WEEK_WINDOW_SECONDS) => Some("weekly"),
        Some(seconds) if quota_window_matches(seconds, MONTH_WINDOW_SECONDS) => Some("monthly"),
        _ => None,
    }
}

fn quota_key_segment(value: &str) -> String {
    let mut segment = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            segment.push(ch.to_ascii_lowercase());
        } else if !segment.ends_with('-') {
            segment.push('-');
        }
    }
    let segment = segment.trim_matches('-');
    if segment.is_empty() {
        "quota".to_string()
    } else {
        segment.to_string()
    }
}

fn quota_window_sort_key(window: &AdminAccountQuotaWindowData) -> (u8, u64, String) {
    let group_order = match window.group.as_str() {
        "monthly" => 0,
        "shortTerm" => 1,
        _ => 2,
    };
    (
        group_order,
        window.window_seconds.unwrap_or(0),
        window.key.clone(),
    )
}

fn quota_window_group(window_seconds: Option<u64>) -> &'static str {
    match window_seconds {
        Some(seconds) if quota_window_matches(seconds, MONTH_WINDOW_SECONDS) => "monthly",
        Some(seconds)
            if quota_window_matches(seconds, FIVE_HOUR_WINDOW_SECONDS)
                || quota_window_matches(seconds, WEEK_WINDOW_SECONDS) =>
        {
            "shortTerm"
        }
        _ => "other",
    }
}

fn quota_window_matches(actual: u64, expected: u64) -> bool {
    actual > 0 && actual.abs_diff(expected) <= expected / 20
}

fn quota_window_label_display(window_seconds: Option<u64>) -> String {
    let Some(window_seconds) = window_seconds.filter(|seconds| *seconds > 0) else {
        return "额度".to_string();
    };
    match window_seconds {
        seconds if quota_window_matches(seconds, FIVE_HOUR_WINDOW_SECONDS) => {
            "5小时限额".to_string()
        }
        seconds if quota_window_matches(seconds, WEEK_WINDOW_SECONDS) => "周限额".to_string(),
        seconds if quota_window_matches(seconds, MONTH_WINDOW_SECONDS) => "月限额".to_string(),
        seconds if seconds % 86_400 == 0 => format!("{}天限额", seconds / 86_400),
        seconds if seconds % 3_600 == 0 => format!("{}小时限额", seconds / 3_600),
        seconds => format!("{}分钟限额", seconds.div_ceil(60)),
    }
}

fn quota_window_used_display(
    reset_at: Option<DateTime<Utc>>,
    window_seconds: Option<u64>,
) -> String {
    let (Some(reset_at), Some(window_seconds)) = (reset_at, window_seconds) else {
        return "-".to_string();
    };
    let remaining = reset_at
        .signed_duration_since(Utc::now())
        .num_seconds()
        .max(0) as u64;
    let used = window_seconds.saturating_sub(remaining);
    format!(
        "{} / {}",
        format_duration_days(used),
        format_duration_days(window_seconds)
    )
}

fn format_duration_days(seconds: u64) -> String {
    let days = seconds as f64 / 86_400.0;
    if days >= 1.0 {
        format!("{days:.1}d")
    } else {
        format!("{:.1}h", seconds as f64 / 3_600.0)
    }
}

fn metadata_usage_number(metadata: &Value, field: &str) -> u64 {
    metadata
        .get("usage")
        .and_then(|usage| usage.get(field))
        .or_else(|| metadata.get(field))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

fn number_value(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|value| value.parse::<f64>().ok()))
        .filter(|value| value.is_finite())
}

fn is_review_limit_label(value: Option<&str>) -> bool {
    let normalized = value
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .replace(['-', ' '], "_");
    normalized == "review"
        || normalized == "code_review"
        || normalized == "codex_review"
        || normalized == "codex_code_review"
        || normalized.contains("code_review")
        || normalized.contains("codex_review")
}

fn nonnegative_i64_to_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn format_count(value: u64) -> String {
    value.to_string()
}

fn format_tokens(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.1}M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1}K", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

fn format_percent(value: f64) -> String {
    format!("{value:.1}%")
}

fn format_cost(value: f64) -> String {
    format!("${value:.2}")
}

// ============================================================================
// Error handling
// ============================================================================

enum ParsedAccountUpdate {
    Single {
        id: String,
        update: AdminAccountMetadataUpdate,
    },
    BatchStatus {
        ids: Vec<String>,
        status: String,
    },
}

fn parse_account_update(
    payload: Value,
    request_id: &str,
) -> Result<ParsedAccountUpdate, AdminError> {
    let object = payload.as_object().ok_or_else(|| {
        AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "Account update request must be an object",
            request_id,
        )
    })?;
    if object.contains_key("ids") {
        if object.contains_key("id") {
            return Err(AdminError::new(
                StatusCode::BAD_REQUEST,
                40001,
                "Account batch update must not include id",
                request_id,
            ));
        }
        if ["label", "email", "accountId", "userId", "planType"]
            .iter()
            .any(|field| object.contains_key(*field))
        {
            return Err(AdminError::new(
                StatusCode::BAD_REQUEST,
                40001,
                "Account batch update only supports status",
                request_id,
            ));
        }
        let ids = required_string_array_field(object, "ids", request_id)?;
        let status = required_string_field(object, "status", request_id)?;
        return Ok(ParsedAccountUpdate::BatchStatus { ids, status });
    }
    let id = required_string_field(object, "id", request_id)?;
    let label = optional_string_update_field(object, "label", request_id)?;
    let email = optional_string_update_field(object, "email", request_id)?;
    let account_id = optional_string_update_field(object, "accountId", request_id)?;
    let user_id = optional_string_update_field(object, "userId", request_id)?;
    let plan_type = optional_string_update_field(object, "planType", request_id)?;
    let status = object
        .get("status")
        .map(|value| required_string_value(value, "status", request_id))
        .transpose()?;
    let update = AdminAccountMetadataUpdate {
        email,
        account_id,
        user_id,
        label,
        plan_type,
        status,
    };
    if !update.any() {
        return Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "Account update request must include editable fields",
            request_id,
        ));
    }
    Ok(ParsedAccountUpdate::Single { id, update })
}

fn required_string_field(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
    request_id: &str,
) -> Result<String, AdminError> {
    let Some(value) = object.get(field) else {
        return Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            format!("{field} is required"),
            request_id,
        ));
    };
    required_string_value(value, field, request_id)
}

fn required_string_value(
    value: &Value,
    field: &'static str,
    request_id: &str,
) -> Result<String, AdminError> {
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| {
            AdminError::new(
                StatusCode::BAD_REQUEST,
                40001,
                format!("{field} must be a non-empty string"),
                request_id,
            )
        })
}

fn required_string_array_field(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
    request_id: &str,
) -> Result<Vec<String>, AdminError> {
    let Some(value) = object.get(field) else {
        return Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            format!("{field} is required"),
            request_id,
        ));
    };
    let Some(values) = value.as_array() else {
        return Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            format!("{field} must be an array of non-empty strings"),
            request_id,
        ));
    };
    if values.is_empty() {
        return Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            format!("{field} must not be empty"),
            request_id,
        ));
    }
    values
        .iter()
        .map(|value| required_string_value(value, field, request_id))
        .collect()
}

fn optional_string_field(
    value: &Value,
    field: &'static str,
    request_id: &str,
) -> Result<Option<String>, AdminError> {
    if value.is_null() {
        return Ok(None);
    }
    match value.as_str() {
        Some(value) => {
            let value = value.trim();
            Ok((!value.is_empty()).then(|| value.to_string()))
        }
        None => Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            format!("{field} must be a string or null"),
            request_id,
        )),
    }
}

fn optional_string_update_field(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
    request_id: &str,
) -> Result<Option<Option<String>>, AdminError> {
    object
        .get(field)
        .map(|value| optional_string_field(value, field, request_id))
        .transpose()
}

fn account_error(error: AdminAccountError, request_id: String) -> AdminError {
    match error {
        AdminAccountError::InvalidStatus(_)
        | AdminAccountError::LabelTooLong
        | AdminAccountError::EmptyIds
        | AdminAccountError::NoImportableAccounts
        | AdminAccountError::InvalidAccessTokenExpiresAt
        | AdminAccountError::TokenRequired
        | AdminAccountError::InvalidToken(_)
        | AdminAccountError::RefreshTokenExchange(_)
        | AdminAccountError::NoValidCookies => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            error.to_string(),
            request_id,
        ),
        AdminAccountError::NotFound => account_not_found(request_id),
        AdminAccountError::Inactive(_) => {
            AdminError::new(StatusCode::CONFLICT, 40901, error.to_string(), request_id)
        }
        _ => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            error.to_string(),
            request_id,
        ),
    }
}

fn account_not_found(request_id: String) -> AdminError {
    AdminError::new(
        StatusCode::NOT_FOUND,
        40401,
        "Account not found",
        request_id,
    )
}

fn account_status_str(status: crate::upstream::accounts::model::AccountStatus) -> &'static str {
    match status {
        crate::upstream::accounts::model::AccountStatus::Active => "active",
        crate::upstream::accounts::model::AccountStatus::Expired => "expired",
        crate::upstream::accounts::model::AccountStatus::QuotaExhausted => "quota_exhausted",
        crate::upstream::accounts::model::AccountStatus::Refreshing => "refreshing",
        crate::upstream::accounts::model::AccountStatus::Disabled => "disabled",
        crate::upstream::accounts::model::AccountStatus::Banned => "banned",
    }
}

fn parse_health_check_request(
    body: &Bytes,
    request_id: &str,
) -> Result<HealthCheckRequest, AdminError> {
    if body.is_empty() {
        return Ok(HealthCheckRequest::default());
    }
    serde_json::from_slice(body).map_err(|_| {
        AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "Invalid health check request",
            request_id,
        )
    })
}

fn validate_health_check_request(
    payload: &HealthCheckRequest,
    request_id: &str,
) -> Result<(), AdminError> {
    if payload.ids.as_ref().is_some_and(Vec::is_empty) {
        return Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "Account ids must not be empty",
            request_id,
        ));
    }
    if payload
        .stagger_ms
        .is_some_and(|value| !(500..=30_000).contains(&value))
    {
        return Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "staggerMs must be between 500 and 30000",
            request_id,
        ));
    }
    if payload
        .concurrency
        .is_some_and(|value| !(1..=10).contains(&value))
    {
        return Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "concurrency must be between 1 and 10",
            request_id,
        ));
    }
    Ok(())
}

fn validate_account_export_format(value: Option<&str>) -> Result<(), &'static str> {
    match value.unwrap_or("native").trim() {
        "" | "native" => Ok(()),
        _ => Err("Unsupported account export format"),
    }
}

fn account_export_ids(value: Option<&str>) -> Vec<String> {
    value
        .into_iter()
        .flat_map(|ids| ids.split(','))
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(ToString::to_string)
        .collect()
}
