//! 账号管理 HTTP 处理器。

use axum::{
    body::Body,
    extract::{Query, State},
    http::{
        header::{CACHE_CONTROL, CONNECTION, CONTENT_TYPE},
        StatusCode,
    },
    response::{IntoResponse, Response},
    Json,
};
use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{QueryBuilder, Row, Sqlite};

use crate::{
    admin::auth::session::AdminAuth,
    admin::response::{
        AdminEnvelope, AdminError, AdminResponse, BatchDeleteData, CursorPageMeta,
        NumberedPageMeta, PageMeta, ADMIN_OK_CODE, ADMIN_OK_MESSAGE,
    },
    admin::update_payload::{parse_editable_update, EditableUpdateMessages},
    admin::{
        accounts::quota_view::{
            quota_data, AdminAccountQuotaData, AdminAccountQuotaUsageWindow,
            AdminAccountQuotaWindowLocalUsage,
        },
        accounts::service::{
            AdminAccountError, AdminAccountHealthCheck, AdminAccountMetadata,
            AdminAccountRefreshOutcome, AdminAccountRefreshResult, AdminAccountUpdate,
            OAuthExchangeInput,
        },
        monitoring::{account_usage_service::AdminUsageRecord, billing},
    },
    infra::{
        format::{
            format_cost, format_percent, format_plain_number, format_tokens, nonnegative_i64_to_u64,
        },
        json::{clamp_limit, clamp_page, total_pages, Page},
        time::{
            china_datetime, china_quarter_hour_start, china_relative_time, china_rfc3339,
            parse_rfc3339_utc,
        },
    },
    runtime::state::AppState,
    upstream::accounts::{model::AccountStatus, token_refresh::token_refresh_status_eligible},
};

