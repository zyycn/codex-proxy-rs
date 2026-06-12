use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension, Json,
};
use chrono::{SecondsFormat, Utc};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::{
    app::state::AppState,
    codex::accounts::service::{
        AccountImportEntry, AccountProbeResult, AccountQuotaError,
        AccountQuotaWarning as ServiceAccountQuotaWarning,
        AccountQuotaWarnings as ServiceAccountQuotaWarnings, AccountServiceError, HealthCheckError,
        RefreshAccountError, StoreImportAccountError, ValidatedAccountImportError,
    },
    codex::accounts::{
        model::AccountStatus,
        repository::{StoredAccount, StoredAccountMetadata},
    },
    codex::oauth::{default_codex_home, read_cli_auth_from_home},
    http::middleware::RequestId,
    utils::{
        json::{first_string, string_at},
        pagination::{clamp_limit, Page},
    },
};

use super::{require_admin_session, AdminEnvelope, AdminError, AdminPageEnvelope, AdminResponse};

fn account_service_error(error: AccountServiceError, request_id: &str) -> AdminError {
    match error {
        AccountServiceError::RepositoryUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Account repository is not initialized",
            request_id,
        ),
        AccountServiceError::UsageRepositoryUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Account usage repository is not initialized",
            request_id,
        ),
        AccountServiceError::CookieRepositoryUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Cookie repository is not initialized",
            request_id,
        ),
        AccountServiceError::AccountNotFound => AdminError::new(
            StatusCode::NOT_FOUND,
            40401,
            "Account not found",
            request_id,
        ),
        AccountServiceError::List => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to list accounts",
            request_id,
        ),
        AccountServiceError::Export => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to export accounts",
            request_id,
        ),
        AccountServiceError::Inspect => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to inspect account",
            request_id,
        ),
        AccountServiceError::ResetUsage => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to reset account usage",
            request_id,
        ),
        AccountServiceError::UpdateLabel => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to update account label",
            request_id,
        ),
        AccountServiceError::UpdateStatus => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to update account status",
            request_id,
        ),
        AccountServiceError::SyncStatus => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to sync account status",
            request_id,
        ),
        AccountServiceError::Delete => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to delete account",
            request_id,
        ),
        AccountServiceError::LoadCookies => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to load account cookies",
            request_id,
        ),
        AccountServiceError::StoreCookies => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to store account cookies",
            request_id,
        ),
        AccountServiceError::DeleteCookies => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to delete account cookies",
            request_id,
        ),
        AccountServiceError::NoValidCookies => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "No valid cookies found",
            request_id,
        ),
        AccountServiceError::EmptyIds => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "Account ids are required",
            request_id,
        ),
        AccountServiceError::InvalidStatus(message) => {
            AdminError::new(StatusCode::BAD_REQUEST, 40001, message, request_id)
        }
        AccountServiceError::LabelTooLong => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "Account label must be 64 characters or fewer",
            request_id,
        ),
        AccountServiceError::QuotaWarnings => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to load account quota warnings",
            request_id,
        ),
    }
}

fn refresh_account_error(error: RefreshAccountError, request_id: &str) -> AdminError {
    match error {
        RefreshAccountError::RepositoryUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Account repository is not initialized",
            request_id,
        ),
        RefreshAccountError::Load => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to load account",
            request_id,
        ),
        RefreshAccountError::NotFound => AdminError::new(
            StatusCode::NOT_FOUND,
            40401,
            "Account not found",
            request_id,
        ),
        RefreshAccountError::TokenRefresherUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Token refresher is not initialized",
            request_id,
        ),
        RefreshAccountError::StoreRefreshed => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to store refreshed account",
            request_id,
        ),
    }
}

pub(super) enum AccountQuotaHttpError {
    Standard(AdminError),
    UpstreamFetch { error: String, request_id: String },
}

impl From<AdminError> for AccountQuotaHttpError {
    fn from(error: AdminError) -> Self {
        Self::Standard(error)
    }
}

impl IntoResponse for AccountQuotaHttpError {
    fn into_response(self) -> axum::response::Response {
        match self {
            Self::Standard(error) => error.into_response(),
            Self::UpstreamFetch { error, request_id } => AdminResponse::new(
                StatusCode::BAD_GATEWAY,
                AdminEnvelope::new(
                    50201,
                    "Failed to fetch quota from Codex API",
                    json!({ "error": error }),
                    request_id,
                ),
            )
            .into_response(),
        }
    }
}

