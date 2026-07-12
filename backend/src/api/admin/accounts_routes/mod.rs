//! 账号管理 HTTP 处理器。

use axum::{
    Json,
    body::Body,
    extract::{Query, State},
    http::{
        StatusCode,
        header::{CACHE_CONTROL, CONNECTION, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
};
use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    api::AppState,
    api::admin::{
        response::{
            ADMIN_OK_CODE, ADMIN_OK_MESSAGE, AdminEnvelope, AdminError, AdminResponse,
            BatchDeleteData, EditableUpdateMessages, PageMeta, parse_editable_update,
        },
        session::AdminAuth,
    },
    fleet::{
        account::AccountStatus,
        manage::{
            AccountHealthCheck, AccountManageError, AccountRefreshOutcome, AccountRefreshResult,
            AccountUpdate, ManagedAccount, OAuthExchangeInput,
            quota_view::{
                AccountQuotaData, AccountQuotaUsageWindow, AccountQuotaWindowLocalUsage, quota_data,
            },
        },
        refresh::token_refresh_status_eligible,
    },
    infra::{
        format::{
            format_cost, format_percent, format_plain_number, format_tokens, nonnegative_i64_to_u64,
        },
        json::{clamp_limit, clamp_page, total_pages},
        time::{china_datetime, china_relative_time, china_rfc3339},
    },
    telemetry::{
        account_usage::query::AccountUsageRecord,
        billing,
        buckets::query::{ModelUsageWindow, UsageBucketWindow},
    },
};

const ACCOUNT_STATS_PAGE_LIMIT: u32 = 200;
const ACCOUNT_EXPORT_CONFIRMATION: &str = "export_sensitive_accounts";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountsQuery {
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
    quota: AccountQuotaData,
    usage: AdminAccountUsageData,
}

impl From<ManagedAccount> for AdminAccountData {
    fn from(a: ManagedAccount) -> Self {
        Self::from_parts(a, None, None, Vec::new(), false)
    }
}

impl AdminAccountData {
    fn from_parts(
        a: ManagedAccount,
        usage: Option<&AccountUsageRecord>,
        quota: Option<AccountQuotaData>,
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
        usage: Option<&AccountUsageRecord>,
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
    usage_by_account: HashMap<String, AccountUsageRecord>,
    quota_by_account: HashMap<String, AccountQuotaData>,
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
struct AccountQuotaWindowSelection {
    account_id: String,
    window: AccountQuotaUsageWindow,
}

impl AccountListStats {
    fn data_for(&self, account: ManagedAccount) -> AdminAccountData {
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
    fn ok(
        page: crate::infra::json::NumberedPage<AdminAccountData>,
        summary: AdminAccountSummaryData,
    ) -> Self {
        Self {
            code: ADMIN_OK_CODE,
            message: ADMIN_OK_MESSAGE.into(),
            data: AdminAccountPageData {
                page: PageMeta {
                    page: page.page,
                    page_size: page.page_size,
                    total: page.total,
                    total_pages: total_pages(page.total, page.page_size),
                },
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

impl From<AccountRefreshResult> for AccountRefreshData {
    fn from(result: AccountRefreshResult) -> Self {
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
struct AdminAccountQuotaResponseData {
    quota: Value,
    raw: Value,
    quota_data: AccountQuotaData,
    plan_type: Option<String>,
    account: AdminAccountData,
}

impl AdminAccountQuotaResponseData {
    fn from_account(
        quota: Value,
        raw: Value,
        quota_data: AccountQuotaData,
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

impl From<AccountHealthCheck> for AccountHealthCheckData {
    fn from(result: AccountHealthCheck) -> Self {
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
    let page = clamp_page(params.page.unwrap_or(1));
    let page_size = clamp_limit(params.page_size.unwrap_or(50));
    let quota_by_account = quota_snapshots_by_account(&state).await;
    let summary = account_summary_data(&state, &quota_by_account).await;

    match state
        .services
        .admin_accounts
        .list_page(page, page_size, params.search)
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
                AdminAccountPageEnvelope::ok(page, summary),
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

mod export_routes;
mod import_routes;
mod lifecycle_routes;
mod oauth_routes;
mod probe_routes;
mod query;
mod quota_routes;

pub(crate) use export_routes::*;
pub(crate) use import_routes::*;
pub(crate) use lifecycle_routes::*;
pub(crate) use oauth_routes::*;
pub(crate) use probe_routes::*;
use query::*;
pub(crate) use quota_routes::*;