const ACCOUNT_STATS_PAGE_LIMIT: u32 = 200;
const ACCOUNT_EXPORT_CONFIRMATION: &str = "export_sensitive_accounts";

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
    confirm: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountTestQuery {
    id: String,
    model_id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountOAuthExchangeRequest {
    session_id: String,
    callback_url: Option<String>,
    code: Option<String>,
    state: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountModelsData {
    models: Vec<AccountModelData>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountModelData {
    id: String,
    label: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminAccountData {
    id: String,
    email: Option<String>,
    account_id: Option<String>,
    user_id: Option<String>,
    label: Option<String>,
    plan_type: Option<String>,
    has_refresh_token: bool,
    status: String,
    display_status: String,
    token_refreshing: bool,
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
        Self::from_parts(a, None, None, Vec::new(), false)
    }
}

impl AdminAccountData {
    fn from_parts(
        a: AdminAccountMetadata,
        usage: Option<&AdminUsageRecord>,
        quota: Option<AdminAccountQuotaData>,
        models: Vec<AdminAccountModelUsageData>,
        token_refreshing: bool,
    ) -> Self {
        let access_token_expires_at = a.access_token_expires_at.as_ref().map(china_rfc3339);
        let access_token_expires_at_display =
            a.access_token_expires_at.as_ref().map(china_datetime);
        let display_status = account_display_status(a.status, token_refreshing).to_string();
        Self {
            id: a.id,
            email: a.email,
            account_id: a.account_id,
            user_id: a.user_id,
            label: a.label,
            plan_type: a.plan_type,
            has_refresh_token: a.has_refresh_token,
            status: a.status.as_str().to_string(),
            display_status,
            token_refreshing,
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
        let request_count = usage.map_or(0, |usage| usage.window_request_count);
        let empty_response_count = 0;
        let input_tokens = usage.map_or(0, |usage| usage.window_input_tokens);
        let output_tokens = usage.map_or(0, |usage| usage.window_output_tokens);
        let cached_tokens = usage.map_or(0, |usage| usage.window_cached_tokens);
        let reasoning_tokens = 0;
        let total_tokens = input_tokens.saturating_add(output_tokens);
        let image_input_tokens = 0;
        let image_output_tokens = 0;
        let image_request_count = 0;
        let image_request_failed_count = 0;
        let created_tokens = input_tokens.saturating_sub(cached_tokens);
        let read_tokens = cached_tokens;
        let last_used_at = usage.and_then(|usage| usage.last_used_at);

        Self {
            request_count,
            request_count_display: format_plain_number(nonnegative_i64_to_u64(request_count)),
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
    refreshing_account_ids: HashSet<String>,
}

struct AccountModelUsageRecord {
    account_id: String,
    model: String,
    request_count: i64,
    error_count: i64,
    input_tokens: i64,
    output_tokens: i64,
    cached_tokens: i64,
    total_cost_usd: f64,
    last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
struct AccountQuotaUsageWindow {
    account_id: String,
    window: AdminAccountQuotaUsageWindow,
}

impl AccountListStats {
    fn data_for(&self, account: AdminAccountMetadata) -> AdminAccountData {
        let account_id = account.id.clone();
        let token_refreshing = token_refresh_status_eligible(account.status)
            && self.refreshing_account_ids.contains(&account_id);
        AdminAccountData::from_parts(
            account,
            self.usage_by_account.get(&account_id),
            self.quota_by_account.get(&account_id).cloned(),
            self.models_by_account
                .get(&account_id)
                .cloned()
                .unwrap_or_default(),
            token_refreshing,
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
            code: ADMIN_OK_CODE,
            message: ADMIN_OK_MESSAGE.into(),
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
            code: ADMIN_OK_CODE,
            message: ADMIN_OK_MESSAGE.into(),
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
#[serde(untagged)]
enum AccountUpdateData {
    Account(Box<AdminAccountData>),
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountRefreshData {
    id: String,
    email: Option<String>,
    previous_status: String,
    result: &'static str,
    error: Option<String>,
    duration_ms: i64,
}

impl From<AdminAccountRefreshResult> for AccountRefreshData {
    fn from(result: AdminAccountRefreshResult) -> Self {
        Self {
            id: result.id,
            email: result.email,
            previous_status: result.previous_status.as_str().to_string(),
            result: account_refresh_outcome_str(result.outcome),
            error: result.error,
            duration_ms: result.duration_ms,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountQuotaData {
    quota: Value,
    raw: Value,
    quota_data: AdminAccountQuotaData,
    plan_type: Option<String>,
    account: AdminAccountData,
}

impl AccountQuotaData {
    fn from_account(
        quota: Value,
        raw: Value,
        quota_data: AdminAccountQuotaData,
        account: AdminAccountData,
    ) -> Self {
        let plan_type = account.plan_type.clone();
        Self {
            quota,
            raw,
            quota_data,
            plan_type,
            account,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub(crate) struct AccountHealthCheckRequest {
    ids: Option<Vec<String>>,
    stagger_ms: Option<u64>,
    concurrency: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountHealthCheckSummaryData {
    total: usize,
    alive: usize,
    dead: usize,
    skipped: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountHealthCheckData {
    summary: AccountHealthCheckSummaryData,
    results: Vec<AccountRefreshData>,
}

impl From<AdminAccountHealthCheck> for AccountHealthCheckData {
    fn from(result: AdminAccountHealthCheck) -> Self {
        let summary = AccountHealthCheckSummaryData {
            total: result.results.len(),
            alive: result.alive(),
            dead: result.dead(),
            skipped: result.skipped(),
        };
        Self {
            summary,
            results: result
                .results
                .into_iter()
                .map(AccountRefreshData::from)
                .collect(),
        }
    }
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
struct AccountOAuthAuthorizeData {
    session_id: String,
    auth_url: String,
    expires_at: String,
    expires_at_display: String,
}

/// `GET /api/admin/accounts`
pub(crate) async fn accounts(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Query(params): Query<AccountsQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let limit = clamp_limit(params.page_size.or(params.limit).unwrap_or(50));
    let use_numbered_page = params.page.is_some() || params.page_size.is_some();
    let quota_by_account = quota_snapshots_by_account(&state).await;
    let summary = account_summary_data(&state, &quota_by_account).await;

    if use_numbered_page {
        return match state
            .services
            .admin_accounts
            .list_page(clamp_page(params.page.unwrap_or(1)), limit, params.search)
            .await
        {
            Ok(page) => {
                let stats = account_list_stats(&state, &page.items, &quota_by_account).await?;
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
            Err(error) => Err(account_error(&error)),
        };
    }

    match state
        .services
        .admin_accounts
        .list(params.cursor, limit)
        .await
    {
        Ok(page) => {
            let stats = account_list_stats(&state, &page.items, &quota_by_account).await?;
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
        Err(error) => Err(account_error(&error)),
    }
}

/// `POST /api/admin/accounts`
pub(crate) async fn create_account(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Json(payload): Json<CreateAccountRequest>,
) -> Result<impl IntoResponse, AdminError> {
    match state
        .services
        .admin_accounts
        .create(payload.token, payload.refresh_token)
        .await
    {
        Ok(account) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(AdminAccountData::from(account)),
        )),
        Err(error) => Err(account_error(&error)),
    }
}

/// `GET /api/admin/accounts/export`
pub(crate) async fn export_accounts(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Query(query): Query<AccountExportQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let ids = account_export_ids(query.ids.as_deref());

    if query.confirm.as_deref() != Some(ACCOUNT_EXPORT_CONFIRMATION) {
        return Err(AdminError::bad_request(
            "account export requires confirm=export_sensitive_accounts",
        ));
    }

    match state.services.admin_accounts.export(ids).await {
        Ok(result) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(result),
        )),
        Err(error) => Err(account_error(&error)),
    }
}

/// `POST /api/admin/accounts/refresh`
pub(crate) async fn refresh_account(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Json(payload): Json<AccountActionRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let account_id = payload.id;
    match state
        .services
        .admin_accounts
        .refresh_account(&account_id)
        .await
    {
        Ok(result) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(AccountRefreshData::from(result)),
        )),
        Err(error) => Err(account_error(&error)),
    }
}

/// `POST /api/admin/accounts/health-check`
pub(crate) async fn health_check_accounts(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Json(payload): Json<AccountHealthCheckRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let stagger_ms = payload.stagger_ms.unwrap_or(3_000);
    if !(500..=30_000).contains(&stagger_ms) && stagger_ms != 0 {
        return Err(AdminError::bad_request(
            "stagger_ms must be between 500 and 30000",
        ));
    }
    if let Some(concurrency) = payload.concurrency {
        if !(1..=10).contains(&concurrency) {
            return Err(AdminError::bad_request(
                "concurrency must be between 1 and 10",
            ));
        }
    }
    match state
        .services
        .admin_accounts
        .health_check_accounts(payload.ids, stagger_ms, payload.concurrency.unwrap_or(2))
        .await
    {
        Ok(result) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(AccountHealthCheckData::from(result)),
        )),
        Err(error) => Err(account_error(&error)),
    }
}

/// `GET /api/admin/accounts/quota`
pub(crate) async fn account_quota(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Query(query): Query<AccountIdQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let account_id = query.id;
    match state
        .services
        .admin_accounts
        .account_quota(&account_id)
        .await
    {
        Ok(data) => {
            let quota = data.get("quota").cloned().unwrap_or(Value::Null);
            let raw = data.get("raw").cloned().unwrap_or(Value::Null);
            let quota_json = quota.to_string();
            let mut quota_data = quota_data(&quota_json, Some(Utc::now()));
            apply_account_quota_window_local_usage(&state, &account_id, &mut quota_data).await;
            let account =
                account_data_for_quota_refresh(&state, &account_id, quota_data.clone()).await?;
            Ok(AdminResponse::new(
                StatusCode::OK,
                AdminEnvelope::ok(AccountQuotaData::from_account(
                    quota, raw, quota_data, account,
                )),
            ))
        }
        Err(AdminAccountError::NotFound) => Err(account_not_found()),
        Err(AdminAccountError::Inactive(status)) => Err(AdminError::conflict(format!(
            "Account is {}, cannot query quota",
            status.as_str()
        ))),
        Err(AdminAccountError::FetchQuota(msg)) => Err(AdminError::bad_gateway(format!(
            "Failed to fetch quota from Codex API: {msg}"
        ))),
        Err(e) => Err(account_error(&e)),
    }
}

async fn account_data_for_quota_refresh(
    state: &AppState,
    account_id: &str,
    quota_data: AdminAccountQuotaData,
) -> Result<AdminAccountData, AdminError> {
    let Some(account) = state
        .services
        .admin_accounts
        .get(account_id)
        .await
        .map_err(|error| account_error(&error))?
    else {
        return Err(account_not_found());
    };
    let quota_by_account = HashMap::from([(account_id.to_string(), quota_data)]);
    let stats =
        account_list_stats(state, std::slice::from_ref(&account), &quota_by_account).await?;
    Ok(stats.data_for(account))
}

/// `POST /api/admin/accounts/import`
pub(crate) async fn import_accounts(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Json(payload): Json<Value>,
) -> Result<impl IntoResponse, AdminError> {
    match state.services.admin_accounts.import(payload).await {
        Ok(result) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(AccountImportData {
                imported: result.imported,
                skipped: result.skipped,
                source_format: result.source_format.to_string(),
            }),
        )),
        Err(error) => Err(account_error(&error)),
    }
}

/// `POST /api/admin/accounts/oauth/authorize`
pub(crate) async fn oauth_authorize_account(
    State(state): State<AppState>,
    _auth: AdminAuth,
) -> Result<impl IntoResponse, AdminError> {
    match state.services.admin_accounts.oauth_authorize().await {
        Ok(result) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(AccountOAuthAuthorizeData {
                session_id: result.session_id,
                auth_url: result.auth_url,
                expires_at: china_rfc3339(&result.expires_at),
                expires_at_display: china_datetime(&result.expires_at),
            }),
        )),
        Err(error) => Err(account_error(&error)),
    }
}

/// `POST /api/admin/accounts/oauth/exchange`
pub(crate) async fn oauth_exchange_account(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Json(payload): Json<AccountOAuthExchangeRequest>,
) -> Result<impl IntoResponse, AdminError> {
    match state
        .services
        .admin_accounts
        .oauth_exchange(OAuthExchangeInput {
            session_id: payload.session_id,
            callback_url: payload.callback_url,
            code: payload.code,
            state: payload.state,
        })
        .await
    {
        Ok(result) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(AccountImportData {
                imported: result.imported,
                skipped: result.skipped,
                source_format: result.source_format.to_string(),
            }),
        )),
        Err(error) => Err(account_error(&error)),
    }
}

/// `GET /api/admin/accounts/test?id=...&modelId=...`
pub(crate) async fn test_account_connection(
    State(state): State<AppState>,
    Query(query): Query<AccountTestQuery>,
    _auth: AdminAuth,
) -> Result<Response, AdminError> {
    let account_id = query.id;

    let model = query
        .model_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| AdminError::bad_request("Model is required"))?;
    let stream = state
        .services
        .admin_accounts
        .test_connection_stream(&account_id, model)
        .await
        .map_err(|error| account_error(&error))?;

    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/event-stream")
        .header(CACHE_CONTROL, "no-cache")
        .header(CONNECTION, "keep-alive")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(stream))
        .map_err(|_| AdminError::internal("Failed to build account test stream"))
}

/// `GET /api/admin/accounts/models?id=...`
pub(crate) async fn account_models(
    State(state): State<AppState>,
    Query(query): Query<AccountIdQuery>,
    _auth: AdminAuth,
) -> Result<impl IntoResponse, AdminError> {
    let account_id = query.id;
    match state
        .services
        .admin_accounts
        .account_models(&account_id)
        .await
    {
        Ok(models) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(AccountModelsData {
                models: models
                    .into_iter()
                    .map(|model| AccountModelData {
                        id: model.id,
                        label: model.label,
                    })
                    .collect(),
            }),
        )),
        Err(error) => Err(account_error(&error)),
    }
}

/// `POST /api/admin/accounts/delete`
pub(crate) async fn batch_delete_accounts(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Json(payload): Json<BatchDeleteAccountsRequest>,
) -> Result<impl IntoResponse, AdminError> {
    match state
        .services
        .admin_accounts
        .batch_delete(payload.ids)
        .await
    {
        Ok(result) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(BatchDeleteData {
                deleted: result.deleted,
                not_found: result.not_found,
            }),
        )),
        Err(error) => Err(account_error(&error)),
    }
}

/// `POST /api/admin/accounts/update`
pub(crate) async fn update_account(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Json(payload): Json<Value>,
) -> Result<impl IntoResponse, AdminError> {
    let ParsedAccountUpdate { id, update } = parse_account_update(&payload)?;

    match state
        .services
        .admin_accounts
        .update_account(&id, update)
        .await
    {
        Ok(Some(account)) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(AccountUpdateData::Account(Box::new(account.into()))),
        )),
        Ok(None) => Err(account_not_found()),
        Err(error) => Err(account_error(&error)),
    }
}