fn account_quota_error(error: AccountQuotaError, request_id: &str) -> AccountQuotaHttpError {
    match error {
        AccountQuotaError::RepositoryUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Account repository is not initialized",
            request_id,
        )
        .into(),
        AccountQuotaError::Load => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to load account",
            request_id,
        )
        .into(),
        AccountQuotaError::NotFound => AdminError::new(
            StatusCode::NOT_FOUND,
            40401,
            "Account not found",
            request_id,
        )
        .into(),
        AccountQuotaError::Inactive(status) => AdminError::new(
            StatusCode::CONFLICT,
            40901,
            format!(
                "Account is {}, cannot query quota",
                account_status_value(status)
            ),
            request_id,
        )
        .into(),
        AccountQuotaError::StoreQuota => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to store account quota",
            request_id,
        )
        .into(),
        AccountQuotaError::Fetch(error) => AccountQuotaHttpError::UpstreamFetch {
            error,
            request_id: request_id.to_string(),
        },
    }
}

fn health_check_error(error: HealthCheckError, request_id: &str) -> AdminError {
    match error {
        HealthCheckError::RepositoryUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Account repository is not initialized",
            request_id,
        ),
        HealthCheckError::List => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to list accounts",
            request_id,
        ),
    }
}

fn account_probe_data_from_service(result: AccountProbeResult) -> AccountProbeData {
    AccountProbeData {
        id: result.id,
        email: result.email,
        previous_status: account_status_value(result.previous_status).to_string(),
        result: result.outcome.as_str().to_string(),
        status: result
            .status
            .map(account_status_value)
            .map(ToString::to_string),
        error: result.error,
        duration_ms: result.duration_ms,
    }
}

pub async fn accounts(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<AccountsQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let limit = clamp_limit(query.limit.unwrap_or(50));
    let page = state
        .services
        .accounts
        .list(query.cursor, limit)
        .await
        .map_err(|error| account_service_error(error, &request_id))?;
    let Page { items, next_cursor } = page;
    let page = Page {
        items: items.into_iter().map(AdminAccountData::from).collect(),
        next_cursor,
    };

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminPageEnvelope::ok(page, limit, request_id),
    ))
}

pub async fn export_accounts(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<AccountExportQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    let format = match parse_account_export_format(query.format.as_deref()) {
        Ok(format) => format,
        Err(message) => {
            return Err(AdminError::new(
                StatusCode::BAD_REQUEST,
                40001,
                message,
                request_id,
            ));
        }
    };
    require_admin_session(&state, &headers, &request_id).await?;

    let ids = account_export_ids(query.ids.as_deref());
    let accounts = state
        .services
        .accounts
        .export(ids)
        .await
        .map_err(|error| account_service_error(error, &request_id))?;

    // 账号导出会返回可重新导入的 OAuth token；只允许 admin session 访问，不写入日志。
    let data = match format {
        AccountExportFormat::Native => native_account_export(accounts),
        AccountExportFormat::Sub2Api => sub2api_account_export(accounts),
    };
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(data, request_id),
    ))
}

pub async fn create_account(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<CreateAccountRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let stored = state
        .services
        .accounts
        .import_validated(payload.token, payload.refresh_token)
        .await
        .map_err(|error| validated_account_import_error(error, &request_id))?;

    // 手动添加账号的响应只返回可展示元数据，OAuth token 永不回显。
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(admin_account_data_from_stored(stored), request_id),
    ))
}

pub async fn health_check_accounts(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    payload: Option<Json<HealthCheckRequest>>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    let payload = payload.map(|Json(payload)| payload).unwrap_or_default();
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
    require_admin_session(&state, &headers, &request_id).await?;

    let concurrency = usize::from(payload.concurrency.unwrap_or(2));
    let stagger_ms = payload.stagger_ms.unwrap_or(3_000);
    let results = state
        .services
        .accounts
        .health_check_accounts(payload.ids, concurrency, stagger_ms, &request_id)
        .await
        .map_err(|error| health_check_error(error, &request_id))?
        .into_iter()
        .map(account_probe_data_from_service)
        .collect::<Vec<_>>();
    let summary = HealthCheckSummary {
        total: results.len(),
        alive: results
            .iter()
            .filter(|result| result.result == "alive")
            .count(),
        dead: results
            .iter()
            .filter(|result| result.result == "dead")
            .count(),
        skipped: results
            .iter()
            .filter(|result| result.result == "skipped")
            .count(),
    };

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(HealthCheckData { summary, results }, request_id),
    ))
}

