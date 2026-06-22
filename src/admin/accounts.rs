//! 账号管理 HTTP 处理器。

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension, Json,
};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    accounts::admin_service::{AdminAccountError, AdminAccountMetadata},
    accounts::store::StoredAccount,
    admin::response::{AdminEnvelope, AdminError, AdminPageEnvelope, AdminResponse},
    admin::session::require_admin_session,
    http::middleware::request_id::RequestId,
    infra::json::{clamp_limit, Page},
    runtime::state::AppState,
};

// ============================================================================
// Query / Request types
// ============================================================================

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountsQuery {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateAccountRequest {
    pub token: Option<String>,
    pub refresh_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAccountLabelRequest {
    pub label: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAccountStatusRequest {
    pub status: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeleteAccountsRequest {
    pub ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchUpdateAccountStatusRequest {
    pub ids: Vec<String>,
    pub status: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetAccountCookiesRequest {
    pub cookies: Value,
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportCliRequest {
    codex_home: String,
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
    pub added_at: String,
    pub updated_at: String,
}

impl From<AdminAccountMetadata> for AdminAccountData {
    fn from(a: AdminAccountMetadata) -> Self {
        Self {
            id: a.id,
            email: a.email,
            account_id: a.account_id,
            user_id: a.user_id,
            label: a.label,
            plan_type: a.plan_type,
            status: account_status_str(a.status).to_string(),
            access_token_expires_at: a.access_token_expires_at.map(|dt| dt.to_rfc3339()),
            added_at: a.added_at.to_rfc3339(),
            updated_at: a.updated_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAccountLabelData {
    pub id: String,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAccountStatusData {
    pub id: String,
    pub status: String,
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
#[serde(rename_all = "camelCase")]
pub struct DeleteAccountData {
    pub deleted: bool,
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
pub struct DeleteAccountCookiesData {
    pub deleted: bool,
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
            access_token_expires_at: a.access_token_expires_at.map(|dt| dt.to_rfc3339()),
            added_at: a.added_at,
            updated_at: a.updated_at,
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
pub struct AccountImportCliData {
    imported: u32,
    skipped: u32,
    source_format: &'static str,
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
    let limit = clamp_limit(params.limit.unwrap_or(50));
    match state
        .services
        .admin_accounts
        .list(params.cursor, limit)
        .await
    {
        Ok(page) => {
            let page = Page {
                items: page.items.into_iter().map(AdminAccountData::from).collect(),
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

/// `DELETE /api/admin/accounts/{account_id}`
pub async fn delete_account(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    match state.services.admin_accounts.delete(&account_id).await {
        Ok(true) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(DeleteAccountData { deleted: true }, request_id),
        )),
        Ok(false) => Err(account_not_found(request_id)),
        Err(error) => Err(account_error(error, request_id)),
    }
}

/// `POST /api/admin/accounts/{account_id}/refresh`
pub async fn refresh_account(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
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

/// `POST /api/admin/accounts/{account_id}/reset-usage`
pub async fn reset_account_usage(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
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

/// `GET /api/admin/accounts/{account_id}/cookies`
pub async fn get_account_cookies(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    match state.services.admin_accounts.cookies(&account_id).await {
        Ok(cookies) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(AccountCookiesData { cookies }, request_id),
        )),
        Err(error) => Err(account_error(error, request_id)),
    }
}

/// `POST /api/admin/accounts/{account_id}/cookies`
pub async fn set_account_cookies(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
    Json(payload): Json<SetAccountCookiesRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
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

/// `DELETE /api/admin/accounts/{account_id}/cookies`
pub async fn delete_account_cookies(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    match state
        .services
        .admin_accounts
        .delete_cookies(&account_id)
        .await
    {
        Ok(()) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(DeleteAccountCookiesData { deleted: true }, request_id),
        )),
        Err(error) => Err(account_error(error, request_id)),
    }
}

/// `GET /api/admin/accounts/{account_id}/quota`
pub async fn account_quota(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
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

/// `POST /api/admin/accounts/import-cli`
pub async fn import_cli_account(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<ImportCliRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let auth_path = std::path::PathBuf::from(payload.codex_home).join("auth.json");
    let auth_payload =
        read_auth_json(&auth_path).map_err(|e| account_error(e, request_id.clone()))?;

    match state
        .services
        .admin_accounts
        .import_codex_cli_auth(auth_payload)
        .await
    {
        Ok(_) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(
                AccountImportCliData {
                    imported: 1,
                    skipped: 0,
                    source_format: "codex_cli",
                },
                request_id,
            ),
        )),
        Err(error) => Err(account_error(error, request_id)),
    }
}

/// `POST /api/admin/accounts/batch-delete`
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

/// `POST /api/admin/accounts/batch-status`
pub async fn batch_update_account_status(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<BatchUpdateAccountStatusRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    match state
        .services
        .admin_accounts
        .batch_update_status(payload.ids, &payload.status)
        .await
    {
        Ok(result) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(
                BatchUpdateAccountStatusData {
                    updated: result.updated,
                    not_found: result.not_found,
                },
                request_id,
            ),
        )),
        Err(error) => Err(account_error(error, request_id)),
    }
}

/// `PATCH /api/admin/accounts/{account_id}/label`
pub async fn update_account_label(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
    Json(payload): Json<UpdateAccountLabelRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    let label = payload.label;
    require_admin_session(&state, &headers, &request_id).await?;
    match state
        .services
        .admin_accounts
        .update_label(&account_id, label.clone())
        .await
    {
        Ok(true) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(
                UpdateAccountLabelData {
                    id: account_id,
                    label,
                },
                request_id,
            ),
        )),
        Ok(false) => Err(account_not_found(request_id)),
        Err(error) => Err(account_error(error, request_id)),
    }
}

/// `PATCH /api/admin/accounts/{account_id}/status`
pub async fn update_account_status(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
    Json(payload): Json<UpdateAccountStatusRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    match state
        .services
        .admin_accounts
        .update_status(&account_id, &payload.status)
        .await
    {
        Ok(Some(updated)) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(
                UpdateAccountStatusData {
                    id: updated.id,
                    status: account_status_str(updated.status).to_string(),
                },
                request_id,
            ),
        )),
        Ok(None) => Err(account_not_found(request_id)),
        Err(error) => Err(account_error(error, request_id)),
    }
}

// ============================================================================
// Error handling
// ============================================================================

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

fn account_status_str(status: crate::accounts::model::AccountStatus) -> &'static str {
    match status {
        crate::accounts::model::AccountStatus::Active => "active",
        crate::accounts::model::AccountStatus::Expired => "expired",
        crate::accounts::model::AccountStatus::QuotaExhausted => "quota_exhausted",
        crate::accounts::model::AccountStatus::Refreshing => "refreshing",
        crate::accounts::model::AccountStatus::Disabled => "disabled",
        crate::accounts::model::AccountStatus::Banned => "banned",
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

fn read_auth_json(path: &std::path::Path) -> Result<Value, AdminAccountError> {
    let content =
        std::fs::read_to_string(path).map_err(|_| AdminAccountError::NoImportableAccounts)?;
    serde_json::from_str(&content).map_err(|_| AdminAccountError::NoImportableAccounts)
}