async fn account_list_stats(
    state: &AppState,
    accounts: &[AdminAccountMetadata],
    quota_by_account: &HashMap<String, AdminAccountQuotaData>,
) -> Result<AccountListStats, AdminError> {
    let account_ids = accounts
        .iter()
        .map(|account| account.id.clone())
        .collect::<Vec<_>>();
    let usage_records = state
        .services
        .usage
        .list_by_account_ids(&account_ids)
        .await
        .unwrap_or_default();
    let mut quota_by_account = quota_by_account.clone();
    let all_quota_windows = quota_usage_windows_by_account(accounts, &quota_by_account);
    let quota_window_usage_stats =
        quota_window_local_usage_by_account(state, &all_quota_windows).await;
    apply_quota_window_local_usage(accounts, &mut quota_by_account, &quota_window_usage_stats);
    let selected_quota_windows = selected_quota_windows_by_account(accounts, &quota_by_account);
    let usage_records = usage_records
        .into_iter()
        .map(|usage| {
            apply_selected_quota_window_usage(
                usage,
                &selected_quota_windows,
                &quota_window_usage_stats,
            )
        })
        .collect::<Vec<_>>();
    let models_by_account = list_current_window_model_usage(state, &usage_records).await;
    let refreshing_account_ids = state
        .services
        .token_refresh
        .refreshing_account_ids(&account_ids, Utc::now())
        .await
        .map_err(|error| AdminError::internal(error.to_string()))?;

    Ok(AccountListStats {
        usage_by_account: usage_records
            .into_iter()
            .map(|usage| (usage.account_id.clone(), usage))
            .collect(),
        quota_by_account,
        models_by_account,
        refreshing_account_ids,
    })
}