pub async fn refresh_account(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let result = state
        .services
        .accounts
        .refresh_account(&account_id)
        .await
        .map_err(|error| refresh_account_error(error, &request_id))?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(account_probe_data_from_service(result), request_id),
    ))
}

pub async fn reset_account_usage(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    if !state
        .services
        .accounts
        .reset_usage(&account_id)
        .await
        .map_err(|error| account_service_error(error, &request_id))?
    {
        return Err(AdminError::new(
            StatusCode::NOT_FOUND,
            40401,
            "Account not found",
            request_id,
        ));
    }

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            ResetAccountUsageData {
                id: account_id,
                reset: true,
            },
            request_id,
        ),
    ))
}

pub(super) async fn account_quota(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
) -> Result<impl IntoResponse, AccountQuotaHttpError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let result = state
        .services
        .accounts
        .account_quota(&account_id, &request_id)
        .await
        .map_err(|error| account_quota_error(error, &request_id))?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            AccountQuotaData {
                quota: result.quota,
                raw: result.raw,
            },
            request_id,
        ),
    ))
}

pub(super) async fn quota_warnings(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let warnings = state
        .services
        .accounts
        .quota_warnings()
        .await
        .map_err(|error| account_service_error(error, &request_id))?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AccountQuotaWarningsData::from(warnings), request_id),
    ))
}

pub async fn update_account_label(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
    Json(payload): Json<UpdateAccountLabelRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let label = payload.label;
    let updated = state
        .services
        .accounts
        .update_label(&account_id, label.clone())
        .await
        .map_err(|error| account_service_error(error, &request_id))?;

    if !updated {
        return Err(AdminError::new(
            StatusCode::NOT_FOUND,
            40401,
            "Account not found",
            request_id,
        ));
    }

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            UpdateAccountLabelData {
                id: account_id,
                label,
            },
            request_id,
        ),
    ))
}

pub async fn update_account_status(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
    Json(payload): Json<UpdateAccountStatusRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let updated = state
        .services
        .accounts
        .update_status(&account_id, &payload.status)
        .await
        .map_err(|error| account_service_error(error, &request_id))?
        .ok_or_else(|| {
            AdminError::new(
                StatusCode::NOT_FOUND,
                40401,
                "Account not found",
                request_id.as_str(),
            )
        })?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            UpdateAccountStatusData {
                id: updated.id,
                status: account_status_value(updated.status).to_string(),
            },
            request_id,
        ),
    ))
}

pub async fn delete_account(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let deleted = state
        .services
        .accounts
        .delete(&account_id)
        .await
        .map_err(|error| account_service_error(error, &request_id))?;

    if !deleted {
        return Err(AdminError::new(
            StatusCode::NOT_FOUND,
            40401,
            "Account not found",
            request_id,
        ));
    }

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(DeleteAccountData { deleted: true }, request_id),
    ))
}

pub async fn batch_delete_accounts(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<BatchDeleteAccountsRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    if payload.ids.is_empty() {
        return Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "Account ids are required",
            request_id,
        ));
    }
    require_admin_session(&state, &headers, &request_id).await?;

    let deleted = state
        .services
        .accounts
        .batch_delete(payload.ids)
        .await
        .map_err(|error| account_service_error(error, &request_id))?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            BatchDeleteAccountsData {
                deleted: deleted.deleted,
                not_found: deleted.not_found,
            },
            request_id,
        ),
    ))
}

pub async fn batch_update_account_status(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<BatchUpdateAccountStatusRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    if payload.ids.is_empty() {
        return Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "Account ids are required",
            request_id,
        ));
    }
    parse_admin_account_status(&payload.status).map_err(|message| {
        AdminError::new(StatusCode::BAD_REQUEST, 40001, message, request_id.as_str())
    })?;
    require_admin_session(&state, &headers, &request_id).await?;

    let updated = state
        .services
        .accounts
        .batch_update_status(payload.ids, &payload.status)
        .await
        .map_err(|error| account_service_error(error, &request_id))?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            BatchUpdateAccountStatusData {
                updated: updated.updated,
                not_found: updated.not_found,
            },
            request_id,
        ),
    ))
}

