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
    admin::response::{AdminEnvelope, AdminError, AdminPageEnvelope, AdminResponse},
    admin::{
        accounts::service::{AdminAccountError, AdminAccountMetadata},
        monitoring::{
            billing,
            event_store::AdminLogFilter,
            events::{EventLevel, EventLog},
            service::AdminUsageRecord,
        },
    },
    http::middleware::request_id::RequestId,
    infra::{
        json::{clamp_limit, clamp_page, Page},
        time::{china_datetime, china_relative_time, china_rfc3339, china_rfc3339_str},
    },
    runtime::state::AppState,
    upstream::accounts::store::StoredAccount,
};

const ACCOUNT_STATS_PAGE_LIMIT: u32 = 200;

// ============================================================================
// Query / Request types
// ============================================================================

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountsQuery {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
    pub page: Option<u32>,
    pub page_size: Option<u32>,
    pub search: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateAccountRequest {
    pub token: Option<String>,
    pub refresh_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeleteAccountsRequest {
    pub ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetAccountCookiesRequest {
    pub id: String,
    pub cookies: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountActionRequest {
    pub id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountIdQuery {
    pub id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountExportQuery {
    pub ids: Option<String>,
    pub format: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HealthCheckRequest {
    pub ids: Option<Vec<String>>,
    pub stagger_ms: Option<u64>,
    pub concurrency: Option<u8>,
}

// ============================================================================
// Response types
// ============================================================================

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAccountData {
    pub id: String,
    pub email: Option<String>,
    pub account_id: Option<String>,
    pub user_id: Option<String>,
    pub label: Option<String>,
    pub plan_type: Option<String>,
    pub status: String,
    pub access_token_expires_at: Option<String>,
    pub access_token_expires_at_display: Option<String>,
    pub added_at: String,
    pub added_at_display: String,
    pub updated_at: String,
    pub updated_at_display: String,
    pub quota: AdminAccountQuotaData,
    pub usage: AdminAccountUsageData,
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
pub struct AdminAccountQuotaData {
    pub used_percent: Option<f64>,
    pub used_percent_display: String,
    pub reset_at_display: String,
    pub refreshed_at_display: String,
    pub window_used_display: String,
}

impl Default for AdminAccountQuotaData {
    fn default() -> Self {
        Self {
            used_percent: None,
            used_percent_display: "-".to_string(),
            reset_at_display: "-".to_string(),
            refreshed_at_display: "-".to_string(),
            window_used_display: "-".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAccountUsageData {
    pub request_count: i64,
    pub request_count_display: String,
    pub empty_response_count: i64,
    pub input_tokens: i64,
    pub input_tokens_display: String,
    pub output_tokens: i64,
    pub output_tokens_display: String,
    pub cached_tokens: i64,
    pub cached_tokens_display: String,
    pub reasoning_tokens: i64,
    pub total_tokens: i64,
    pub total_tokens_display: String,
    pub image_input_tokens: i64,
    pub image_output_tokens: i64,
    pub image_tokens_display: String,
    pub image_request_count: i64,
    pub image_request_failed_count: i64,
    pub created_tokens: i64,
    pub created_tokens_display: String,
    pub read_tokens: i64,
    pub read_tokens_display: String,
    pub last_used_at: Option<String>,
    pub last_used_at_display: String,
    pub models: Vec<AdminAccountModelUsageData>,
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
pub struct AdminAccountModelUsageData {
    pub model: String,
    pub request_count: u64,
    pub request_count_display: String,
    pub success_rate: f64,
    pub success_rate_display: String,
    pub input_tokens: u64,
    pub input_tokens_display: String,
    pub output_tokens: u64,
    pub output_tokens_display: String,
    pub cached_tokens: u64,
    pub cached_tokens_display: String,
    pub total_tokens: u64,
    pub total_tokens_display: String,
    pub total_cost_usd: f64,
    pub total_cost_usd_display: String,
    pub last_used_at: Option<String>,
    pub last_used_at_display: String,
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
pub struct BatchDeleteAccountsData {
    pub deleted: u32,
    pub not_found: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchUpdateAccountStatusData {
    pub updated: u32,
    pub not_found: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum AccountUpdateData {
    Account(Box<AdminAccountData>),
    BatchStatus(BatchUpdateAccountStatusData),
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetAccountUsageData {
    pub id: String,
    pub reset: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountCookiesData {
    pub cookies: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAccountExportData {
    pub id: String,
    pub email: Option<String>,
    pub account_id: Option<String>,
    pub user_id: Option<String>,
    pub label: Option<String>,
    pub plan_type: Option<String>,
    pub status: String,
    pub access_token_expires_at: Option<String>,
    pub added_at: String,
    pub updated_at: String,
    pub token: String,
    pub refresh_token: Option<String>,
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
pub struct AccountExportData {
    source_format: &'static str,
    accounts: Vec<AdminAccountExportData>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountImportData {
    imported: u32,
    skipped: u32,
    source_format: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthCheckData {
    pub summary: HealthCheckSummary,
    pub results: Vec<Value>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthCheckSummary {
    pub total: usize,
    pub alive: usize,
    pub dead: usize,
    pub skipped: usize,
}

// ============================================================================
// Handlers
// ============================================================================

/// `GET /api/admin/accounts`
pub async fn accounts(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(params): Query<AccountsQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    let limit = clamp_limit(params.page_size.or(params.limit).unwrap_or(50));
    let use_numbered_page = params.page.is_some() || params.page_size.is_some();
    let stats = account_list_stats(&state).await;

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
                    AdminPageEnvelope::numbered(page, request_id),
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
                AdminPageEnvelope::ok(page, limit, request_id),
            ))
        }
        Err(error) => Err(account_error(error, request_id)),
    }
}

/// `POST /api/admin/accounts`
pub async fn create_account(
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
pub async fn refresh_account(
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
pub async fn reset_account_usage(
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
pub async fn get_account_cookies(
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
pub async fn set_account_cookies(
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
pub async fn account_quota(
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
pub async fn quota_warnings(
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
pub async fn health_check_accounts(
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
pub async fn export_accounts(
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
pub async fn import_accounts(
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
pub async fn batch_delete_accounts(
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
pub async fn update_account(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    match parse_account_update(payload, &request_id)? {
        ParsedAccountUpdate::Single { id, label, status } => {
            if let Some(label) = label {
                match state.services.admin_accounts.update_label(&id, label).await {
                    Ok(true) => {}
                    Ok(false) => return Err(account_not_found(request_id)),
                    Err(error) => return Err(account_error(error, request_id)),
                }
            }
            if let Some(status) = status {
                match state
                    .services
                    .admin_accounts
                    .update_status(&id, &status)
                    .await
                {
                    Ok(Some(_)) => {}
                    Ok(None) => return Err(account_not_found(request_id)),
                    Err(error) => return Err(account_error(error, request_id)),
                }
            }

            match state.services.admin_accounts.get(&id).await {
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
    let used_percent = quota
        .pointer("/rate_limit/used_percent")
        .and_then(number_value)
        .map(|value| value.clamp(0.0, 100.0));
    let reset_at = quota
        .pointer("/rate_limit/reset_at")
        .and_then(Value::as_i64)
        .and_then(|value| DateTime::<Utc>::from_timestamp(value, 0));
    let window_seconds = quota
        .pointer("/rate_limit/limit_window_seconds")
        .and_then(Value::as_u64);

    AdminAccountQuotaData {
        used_percent,
        used_percent_display: used_percent
            .map(format_percent)
            .unwrap_or_else(|| "-".to_string()),
        reset_at_display: reset_at
            .as_ref()
            .map(china_datetime)
            .unwrap_or_else(|| "-".to_string()),
        refreshed_at_display: china_relative_time(fetched_at, Utc::now()),
        window_used_display: quota_window_used_display(reset_at, window_seconds),
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
        label: Option<Option<String>>,
        status: Option<String>,
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
        if object.contains_key("label") {
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
    let label = object
        .get("label")
        .map(|value| optional_string_field(value, "label", request_id))
        .transpose()?;
    let status = object
        .get("status")
        .map(|value| required_string_value(value, "status", request_id))
        .transpose()?;
    if label.is_none() && status.is_none() {
        return Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "Account update request must include label or status",
            request_id,
        ));
    }
    Ok(ParsedAccountUpdate::Single { id, label, status })
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
    value
        .as_str()
        .map(ToString::to_string)
        .map(Some)
        .ok_or_else(|| {
            AdminError::new(
                StatusCode::BAD_REQUEST,
                40001,
                format!("{field} must be a string or null"),
                request_id,
            )
        })
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