fn account_display_status(status: AccountStatus, token_refreshing: bool) -> &'static str {
    if token_refreshing {
        "refreshing"
    } else {
        status.as_str()
    }
}

fn selected_quota_windows_by_account(
    accounts: &[AdminAccountMetadata],
    quota_by_account: &HashMap<String, AdminAccountQuotaData>,
) -> HashMap<String, AdminAccountQuotaUsageWindow> {
    accounts
        .iter()
        .filter_map(|account| {
            quota_by_account
                .get(&account.id)
                .and_then(selected_quota_window)
                .map(|window| (account.id.clone(), window))
        })
        .collect()
}

fn selected_quota_window(quota: &AdminAccountQuotaData) -> Option<AdminAccountQuotaUsageWindow> {
    quota.usage_windows().into_iter().max_by(|left, right| {
        left.duration_seconds()
            .cmp(&right.duration_seconds())
            .then_with(|| left.end.cmp(&right.end))
            .then_with(|| left.key.cmp(&right.key))
    })
}

fn quota_usage_windows_by_account(
    accounts: &[AdminAccountMetadata],
    quota_by_account: &HashMap<String, AdminAccountQuotaData>,
) -> Vec<AccountQuotaUsageWindow> {
    accounts
        .iter()
        .filter_map(|account| {
            quota_by_account
                .get(&account.id)
                .map(|quota| (account.id.as_str(), quota))
        })
        .flat_map(|(account_id, quota)| {
            quota
                .usage_windows()
                .into_iter()
                .map(move |window| AccountQuotaUsageWindow {
                    account_id: account_id.to_string(),
                    window,
                })
        })
        .collect()
}