pub async fn get_account_cookies(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let cookies = state
        .services
        .accounts
        .get_cookies(&account_id)
        .await
        .map_err(|error| account_service_error(error, &request_id))?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AccountCookiesData { cookies }, request_id),
    ))
}

pub async fn set_account_cookies(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
    Json(payload): Json<SetAccountCookiesRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    let cookie_header = match admin_cookie_header(&payload.cookies) {
        Ok(cookie_header) => cookie_header,
        Err(message) => {
            return Err(AdminError::new(
                StatusCode::BAD_REQUEST,
                40001,
                message,
                request_id,
            ));
        }
    };
    require_admin_session(&state, &headers, &request_id).await?;

    let cookies = state
        .services
        .accounts
        .set_cookies(&account_id, &cookie_header)
        .await
        .map_err(|error| account_service_error(error, &request_id))?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AccountCookiesData { cookies }, request_id),
    ))
}

pub async fn delete_account_cookies(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    state
        .services
        .accounts
        .delete_cookies(&account_id)
        .await
        .map_err(|error| account_service_error(error, &request_id))?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(DeleteAccountCookiesData { deleted: true }, request_id),
    ))
}

pub async fn import_accounts(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    if !state.services.accounts.has_repository() {
        return Err(account_service_error(
            AccountServiceError::RepositoryUnavailable,
            &request_id,
        ));
    }
    let parsed = parse_account_import_payload(&payload);
    if parsed.accounts.is_empty() {
        return Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "No importable accounts found",
            request_id,
        ));
    }

    let counts = state
        .services
        .accounts
        .import_entries(parsed.accounts)
        .await
        .map_err(|error| store_import_account_error(error, &request_id))?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            AccountImportData {
                imported: counts.imported,
                skipped: counts.skipped,
                source_format: parsed.source_format.as_str().to_string(),
            },
            request_id,
        ),
    ))
}

pub async fn import_cli_auth(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let payload = if body.is_empty() {
        ImportCliAuthRequest::default()
    } else {
        match serde_json::from_slice::<ImportCliAuthRequest>(&body) {
            Ok(payload) => payload,
            Err(_) => {
                return Err(AdminError::new(
                    StatusCode::BAD_REQUEST,
                    40001,
                    "Invalid CLI import request",
                    request_id,
                ));
            }
        }
    };
    let codex_home = match empty_to_none(payload.codex_home) {
        Some(path) => std::path::PathBuf::from(path),
        None => match default_codex_home() {
            Ok(path) => path,
            Err(error) => {
                return Err(AdminError::new(
                    StatusCode::BAD_REQUEST,
                    40001,
                    error.to_string(),
                    request_id,
                ));
            }
        },
    };
    let cli_auth = match read_cli_auth_from_home(&codex_home) {
        Ok(auth) => auth,
        Err(error) => {
            return Err(AdminError::new(
                StatusCode::BAD_REQUEST,
                40001,
                error.to_string(),
                request_id,
            ));
        }
    };
    let _stored = state
        .services
        .accounts
        .import_validated(
            Some(cli_auth.access_token().to_string()),
            cli_auth.refresh_token().map(str::to_string),
        )
        .await
        .map_err(|error| validated_account_import_error(error, &request_id))?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            AccountImportData {
                imported: 1,
                skipped: 0,
                source_format: AccountImportFormat::CodexCli.as_str().to_string(),
            },
            request_id,
        ),
    ))
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountsQuery {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountExportQuery {
    pub ids: Option<String>,
    pub format: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountImportFormat {
    Native,
    Sub2Api,
    CodexCli,
}

impl AccountImportFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Sub2Api => "sub2api",
            Self::CodexCli => "codex_cli",
        }
    }
}