async fn apply_account_quota_window_local_usage(
    state: &AppState,
    account_id: &str,
    quota: &mut AdminAccountQuotaData,
) {
    let windows = quota
        .usage_windows()
        .into_iter()
        .map(|window| AccountQuotaUsageWindow {
            account_id: account_id.to_string(),
            window,
        })
        .collect::<Vec<_>>();
    let usage_by_account = quota_window_local_usage_by_account(state, &windows).await;
    let empty_usage = HashMap::new();
    quota.apply_local_usage(usage_by_account.get(account_id).unwrap_or(&empty_usage));
}

fn apply_quota_window_local_usage(
    accounts: &[AdminAccountMetadata],
    quota_by_account: &mut HashMap<String, AdminAccountQuotaData>,
    usage_by_account: &HashMap<String, HashMap<String, AdminAccountQuotaWindowLocalUsage>>,
) {
    for account in accounts {
        if let Some(quota) = quota_by_account.get_mut(&account.id) {
            let empty_usage = HashMap::new();
            quota.apply_local_usage(usage_by_account.get(&account.id).unwrap_or(&empty_usage));
        }
    }
}

async fn quota_window_local_usage_by_account(
    state: &AppState,
    windows: &[AccountQuotaUsageWindow],
) -> HashMap<String, HashMap<String, AdminAccountQuotaWindowLocalUsage>> {
    let Some(min_start) = windows.iter().map(|window| window.window.start).min() else {
        return HashMap::new();
    };
    let Some(max_end) = windows.iter().map(|window| window.window.end).max() else {
        return HashMap::new();
    };

    let account_ids = windows
        .iter()
        .map(|window| window.account_id.as_str())
        .collect::<HashSet<_>>();
    let mut windows_by_account = HashMap::<&str, Vec<&AdminAccountQuotaUsageWindow>>::new();
    for window in windows {
        windows_by_account
            .entry(window.account_id.as_str())
            .or_default()
            .push(&window.window);
    }
    let mut builder = QueryBuilder::<Sqlite>::new(
        "select
          account_id,
          bucket_start,
          coalesce(sum(request_count), 0) as request_count,
          coalesce(sum(input_tokens), 0) as input_tokens,
          coalesce(sum(output_tokens), 0) as output_tokens,
          coalesce(sum(cached_tokens), 0) as cached_tokens
        from usage_time_buckets
        where account_id in (",
    );
    let mut separated = builder.separated(", ");
    for account_id in &account_ids {
        separated.push_bind(*account_id);
    }
    separated.push_unseparated(")");
    builder.push(" and bucket_start >= ");
    builder.push_bind(china_quarter_hour_start(min_start).to_rfc3339());
    builder.push(" and bucket_start <= ");
    builder.push_bind(max_end.to_rfc3339());
    builder.push(" group by account_id, bucket_start");

    let rows = builder
        .build()
        .fetch_all(state.services.background_tasks.accounts.pool())
        .await
        .unwrap_or_default();
    let mut usage_by_account =
        HashMap::<String, HashMap<String, AdminAccountQuotaWindowLocalUsage>>::new();

    for row in rows {
        let account_id: String = row.get("account_id");
        let Ok(bucket_start) = parse_rfc3339_utc(row.get::<&str, _>("bucket_start")) else {
            continue;
        };
        let bucket_usage = AdminAccountQuotaWindowLocalUsage {
            request_count: row.get("request_count"),
            input_tokens: row.get("input_tokens"),
            output_tokens: row.get("output_tokens"),
            cached_tokens: row.get("cached_tokens"),
        };
        let Some(account_windows) = windows_by_account.get(account_id.as_str()) else {
            continue;
        };

        for window in account_windows.iter().filter(|window| {
            bucket_start >= china_quarter_hour_start(window.start) && bucket_start <= window.end
        }) {
            usage_by_account
                .entry(account_id.clone())
                .or_default()
                .entry(window.key.clone())
                .or_default()
                .add(bucket_usage);
        }
    }

    usage_by_account
}

fn apply_selected_quota_window_usage(
    mut usage: AdminUsageRecord,
    selected_quota_windows: &HashMap<String, AdminAccountQuotaUsageWindow>,
    quota_window_usage_stats: &HashMap<String, HashMap<String, AdminAccountQuotaWindowLocalUsage>>,
) -> AdminUsageRecord {
    let Some(window) = selected_quota_windows.get(&usage.account_id) else {
        return usage;
    };
    let stats = quota_window_usage_stats
        .get(&usage.account_id)
        .and_then(|usage_by_window| usage_by_window.get(&window.key))
        .copied()
        .unwrap_or_default();
    usage.window_request_count = stats.request_count;
    usage.window_input_tokens = stats.input_tokens;
    usage.window_output_tokens = stats.output_tokens;
    usage.window_cached_tokens = stats.cached_tokens;
    usage.window_started_at = Some(window.start);
    usage.window_reset_at = Some(window.end);
    usage.limit_window_seconds = Some(window.window_seconds);
    usage
}

async fn account_summary_data(
    state: &AppState,
    quota_by_account: &HashMap<String, AdminAccountQuotaData>,
) -> AdminAccountSummaryData {
    let accounts = list_all_account_metadata(state).await;
    let total = accounts.len() as u64;
    let active = accounts
        .iter()
        .filter(|account| account.status == AccountStatus::Active)
        .count() as u64;
    let high_usage = accounts
        .iter()
        .filter(|account| {
            quota_by_account
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

async fn quota_snapshots_by_account(state: &AppState) -> HashMap<String, AdminAccountQuotaData> {
    state
        .services
        .background_tasks
        .accounts
        .list_quota_snapshots()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|snapshot| {
            (
                snapshot.account_id,
                quota_data(&snapshot.quota_json, snapshot.quota_fetched_at),
            )
        })
        .collect()
}

async fn list_all_account_metadata(state: &AppState) -> Vec<AdminAccountMetadata> {
    let mut page = 1;
    let mut accounts = Vec::new();
    loop {
        let Ok(result) = state
            .services
            .admin_accounts
            .list_page(page, ACCOUNT_STATS_PAGE_LIMIT, None)
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
    quota.has_high_usage()
}

fn account_summary_needs_attention(status: AccountStatus) -> bool {
    matches!(
        status,
        AccountStatus::Expired | AccountStatus::Disabled | AccountStatus::Banned
    )
}

async fn list_current_window_model_usage(
    state: &AppState,
    usage_records: &[AdminUsageRecord],
) -> HashMap<String, Vec<AdminAccountModelUsageData>> {
    let now = Utc::now();
    let windows = usage_records
        .iter()
        .filter_map(|usage| {
            current_usage_window(usage, now)
                .map(|(start, end)| (usage.account_id.clone(), start, end))
        })
        .collect::<Vec<_>>();
    if windows.is_empty() {
        return HashMap::new();
    }

    let mut builder = QueryBuilder::<Sqlite>::new(
        "select
          account_id,
          model,
          service_tier,
          coalesce(sum(request_count), 0) as request_count,
          coalesce(sum(error_count), 0) as error_count,
          coalesce(sum(input_tokens), 0) as input_tokens,
          coalesce(sum(output_tokens), 0) as output_tokens,
          coalesce(sum(cached_tokens), 0) as cached_tokens,
          max(bucket_start) as last_used_at
        from usage_time_buckets
        where model != '' and (",
    );
    for (index, (account_id, start, end)) in windows.iter().enumerate() {
        if index > 0 {
            builder.push(" or ");
        }
        let bucket_start = china_quarter_hour_start(*start);
        builder.push("(account_id = ");
        builder.push_bind(account_id);
        builder.push(" and bucket_start >= ");
        builder.push_bind(bucket_start.to_rfc3339());
        builder.push(" and bucket_start <= ");
        builder.push_bind(end.to_rfc3339());
        builder.push(")");
    }
    builder.push(") group by account_id, model, service_tier");

    let rows = builder
        .build()
        .fetch_all(state.services.background_tasks.accounts.pool())
        .await
        .unwrap_or_default();
    let mut records_by_model = HashMap::<(String, String), AccountModelUsageRecord>::new();
    for row in rows {
        let account_id: String = row.get("account_id");
        let model: String = row.get("model");
        let request_count: i64 = row.get("request_count");
        let error_count: i64 = row.get("error_count");
        let input_tokens: i64 = row.get("input_tokens");
        let output_tokens: i64 = row.get("output_tokens");
        let cached_tokens: i64 = row.get("cached_tokens");
        let total_cost_usd = billing::calculate_cost(
            nonnegative_i64_to_u64(input_tokens),
            nonnegative_i64_to_u64(output_tokens),
            nonnegative_i64_to_u64(cached_tokens),
            &model,
            row.get::<Option<String>, _>("service_tier").as_deref(),
        );
        let last_used_at = row
            .get::<Option<String>, _>("last_used_at")
            .and_then(|value| DateTime::parse_from_rfc3339(&value).ok())
            .map(|value| value.with_timezone(&Utc));

        let record = records_by_model
            .entry((account_id.clone(), model.clone()))
            .or_insert_with(|| AccountModelUsageRecord {
                account_id,
                model,
                request_count: 0,
                error_count: 0,
                input_tokens: 0,
                output_tokens: 0,
                cached_tokens: 0,
                total_cost_usd: 0.0,
                last_used_at: None,
            });
        record.request_count += request_count;
        record.error_count += error_count;
        record.input_tokens += input_tokens;
        record.output_tokens += output_tokens;
        record.cached_tokens += cached_tokens;
        record.total_cost_usd += total_cost_usd;
        record.last_used_at = record.last_used_at.max(last_used_at);
    }
    let records = records_by_model.into_values().collect::<Vec<_>>();

    models_by_account(records)
}

fn current_usage_window(
    usage: &AdminUsageRecord,
    now: DateTime<Utc>,
) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let start = usage.window_started_at?;
    let end = usage.window_reset_at.unwrap_or(now);
    (start <= end).then_some((start, end))
}

fn models_by_account(
    records: Vec<AccountModelUsageRecord>,
) -> HashMap<String, Vec<AdminAccountModelUsageData>> {
    let mut by_account = HashMap::<String, Vec<AccountModelUsageRecord>>::new();
    for record in records {
        by_account
            .entry(record.account_id.clone())
            .or_default()
            .push(record);
    }

    by_account
        .into_iter()
        .map(|(account_id, mut records)| {
            records.sort_by(|a, b| {
                b.request_count
                    .cmp(&a.request_count)
                    .then_with(|| b.last_used_at.cmp(&a.last_used_at))
                    .then_with(|| a.model.cmp(&b.model))
            });
            (
                account_id,
                records.into_iter().map(model_usage_data).collect(),
            )
        })
        .collect()
}

fn model_usage_data(usage: AccountModelUsageRecord) -> AdminAccountModelUsageData {
    let request_count = nonnegative_i64_to_u64(usage.request_count);
    let error_count = nonnegative_i64_to_u64(usage.error_count);
    let input_tokens = nonnegative_i64_to_u64(usage.input_tokens);
    let output_tokens = nonnegative_i64_to_u64(usage.output_tokens);
    let cached_tokens = nonnegative_i64_to_u64(usage.cached_tokens);
    let total_tokens = input_tokens + output_tokens;
    let success_rate = if request_count > 0 {
        ((request_count.saturating_sub(error_count)) as f64 / request_count as f64 * 1000.0).round()
            / 10.0
    } else {
        0.0
    };
    let total_cost_usd = usage.total_cost_usd;

    AdminAccountModelUsageData {
        model: usage.model,
        request_count,
        request_count_display: format_plain_number(request_count),
        success_rate,
        success_rate_display: format_percent(success_rate),
        input_tokens,
        input_tokens_display: format_tokens(input_tokens),
        output_tokens,
        output_tokens_display: format_tokens(output_tokens),
        cached_tokens,
        cached_tokens_display: format_tokens(cached_tokens),
        total_tokens,
        total_tokens_display: format_tokens(total_tokens),
        total_cost_usd,
        total_cost_usd_display: format_cost(total_cost_usd),
        last_used_at: usage.last_used_at.map(|value| china_rfc3339(&value)),
        last_used_at_display: china_relative_time(usage.last_used_at, Utc::now()),
    }
}

struct ParsedAccountUpdate {
    id: String,
    update: AdminAccountUpdate,
}

fn parse_account_update(payload: &Value) -> Result<ParsedAccountUpdate, AdminError> {
    let payload = parse_editable_update(
        payload,
        EditableUpdateMessages {
            object_required: "Account update request must be an object",
            invalid: "Invalid account update request",
            empty_update: "Account update request must include editable fields",
            unknown_field_editable: true,
        },
    )?;
    let update = AdminAccountUpdate {
        label: payload.label.map(|label| {
            label.and_then(|value| {
                let value = value.trim();
                (!value.is_empty()).then(|| value.to_string())
            })
        }),
        status: payload.status,
    };
    if !update.any() {
        return Err(AdminError::bad_request(
            "Account update request must include editable fields",
        ));
    }
    Ok(ParsedAccountUpdate {
        id: payload.id,
        update,
    })
}

fn account_error(error: &AdminAccountError) -> AdminError {
    match error {
        AdminAccountError::InvalidStatus(_)
        | AdminAccountError::LabelTooLong
        | AdminAccountError::EmptyIds
        | AdminAccountError::NoImportableAccounts
        | AdminAccountError::NoModels
        | AdminAccountError::InvalidAccessTokenExpiresAt
        | AdminAccountError::TokenRequired
        | AdminAccountError::InvalidToken(_)
        | AdminAccountError::RefreshTokenExchange(_)
        | AdminAccountError::OAuthSessionInvalid
        | AdminAccountError::OAuthCallbackInvalid
        | AdminAccountError::OAuthStateMismatch
        | AdminAccountError::NoValidCookies => AdminError::bad_request(error.to_string()),
        AdminAccountError::OAuthCodeExchange(_) => AdminError::bad_gateway(error.to_string()),
        AdminAccountError::NotFound => account_not_found(),
        AdminAccountError::Inactive(_) => AdminError::conflict(error.to_string()),
        _ => AdminError::internal(error.to_string()),
    }
}

fn account_refresh_outcome_str(outcome: AdminAccountRefreshOutcome) -> &'static str {
    match outcome {
        AdminAccountRefreshOutcome::Alive => "alive",
        AdminAccountRefreshOutcome::Dead => "dead",
        AdminAccountRefreshOutcome::Skipped => "skipped",
    }
}

fn account_not_found() -> AdminError {
    AdminError::not_found("Account not found")
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