#[derive(Debug, Clone)]
struct ParsedAccountImportPayload {
    accounts: Vec<AccountImportEntry>,
    source_format: AccountImportFormat,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountImportData {
    pub imported: u32,
    pub skipped: u32,
    pub source_format: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateAccountRequest {
    pub token: Option<String>,
    pub refresh_token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportCliAuthRequest {
    pub codex_home: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthCheckRequest {
    pub ids: Option<Vec<String>>,
    #[serde(alias = "stagger_ms")]
    pub stagger_ms: Option<u64>,
    pub concurrency: Option<u8>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthCheckData {
    pub summary: HealthCheckSummary,
    pub results: Vec<AccountProbeData>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthCheckSummary {
    pub total: usize,
    pub alive: usize,
    pub dead: usize,
    pub skipped: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountProbeData {
    pub id: String,
    pub email: Option<String>,
    pub previous_status: String,
    pub result: String,
    pub status: Option<String>,
    pub error: Option<String>,
    pub duration_ms: Option<u128>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetAccountUsageData {
    pub id: String,
    pub reset: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountQuotaData {
    pub quota: Value,
    pub raw: Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountQuotaWarningsData {
    pub warnings: Vec<AccountQuotaWarningData>,
    pub updated_at: Option<String>,
}

impl From<ServiceAccountQuotaWarnings> for AccountQuotaWarningsData {
    fn from(warnings: ServiceAccountQuotaWarnings) -> Self {
        Self {
            warnings: warnings.warnings.into_iter().map(Into::into).collect(),
            updated_at: warnings
                .updated_at
                .map(|value| value.to_rfc3339_opts(SecondsFormat::Secs, true)),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountQuotaWarningData {
    pub account_id: String,
    pub email: Option<String>,
    pub window: String,
    pub level: String,
    pub used_percent: f64,
    pub reset_at: Option<i64>,
}

impl From<ServiceAccountQuotaWarning> for AccountQuotaWarningData {
    fn from(warning: ServiceAccountQuotaWarning) -> Self {
        Self {
            account_id: warning.account_id,
            email: warning.email,
            window: warning.window.as_str().to_string(),
            level: warning.level.as_str().to_string(),
            used_percent: warning.used_percent,
            reset_at: warning.reset_at,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountExportFormat {
    Native,
    Sub2Api,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAccountLabelRequest {
    pub label: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAccountLabelData {
    pub id: String,
    pub label: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAccountStatusRequest {
    pub status: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAccountStatusData {
    pub id: String,
    pub status: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteAccountData {
    pub deleted: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeleteAccountsRequest {
    pub ids: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeleteAccountsData {
    pub deleted: u32,
    pub not_found: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchUpdateAccountStatusRequest {
    pub ids: Vec<String>,
    pub status: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchUpdateAccountStatusData {
    pub updated: u32,
    pub not_found: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetAccountCookiesRequest {
    pub cookies: Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountCookiesData {
    pub cookies: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteAccountCookiesData {
    pub deleted: bool,
}

#[derive(Debug, Serialize)]
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

impl From<StoredAccountMetadata> for AdminAccountData {
    fn from(account: StoredAccountMetadata) -> Self {
        Self {
            id: account.id,
            email: account.email,
            account_id: account.account_id,
            user_id: account.user_id,
            label: account.label,
            plan_type: account.plan_type,
            status: account_status_value(account.status).to_string(),
            access_token_expires_at: account
                .access_token_expires_at
                .map(|value| value.to_rfc3339()),
            added_at: account.added_at.to_rfc3339(),
            updated_at: account.updated_at.to_rfc3339(),
        }
    }
}

fn admin_account_data_from_stored(account: StoredAccount) -> AdminAccountData {
    AdminAccountData {
        id: account.id,
        email: account.email,
        account_id: account.account_id,
        user_id: account.user_id,
        label: account.label,
        plan_type: account.plan_type,
        status: account_status_value(account.status).to_string(),
        access_token_expires_at: account
            .access_token_expires_at
            .map(|value| value.to_rfc3339()),
        added_at: account.added_at.to_rfc3339(),
        updated_at: account.updated_at.to_rfc3339(),
    }
}

pub(super) fn validated_account_import_error(
    error: ValidatedAccountImportError,
    request_id: &str,
) -> AdminError {
    match error {
        ValidatedAccountImportError::RepositoryUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Account repository is not initialized",
            request_id,
        ),
        ValidatedAccountImportError::TokenRequired => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "Either token or refreshToken is required",
            request_id,
        ),
        ValidatedAccountImportError::TokenRefresherUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Token refresher is not initialized",
            request_id,
        ),
        ValidatedAccountImportError::RefreshTransport => AdminError::new(
            StatusCode::BAD_GATEWAY,
            50201,
            "Refresh token exchange failed",
            request_id,
        ),
        ValidatedAccountImportError::RefreshRejected => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "Refresh token exchange failed",
            request_id,
        ),
        ValidatedAccountImportError::InvalidToken(message) => {
            AdminError::new(StatusCode::BAD_REQUEST, 40001, message, request_id)
        }
        ValidatedAccountImportError::Inspect => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to inspect account",
            request_id,
        ),
        ValidatedAccountImportError::NotFound => AdminError::new(
            StatusCode::NOT_FOUND,
            40401,
            "Account not found",
            request_id,
        ),
        ValidatedAccountImportError::Update => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to update account",
            request_id,
        ),
        ValidatedAccountImportError::Insert => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to create account",
            request_id,
        ),
        ValidatedAccountImportError::Load => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to load account",
            request_id,
        ),
    }
}

fn store_import_account_error(error: StoreImportAccountError, request_id: &str) -> AdminError {
    match error {
        StoreImportAccountError::RepositoryUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Account repository is not initialized",
            request_id,
        ),
        StoreImportAccountError::Inspect => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to inspect account",
            request_id,
        ),
        StoreImportAccountError::Invalid(message) => {
            AdminError::new(StatusCode::BAD_REQUEST, 40001, message, request_id)
        }
        StoreImportAccountError::Insert => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to import account",
            request_id,
        ),
    }
}

pub(super) fn account_export_ids(value: Option<&str>) -> Vec<String> {
    value
        .into_iter()
        .flat_map(|ids| ids.split(','))
        .filter_map(|id| {
            let id = id.trim();
            (!id.is_empty()).then(|| id.to_string())
        })
        .collect()
}

fn parse_account_export_format(value: Option<&str>) -> Result<AccountExportFormat, &'static str> {
    match value.unwrap_or("native") {
        "" | "native" | "full" => Ok(AccountExportFormat::Native),
        "sub2api" => Ok(AccountExportFormat::Sub2Api),
        _ => Err("Unsupported account export format"),
    }
}

fn native_account_export(accounts: Vec<StoredAccount>) -> Value {
    json!({
        "sourceFormat": "native",
        "accounts": accounts.into_iter().map(native_export_account).collect::<Vec<_>>(),
    })
}

fn native_export_account(account: StoredAccount) -> Value {
    json!({
        "id": account.id,
        "email": account.email,
        "accountId": account.account_id,
        "userId": account.user_id,
        "label": account.label,
        "planType": account.plan_type,
        "token": account.access_token.expose_secret(),
        "refreshToken": account
            .refresh_token
            .as_ref()
            .map(|token| token.expose_secret().to_string()),
        "status": account_status_value(account.status),
        "accessTokenExpiresAt": account.access_token_expires_at.map(|value| value.to_rfc3339()),
        "addedAt": account.added_at.to_rfc3339(),
        "updatedAt": account.updated_at.to_rfc3339(),
    })
}

fn sub2api_account_export(accounts: Vec<StoredAccount>) -> Value {
    json!({
        "exported_at": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        "proxies": [],
        "accounts": accounts.into_iter().map(sub2api_export_account).collect::<Vec<_>>(),
        "type": "sub2api-data",
        "version": 1,
    })
}

fn sub2api_export_account(account: StoredAccount) -> Value {
    let mut credentials = Map::new();
    credentials.insert(
        "access_token".to_string(),
        Value::String(account.access_token.expose_secret().to_string()),
    );
    insert_optional_string(
        &mut credentials,
        "refresh_token",
        account
            .refresh_token
            .as_ref()
            .map(|token| token.expose_secret().to_string()),
    );
    insert_optional_string(&mut credentials, "email", account.email.clone());
    insert_optional_string(&mut credentials, "chatgpt_account_id", account.account_id);
    insert_optional_string(&mut credentials, "chatgpt_user_id", account.user_id);
    insert_optional_string(&mut credentials, "plan_type", account.plan_type);
    insert_optional_string(
        &mut credentials,
        "expires_at",
        account
            .access_token_expires_at
            .map(|value| value.to_rfc3339()),
    );

    json!({
        "name": account
            .label
            .as_deref()
            .filter(|label| !label.trim().is_empty())
            .map(str::to_string)
            .or_else(|| account.email.clone())
            .unwrap_or_else(|| account.id.clone()),
        "platform": "openai",
        "type": "oauth",
        "credentials": credentials,
        "concurrency": 0,
        "priority": 0,
    })
}

fn insert_optional_string(map: &mut Map<String, Value>, key: &str, value: Option<String>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        map.insert(key.to_string(), Value::String(value));
    }
}

fn parse_account_import_payload(payload: &Value) -> ParsedAccountImportPayload {
    if let Some(accounts) = parse_sub2api_oauth_payload(payload) {
        return ParsedAccountImportPayload {
            accounts,
            source_format: AccountImportFormat::Sub2Api,
        };
    }

    parse_native_account_payload(payload)
}

fn parse_sub2api_oauth_payload(payload: &Value) -> Option<Vec<AccountImportEntry>> {
    let accounts = payload.get("accounts")?.as_array()?;
    let looks_like_sub2api = string_at(payload, &["type"]).as_deref() == Some("sub2api-data")
        || payload.get("proxies").is_some()
        || accounts
            .iter()
            .any(|account| account.get("credentials").is_some());
    if !looks_like_sub2api {
        return None;
    }

    Some(
        accounts
            .iter()
            .filter_map(sub2api_oauth_account_entry)
            .collect(),
    )
}

fn sub2api_oauth_account_entry(account: &Value) -> Option<AccountImportEntry> {
    let platform = string_at(account, &["platform"])?.to_ascii_lowercase();
    let account_type = string_at(account, &["type"])?.to_ascii_lowercase();
    if platform != "openai" || account_type != "oauth" {
        return None;
    }
    let credentials = account.get("credentials")?;
    let fallback_label = normalized_label(string_at(account, &["name"]));
    let mut entry = account_entry_from_value(credentials, fallback_label);
    if entry.token.is_none() && entry.refresh_token.is_none() {
        return None;
    }
    if entry.email.is_none() {
        entry.email = string_at(credentials, &["email"]);
    }
    if entry.account_id.is_none() {
        entry.account_id = first_string(
            credentials,
            &[&["chatgpt_account_id"], &["account_id"], &["accountId"]],
        );
    }
    if entry.user_id.is_none() {
        entry.user_id = first_string(
            credentials,
            &[&["chatgpt_user_id"], &["user_id"], &["userId"]],
        );
    }
    if entry.plan_type.is_none() {
        entry.plan_type = first_string(credentials, &[&["plan_type"], &["planType"]]);
    }
    Some(entry)
}

fn parse_native_account_payload(payload: &Value) -> ParsedAccountImportPayload {
    if let Some(accounts) = payload.as_array() {
        return ParsedAccountImportPayload {
            accounts: accounts
                .iter()
                .filter_map(|account| {
                    let entry = account_entry_from_value(account, None);
                    (entry.token.is_some() || entry.refresh_token.is_some()).then_some(entry)
                })
                .collect(),
            source_format: AccountImportFormat::Native,
        };
    }

    if let Some(accounts) = payload.get("accounts").and_then(Value::as_array) {
        let source_format = if looks_like_sub2api_native_export(accounts) {
            AccountImportFormat::Sub2Api
        } else {
            AccountImportFormat::Native
        };
        return ParsedAccountImportPayload {
            accounts: accounts
                .iter()
                .filter_map(|account| {
                    let entry = account_entry_from_value(account, None);
                    (entry.token.is_some() || entry.refresh_token.is_some()).then_some(entry)
                })
                .collect(),
            source_format,
        };
    }

    let entry = account_entry_from_value(payload, None);
    let accounts = if entry.token.is_some() || entry.refresh_token.is_some() {
        vec![entry]
    } else {
        Vec::new()
    };
    ParsedAccountImportPayload {
        accounts,
        source_format: AccountImportFormat::Native,
    }
}

fn looks_like_sub2api_native_export(accounts: &[Value]) -> bool {
    accounts.iter().any(|account| {
        // sub2api 兼容导出会携带代理/配额运行态字段；这里只用于格式识别，代理数据不进入 Rust 服务。
        account.get("proxyApiKey").is_some()
            || account.get("cachedQuota").is_some()
            || account.get("quotaVerifyRequired").is_some()
    })
}

fn account_entry_from_value(value: &Value, fallback_label: Option<String>) -> AccountImportEntry {
    let token = first_string(
        value,
        &[
            &["token"],
            &["accessToken"],
            &["access_token"],
            &["tokens", "accessToken"],
            &["tokens", "access_token"],
            &["credentials", "token"],
            &["credentials", "accessToken"],
            &["credentials", "access_token"],
        ],
    )
    .map(normalize_bearer_token);
    let refresh_token = first_string(
        value,
        &[
            &["refreshToken"],
            &["refresh_token"],
            &["tokens", "refreshToken"],
            &["tokens", "refresh_token"],
            &["credentials", "refreshToken"],
            &["credentials", "refresh_token"],
        ],
    );

    AccountImportEntry {
        id: first_string(value, &[&["id"]]),
        email: first_string(value, &[&["email"], &["credentials", "email"]]),
        account_id: first_string(
            value,
            &[
                &["accountId"],
                &["account_id"],
                &["chatgpt_account_id"],
                &["credentials", "accountId"],
                &["credentials", "account_id"],
                &["credentials", "chatgpt_account_id"],
            ],
        ),
        user_id: first_string(
            value,
            &[
                &["userId"],
                &["user_id"],
                &["chatgpt_user_id"],
                &["credentials", "userId"],
                &["credentials", "user_id"],
                &["credentials", "chatgpt_user_id"],
            ],
        ),
        label: label_from_value(value).or(fallback_label),
        plan_type: first_string(
            value,
            &[
                &["planType"],
                &["plan_type"],
                &["credentials", "planType"],
                &["credentials", "plan_type"],
            ],
        ),
        token,
        refresh_token,
        status: first_string(value, &[&["status"]]),
    }
}

fn label_from_value(value: &Value) -> Option<String> {
    normalized_label(first_string(
        value,
        &[
            &["label"],
            &["name"],
            &["account_name"],
            &["accountName"],
            &["account_note"],
            &["accountNote"],
            &["note"],
        ],
    ))
}

fn normalized_label(value: Option<String>) -> Option<String> {
    value.map(|label| label.chars().take(64).collect())
}

fn normalize_bearer_token(value: String) -> String {
    let trimmed = value.trim();
    trimmed
        .strip_prefix("Bearer ")
        .or_else(|| trimmed.strip_prefix("bearer "))
        .unwrap_or(trimmed)
        .trim()
        .to_string()
}

fn empty_to_none(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_admin_account_status(status: &str) -> Result<AccountStatus, String> {
    match status.trim().to_ascii_lowercase().as_str() {
        "active" => Ok(AccountStatus::Active),
        "disabled" => Ok(AccountStatus::Disabled),
        other => Err(format!("Unsupported account status: {other}")),
    }
}

fn admin_cookie_header(value: &Value) -> Result<String, &'static str> {
    if let Some(cookies) = value.as_str() {
        let cookies = cookies.trim();
        if cookies.is_empty() {
            return Err("cookies field is required");
        }
        return Ok(cookies.to_string());
    }
    let Some(object) = value.as_object() else {
        return Err("cookies must be a string or object");
    };
    if object.is_empty() {
        return Err("cookies field is required");
    }
    let pairs = object
        .iter()
        .filter_map(|(name, value)| {
            let value = value.as_str()?.trim();
            (!name.trim().is_empty() && !value.is_empty())
                .then(|| format!("{}={value}", name.trim()))
        })
        .collect::<Vec<_>>();
    if pairs.is_empty() {
        Err("No valid cookies found")
    } else {
        Ok(pairs.join("; "))
    }
}

pub(super) fn account_status_value(status: AccountStatus) -> &'static str {
    match status {
        AccountStatus::Active => "active",
        AccountStatus::Expired => "expired",
        AccountStatus::QuotaExhausted => "quota_exhausted",
        AccountStatus::Refreshing => "refreshing",
        AccountStatus::Disabled => "disabled",
        AccountStatus::Banned => "banned",
    }
}
