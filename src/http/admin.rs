use axum::{
    extract::{Path, Query, State},
    http::{
        header::{HeaderValue, SET_COOKIE},
        HeaderMap, StatusCode,
    },
    response::{IntoResponse, Response},
    Extension, Json,
};
use chrono::{Duration, SecondsFormat, Utc};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::{
    accounts::{
        model::{Account, AccountStatus},
        repository::{
            AccountRepositoryError, AccountUsageListRecord, AccountUsageSummary, NewAccount,
            StoredAccount, StoredAccountMetadata,
        },
    },
    auth::admin_session::verify_admin_password,
    auth::api_key_repository::StoredClientApiKey,
    codex::client::{build_reqwest_client, CodexBackendClient, CodexRequestContext},
    config::{AppConfig, QuotaWarningThresholds},
    fingerprint::model::Fingerprint,
    http::{auth::admin_session_id, middleware::RequestId},
    models::catalog::{ModelCatalog, ModelPlanSnapshot},
    pagination::{clamp_limit, Page},
    state::AppState,
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminEnvelope<T> {
    pub code: u32,
    pub message: String,
    pub data: T,
    pub request_id: String,
}

impl<T> AdminEnvelope<T> {
    pub fn new(
        code: u32,
        message: impl Into<String>,
        data: T,
        request_id: impl Into<String>,
    ) -> Self {
        // body code 给前端做业务分支，HTTP status 仍然是传输层真相。
        Self {
            code,
            message: message.into(),
            data,
            request_id: request_id.into(),
        }
    }

    pub fn ok(data: T, request_id: impl Into<String>) -> Self {
        Self::new(200, "OK", data, request_id)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageMeta {
    pub limit: u32,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminPageEnvelope<T> {
    pub code: u32,
    pub message: String,
    pub data: Vec<T>,
    pub page: PageMeta,
    pub request_id: String,
}

impl<T> AdminPageEnvelope<T> {
    pub fn new(
        code: u32,
        message: impl Into<String>,
        page: Page<T>,
        limit: u32,
        request_id: impl Into<String>,
    ) -> Self {
        let Page { items, next_cursor } = page;
        Self {
            code,
            message: message.into(),
            data: items,
            page: PageMeta { limit, next_cursor },
            request_id: request_id.into(),
        }
    }

    pub fn ok(page: Page<T>, limit: u32, request_id: impl Into<String>) -> Self {
        Self::new(200, "OK", page, limit, request_id)
    }
}

#[derive(Debug, Clone)]
pub struct AdminResponse<T> {
    pub status: StatusCode,
    pub body: T,
}

impl<T> AdminResponse<T> {
    pub fn new(status: StatusCode, body: T) -> Self {
        Self { status, body }
    }
}

impl<T> IntoResponse for AdminResponse<T>
where
    T: Serialize,
{
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogsQuery {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeysQuery {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageStatsQuery {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginRequest {
    pub password: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginData {
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminSettingsData {
    pub default_model: String,
    pub default_reasoning_effort: Option<String>,
    pub service_tier: Option<String>,
    pub model_aliases: std::collections::BTreeMap<String, String>,
    pub refresh_enabled: bool,
    pub refresh_margin_seconds: u64,
    pub refresh_concurrency: u32,
    pub max_concurrent_per_account: usize,
    pub request_interval_ms: u64,
    pub rotation_strategy: String,
    pub tier_priority: Vec<String>,
    pub quota_refresh_interval_minutes: u64,
    pub quota_warning_thresholds: QuotaWarningThresholds,
    pub quota_skip_exhausted: bool,
    pub logs_enabled: bool,
    pub logs_capacity: u32,
    pub logs_capture_body: bool,
    pub usage_history_retention_days: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountImportFormat {
    Native,
    Sub2Api,
}

impl AccountImportFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Sub2Api => "sub2api",
        }
    }
}

#[derive(Debug, Clone)]
struct ParsedAccountImportPayload {
    accounts: Vec<AccountImportEntry>,
    source_format: AccountImportFormat,
}

#[derive(Debug, Clone)]
struct AccountImportEntry {
    pub id: Option<String>,
    pub email: Option<String>,
    pub account_id: Option<String>,
    pub user_id: Option<String>,
    pub label: Option<String>,
    pub plan_type: Option<String>,
    pub token: Option<String>,
    pub refresh_token: Option<String>,
    pub status: Option<String>,
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
    pub id: Option<String>,
    pub email: Option<String>,
    pub account_id: Option<String>,
    pub user_id: Option<String>,
    pub label: Option<String>,
    pub plan_type: Option<String>,
    pub token: Option<String>,
    pub refresh_token: Option<String>,
    pub status: Option<String>,
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
pub struct RefreshModelsData {
    pub refreshed_plans: usize,
    pub model_count: usize,
    pub failed_plans: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateApiKeyRequest {
    pub name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientApiKeyData {
    pub id: String,
    pub name: String,
    pub label: Option<String>,
    pub prefix: String,
    pub enabled: bool,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedClientApiKeyData {
    pub id: String,
    pub name: String,
    pub label: Option<String>,
    pub prefix: String,
    pub enabled: bool,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub plaintext: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateClientApiKeyStatusRequest {
    pub status: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateClientApiKeyLabelRequest {
    pub label: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateClientApiKeyStatusData {
    pub id: String,
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeleteClientApiKeysRequest {
    pub ids: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeleteClientApiKeysData {
    pub deleted: u32,
    pub not_found: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteClientApiKeyData {
    pub deleted: bool,
}

impl From<StoredClientApiKey> for ClientApiKeyData {
    fn from(key: StoredClientApiKey) -> Self {
        Self {
            id: key.id,
            name: key.name,
            label: key.label,
            prefix: key.prefix,
            enabled: key.enabled,
            created_at: key.created_at,
            last_used_at: key.last_used_at,
        }
    }
}

impl CreatedClientApiKeyData {
    fn new(key: StoredClientApiKey, plaintext: String) -> Self {
        Self {
            id: key.id,
            name: key.name,
            label: key.label,
            prefix: key.prefix,
            enabled: key.enabled,
            created_at: key.created_at,
            last_used_at: key.last_used_at,
            plaintext,
        }
    }
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminUsageStatsData {
    pub account_id: String,
    pub email: Option<String>,
    pub label: Option<String>,
    pub plan_type: Option<String>,
    pub request_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
    pub last_used_at: Option<String>,
}

impl From<AccountUsageListRecord> for AdminUsageStatsData {
    fn from(usage: AccountUsageListRecord) -> Self {
        Self {
            account_id: usage.account_id,
            email: usage.email,
            label: usage.label,
            plan_type: usage.plan_type,
            request_count: usage.request_count,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cached_tokens: usage.cached_tokens,
            last_used_at: usage.last_used_at.map(|value| value.to_rfc3339()),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminUsageStatsSummaryData {
    pub account_count: i64,
    pub request_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
}

impl From<AccountUsageSummary> for AdminUsageStatsSummaryData {
    fn from(summary: AccountUsageSummary) -> Self {
        Self {
            account_count: summary.account_count,
            request_count: summary.request_count,
            input_tokens: summary.input_tokens,
            output_tokens: summary.output_tokens,
            cached_tokens: summary.cached_tokens,
        }
    }
}

impl AdminSettingsData {
    fn from_config(config: &AppConfig) -> Self {
        Self {
            default_model: config.model.default_model.clone(),
            default_reasoning_effort: config.model.default_reasoning_effort.clone(),
            service_tier: config.model.service_tier.clone(),
            model_aliases: config.model.aliases.clone(),
            refresh_enabled: config.auth.refresh_enabled,
            refresh_margin_seconds: config.auth.refresh_margin_seconds,
            refresh_concurrency: config.auth.refresh_concurrency,
            max_concurrent_per_account: config.auth.max_concurrent_per_account,
            request_interval_ms: config.auth.request_interval_ms,
            rotation_strategy: config.auth.rotation_strategy.clone(),
            tier_priority: config.auth.tier_priority.clone(),
            quota_refresh_interval_minutes: config.quota.refresh_interval_minutes,
            quota_warning_thresholds: config.quota.warning_thresholds.clone(),
            quota_skip_exhausted: config.quota.skip_exhausted,
            logs_enabled: config.logging.enabled,
            logs_capacity: config.logging.capacity,
            logs_capture_body: config.logging.capture_body,
            usage_history_retention_days: config.usage_stats.history_retention_days,
        }
    }
}

pub async fn login(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Json(payload): Json<LoginRequest>,
) -> Response {
    let request_id = request_id.as_str().to_string();
    let Some(pool) = state.db() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Database is not initialized", (), request_id),
        )
        .into_response();
    };

    let admin = match load_first_admin(pool).await {
        Ok(Some(admin)) => admin,
        Ok(None) => {
            return AdminResponse::new(
                StatusCode::UNAUTHORIZED,
                AdminEnvelope::new(40102, "Admin password invalid", (), request_id),
            )
            .into_response();
        }
        Err(_) => {
            return AdminResponse::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                AdminEnvelope::new(50001, "Failed to load admin user", (), request_id),
            )
            .into_response();
        }
    };

    // admin 登录只校验管理员密码；客户端 cpr_ API Key 不能参与后台登录。
    match verify_admin_password(&payload.password, &admin.password_hash) {
        Ok(true) => {}
        Ok(false) => {
            return AdminResponse::new(
                StatusCode::UNAUTHORIZED,
                AdminEnvelope::new(40102, "Admin password invalid", (), request_id),
            )
            .into_response();
        }
        Err(_) => {
            return AdminResponse::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                AdminEnvelope::new(50001, "Failed to verify admin password", (), request_id),
            )
            .into_response();
        }
    }

    let ttl_minutes = state.config().admin.session_ttl_minutes;
    let Ok(ttl_minutes_i64) = i64::try_from(ttl_minutes) else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Admin session ttl is invalid", (), request_id),
        )
        .into_response();
    };
    let expires_at = Utc::now() + Duration::minutes(ttl_minutes_i64);
    let session_id = format!("sess_{}", Uuid::new_v4().simple());
    if create_admin_session(pool, &session_id, &admin.id, expires_at)
        .await
        .is_err()
    {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Failed to create admin session", (), request_id),
        )
        .into_response();
    }

    let Some(cookie) = admin_session_set_cookie(&session_id, ttl_minutes) else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(
                50001,
                "Failed to create admin session cookie",
                (),
                request_id,
            ),
        )
        .into_response();
    };
    let mut response = AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            LoginData {
                expires_at: expires_at.to_rfc3339(),
            },
            request_id,
        ),
    )
    .into_response();
    response.headers_mut().insert(SET_COOKIE, cookie);
    response
}

pub async fn logs(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<LogsQuery>,
) -> Response {
    let request_id = request_id.as_str().to_string();
    let Some(pool) = state.db() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Database is not initialized", (), request_id),
        )
        .into_response();
    };
    if let Err(response) = require_admin_session(pool, &headers, &request_id).await {
        return response;
    }

    let limit = clamp_limit(query.limit.unwrap_or(50));
    let Some(repo) = state.event_logs() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(
                50001,
                "Event log repository is not initialized",
                (),
                request_id,
            ),
        )
        .into_response();
    };
    match repo.list(query.cursor, limit).await {
        Ok(page) => AdminResponse::new(
            StatusCode::OK,
            AdminPageEnvelope::ok(page, limit, request_id),
        )
        .into_response(),
        Err(_) => AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Failed to list event logs", (), request_id),
        )
        .into_response(),
    }
}

pub async fn settings(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let request_id = request_id.as_str().to_string();
    let Some(pool) = state.db() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Database is not initialized", (), request_id),
        )
        .into_response();
    };
    if let Err(response) = require_admin_session(pool, &headers, &request_id).await {
        return response;
    }

    AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AdminSettingsData::from_config(state.config()), request_id),
    )
    .into_response()
}

pub async fn refresh_models(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let request_id = request_id.as_str().to_string();
    let Some(pool) = state.db() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Database is not initialized", (), request_id),
        )
        .into_response();
    };
    if let Err(response) = require_admin_session(pool, &headers, &request_id).await {
        return response;
    }
    let Some(account_repo) = state.account_repository() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(
                50001,
                "Account repository is not initialized",
                (),
                request_id,
            ),
        )
        .into_response();
    };
    let Some(model_repo) = state.model_snapshot_repository() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Model repository is not initialized", (), request_id),
        )
        .into_response();
    };
    let accounts = match account_repo.list_pool_accounts().await {
        Ok(accounts) => accounts,
        Err(_) => {
            return AdminResponse::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                AdminEnvelope::new(50001, "Failed to list accounts", (), request_id),
            )
            .into_response();
        }
    };
    let plan_accounts = distinct_active_plan_accounts(accounts);
    if plan_accounts.is_empty() {
        return AdminResponse::new(
            StatusCode::BAD_REQUEST,
            AdminEnvelope::new(
                40001,
                "No accounts available for model refresh",
                (),
                request_id,
            ),
        )
        .into_response();
    }
    let client = match build_reqwest_client(state.config().tls.force_http11) {
        Ok(client) => CodexBackendClient::new(
            client,
            state.config().api.base_url.clone(),
            Fingerprint::default_codex_desktop(),
        ),
        Err(_) => {
            return AdminResponse::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                AdminEnvelope::new(50001, "Failed to build Codex client", (), request_id),
            )
            .into_response();
        }
    };

    let mut refreshed_plans = 0usize;
    let mut model_count = 0usize;
    let mut failed_plans = 0usize;
    for (plan_type, account) in plan_accounts {
        let context = CodexRequestContext {
            access_token: &account.access_token,
            account_id: account.account_id.as_deref(),
            request_id: &request_id,
            turn_state: None,
            turn_metadata: None,
            beta_features: None,
            include_timing_metrics: None,
            version: None,
            codex_window_id: None,
            parent_thread_id: None,
            cookie_header: None,
        };
        let entries = match client.fetch_models(context).await {
            Ok(entries) if !entries.is_empty() => entries,
            Ok(_) => {
                failed_plans += 1;
                continue;
            }
            Err(error) => {
                tracing::warn!(?error, plan_type, "failed to refresh backend models");
                failed_plans += 1;
                continue;
            }
        };
        let snapshot = ModelPlanSnapshot::from_backend_entries(plan_type, entries);
        model_count += snapshot.models.len();
        if model_repo.replace_plan_snapshot(&snapshot).await.is_err() {
            return AdminResponse::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                AdminEnvelope::new(50001, "Failed to store model snapshot", (), request_id),
            )
            .into_response();
        }
        refreshed_plans += 1;
    }

    let data = RefreshModelsData {
        refreshed_plans,
        model_count,
        failed_plans,
    };
    if refreshed_plans == 0 {
        return AdminResponse::new(
            StatusCode::BAD_GATEWAY,
            AdminEnvelope::new(50201, "Failed to refresh backend models", data, request_id),
        )
        .into_response();
    }
    match model_repo.list_plan_snapshots().await {
        Ok(snapshots) => {
            let allowlist =
                ModelCatalog::from_config_and_snapshots(&state.config().model, &snapshots)
                    .model_plan_allowlist();
            // 刷新后的 model -> plans 要立即同步给调度器，避免新模型被分配到不支持的账号 plan。
            state
                .account_pool()
                .lock()
                .await
                .set_model_plan_allowlist(allowlist);
        }
        Err(_) => {
            return AdminResponse::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                AdminEnvelope::new(50001, "Failed to load model snapshots", (), request_id),
            )
            .into_response();
        }
    }

    AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data, request_id)).into_response()
}

pub async fn usage_stats(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<UsageStatsQuery>,
) -> Response {
    let request_id = request_id.as_str().to_string();
    let Some(pool) = state.db() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Database is not initialized", (), request_id),
        )
        .into_response();
    };
    if let Err(response) = require_admin_session(pool, &headers, &request_id).await {
        return response;
    }
    let Some(repo) = state.account_usage_repository() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(
                50001,
                "Account usage repository is not initialized",
                (),
                request_id,
            ),
        )
        .into_response();
    };

    let limit = clamp_limit(query.limit.unwrap_or(50));
    match repo.list(query.cursor, limit).await {
        Ok(page) => {
            let Page { items, next_cursor } = page;
            let page = Page {
                items: items.into_iter().map(AdminUsageStatsData::from).collect(),
                next_cursor,
            };
            AdminResponse::new(
                StatusCode::OK,
                AdminPageEnvelope::ok(page, limit, request_id),
            )
            .into_response()
        }
        Err(AccountRepositoryError::InvalidCursor) => AdminResponse::new(
            StatusCode::BAD_REQUEST,
            AdminEnvelope::new(40002, "Invalid cursor", (), request_id),
        )
        .into_response(),
        Err(_) => AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Failed to list usage stats", (), request_id),
        )
        .into_response(),
    }
}

pub async fn usage_stats_summary(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let request_id = request_id.as_str().to_string();
    let Some(pool) = state.db() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Database is not initialized", (), request_id),
        )
        .into_response();
    };
    if let Err(response) = require_admin_session(pool, &headers, &request_id).await {
        return response;
    }
    let Some(repo) = state.account_usage_repository() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(
                50001,
                "Account usage repository is not initialized",
                (),
                request_id,
            ),
        )
        .into_response();
    };

    match repo.summary().await {
        Ok(summary) => AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(AdminUsageStatsSummaryData::from(summary), request_id),
        )
        .into_response(),
        Err(_) => AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Failed to summarize usage stats", (), request_id),
        )
        .into_response(),
    }
}

pub async fn accounts(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<AccountsQuery>,
) -> Response {
    let request_id = request_id.as_str().to_string();
    let Some(pool) = state.db() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Database is not initialized", (), request_id),
        )
        .into_response();
    };
    if let Err(response) = require_admin_session(pool, &headers, &request_id).await {
        return response;
    }
    let Some(repo) = state.account_repository() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(
                50001,
                "Account repository is not initialized",
                (),
                request_id,
            ),
        )
        .into_response();
    };

    let limit = clamp_limit(query.limit.unwrap_or(50));
    match repo.list_metadata(query.cursor, limit).await {
        Ok(page) => {
            let Page { items, next_cursor } = page;
            let page = Page {
                items: items.into_iter().map(AdminAccountData::from).collect(),
                next_cursor,
            };
            AdminResponse::new(
                StatusCode::OK,
                AdminPageEnvelope::ok(page, limit, request_id),
            )
            .into_response()
        }
        Err(_) => AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Failed to list accounts", (), request_id),
        )
        .into_response(),
    }
}

pub async fn export_accounts(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<AccountExportQuery>,
) -> Response {
    let request_id = request_id.as_str().to_string();
    let format = match parse_account_export_format(query.format.as_deref()) {
        Ok(format) => format,
        Err(message) => {
            return AdminResponse::new(
                StatusCode::BAD_REQUEST,
                AdminEnvelope::new(40001, message, (), request_id),
            )
            .into_response();
        }
    };
    let Some(pool) = state.db() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Database is not initialized", (), request_id),
        )
        .into_response();
    };
    if let Err(response) = require_admin_session(pool, &headers, &request_id).await {
        return response;
    }
    let Some(repo) = state.account_repository() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(
                50001,
                "Account repository is not initialized",
                (),
                request_id,
            ),
        )
        .into_response();
    };

    let ids = account_export_ids(query.ids.as_deref());
    let accounts = if ids.is_empty() {
        match repo.list_all().await {
            Ok(accounts) => accounts,
            Err(_) => {
                return AdminResponse::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    AdminEnvelope::new(50001, "Failed to export accounts", (), request_id),
                )
                .into_response();
            }
        }
    } else {
        let mut accounts = Vec::with_capacity(ids.len());
        for id in ids {
            match repo.get(&id).await {
                Ok(Some(account)) => accounts.push(account),
                Ok(None) => {}
                Err(_) => {
                    return AdminResponse::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        AdminEnvelope::new(50001, "Failed to export accounts", (), request_id),
                    )
                    .into_response();
                }
            }
        }
        accounts
    };

    // 账号导出会返回可重新导入的 OAuth token；只允许 admin session 访问，不写入日志。
    let data = match format {
        AccountExportFormat::Native => native_account_export(accounts),
        AccountExportFormat::Sub2Api => sub2api_account_export(accounts),
    };
    AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data, request_id)).into_response()
}

pub async fn create_account(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<CreateAccountRequest>,
) -> Response {
    let request_id = request_id.as_str().to_string();
    let access_token = payload.token.as_deref().unwrap_or_default().trim();
    if access_token.is_empty() {
        return AdminResponse::new(
            StatusCode::BAD_REQUEST,
            AdminEnvelope::new(40001, "Account token is required", (), request_id),
        )
        .into_response();
    }
    if payload
        .label
        .as_ref()
        .is_some_and(|label| label.chars().count() > 64)
    {
        return AdminResponse::new(
            StatusCode::BAD_REQUEST,
            AdminEnvelope::new(
                40001,
                "Account label must be 64 characters or fewer",
                (),
                request_id,
            ),
        )
        .into_response();
    }
    let status = match parse_import_status(payload.status.as_deref()) {
        Ok(status) => status,
        Err(message) => {
            return AdminResponse::new(
                StatusCode::BAD_REQUEST,
                AdminEnvelope::new(40001, message, (), request_id),
            )
            .into_response();
        }
    };
    let Some(pool) = state.db() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Database is not initialized", (), request_id),
        )
        .into_response();
    };
    if let Err(response) = require_admin_session(pool, &headers, &request_id).await {
        return response;
    }
    let Some(repo) = state.account_repository() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(
                50001,
                "Account repository is not initialized",
                (),
                request_id,
            ),
        )
        .into_response();
    };

    let id = normalized_account_id(payload.id);
    match repo.exists(&id).await {
        Ok(true) => {
            return AdminResponse::new(
                StatusCode::CONFLICT,
                AdminEnvelope::new(40901, "Account already exists", (), request_id),
            )
            .into_response();
        }
        Ok(false) => {}
        Err(_) => {
            return AdminResponse::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                AdminEnvelope::new(50001, "Failed to inspect account", (), request_id),
            )
            .into_response();
        }
    }

    let email = empty_to_none(payload.email);
    let account_id = empty_to_none(payload.account_id);
    let user_id = empty_to_none(payload.user_id);
    let label = empty_to_none(payload.label);
    let plan_type = empty_to_none(payload.plan_type);
    let refresh_token = empty_to_none(payload.refresh_token);
    let now = Utc::now().to_rfc3339();
    let pool_account = Account {
        id: id.clone(),
        email: email.clone(),
        account_id: account_id.clone(),
        user_id: user_id.clone(),
        label: label.clone(),
        plan_type: plan_type.clone(),
        access_token: access_token.to_string(),
        refresh_token: refresh_token.clone(),
        access_token_expires_at: None,
        status,
        quota_limit_reached: false,
        quota_cooldown_until: None,
        cloudflare_cooldown_until: None,
        added_at: now.clone(),
        last_used_at: None,
    };
    let account = NewAccount {
        id: id.clone(),
        email: email.clone(),
        account_id: account_id.clone(),
        user_id: user_id.clone(),
        label: label.clone(),
        plan_type: plan_type.clone(),
        access_token: SecretString::new(access_token.to_string().into()),
        refresh_token: refresh_token.map(|token| SecretString::new(token.into())),
        access_token_expires_at: None,
        status,
    };
    if repo.insert(account).await.is_err() {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Failed to create account", (), request_id),
        )
        .into_response();
    }
    state.account_pool().lock().await.insert(pool_account);

    // 手动添加账号的响应只返回可展示元数据，OAuth token 永不回显。
    AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            AdminAccountData {
                id,
                email,
                account_id,
                user_id,
                label,
                plan_type,
                status: account_status_value(status).to_string(),
                access_token_expires_at: None,
                added_at: now.clone(),
                updated_at: now,
            },
            request_id,
        ),
    )
    .into_response()
}

pub async fn update_account_label(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
    Json(payload): Json<UpdateAccountLabelRequest>,
) -> Response {
    let request_id = request_id.as_str().to_string();
    let Some(pool) = state.db() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Database is not initialized", (), request_id),
        )
        .into_response();
    };
    if let Err(response) = require_admin_session(pool, &headers, &request_id).await {
        return response;
    }
    let Some(repo) = state.account_repository() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(
                50001,
                "Account repository is not initialized",
                (),
                request_id,
            ),
        )
        .into_response();
    };
    if payload
        .label
        .as_ref()
        .is_some_and(|label| label.chars().count() > 64)
    {
        return AdminResponse::new(
            StatusCode::BAD_REQUEST,
            AdminEnvelope::new(
                40001,
                "Account label must be 64 characters or fewer",
                (),
                request_id,
            ),
        )
        .into_response();
    }

    match repo.set_label(&account_id, payload.label.clone()).await {
        Ok(true) => {
            // 管理后台改名后要同步内存调度池，否则列表和实际调度状态会短暂不一致。
            state
                .account_pool()
                .lock()
                .await
                .set_label(&account_id, payload.label.clone());
            AdminResponse::new(
                StatusCode::OK,
                AdminEnvelope::ok(
                    UpdateAccountLabelData {
                        id: account_id,
                        label: payload.label,
                    },
                    request_id,
                ),
            )
            .into_response()
        }
        Ok(false) => AdminResponse::new(
            StatusCode::NOT_FOUND,
            AdminEnvelope::new(40401, "Account not found", (), request_id),
        )
        .into_response(),
        Err(_) => AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Failed to update account label", (), request_id),
        )
        .into_response(),
    }
}

pub async fn update_account_status(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
    Json(payload): Json<UpdateAccountStatusRequest>,
) -> Response {
    let request_id = request_id.as_str().to_string();
    let Some(pool) = state.db() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Database is not initialized", (), request_id),
        )
        .into_response();
    };
    if let Err(response) = require_admin_session(pool, &headers, &request_id).await {
        return response;
    }
    let status = match parse_admin_account_status(&payload.status) {
        Ok(status) => status,
        Err(message) => {
            return AdminResponse::new(
                StatusCode::BAD_REQUEST,
                AdminEnvelope::new(40001, message, (), request_id),
            )
            .into_response();
        }
    };
    let Some(repo) = state.account_repository() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(
                50001,
                "Account repository is not initialized",
                (),
                request_id,
            ),
        )
        .into_response();
    };

    match repo.set_status(&account_id, status).await {
        Ok(true) => {
            if sync_runtime_account_status(&state, &repo, &account_id, status)
                .await
                .is_err()
            {
                return AdminResponse::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    AdminEnvelope::new(50001, "Failed to sync account status", (), request_id),
                )
                .into_response();
            }
            AdminResponse::new(
                StatusCode::OK,
                AdminEnvelope::ok(
                    UpdateAccountStatusData {
                        id: account_id,
                        status: account_status_value(status).to_string(),
                    },
                    request_id,
                ),
            )
            .into_response()
        }
        Ok(false) => AdminResponse::new(
            StatusCode::NOT_FOUND,
            AdminEnvelope::new(40401, "Account not found", (), request_id),
        )
        .into_response(),
        Err(_) => AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Failed to update account status", (), request_id),
        )
        .into_response(),
    }
}

pub async fn delete_account(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
) -> Response {
    let request_id = request_id.as_str().to_string();
    let Some(pool) = state.db() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Database is not initialized", (), request_id),
        )
        .into_response();
    };
    if let Err(response) = require_admin_session(pool, &headers, &request_id).await {
        return response;
    }
    let Some(repo) = state.account_repository() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(
                50001,
                "Account repository is not initialized",
                (),
                request_id,
            ),
        )
        .into_response();
    };

    match repo.delete(&account_id).await {
        Ok(true) => {
            // DB 外键会级联清理账号关联数据；内存池仍需立即摘除，避免删除后继续被调度。
            state.account_pool().lock().await.remove(&account_id);
            AdminResponse::new(
                StatusCode::OK,
                AdminEnvelope::ok(DeleteAccountData { deleted: true }, request_id),
            )
            .into_response()
        }
        Ok(false) => AdminResponse::new(
            StatusCode::NOT_FOUND,
            AdminEnvelope::new(40401, "Account not found", (), request_id),
        )
        .into_response(),
        Err(_) => AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Failed to delete account", (), request_id),
        )
        .into_response(),
    }
}

pub async fn batch_delete_accounts(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<BatchDeleteAccountsRequest>,
) -> Response {
    let request_id = request_id.as_str().to_string();
    if payload.ids.is_empty() {
        return AdminResponse::new(
            StatusCode::BAD_REQUEST,
            AdminEnvelope::new(40001, "Account ids are required", (), request_id),
        )
        .into_response();
    }
    let Some(pool) = state.db() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Database is not initialized", (), request_id),
        )
        .into_response();
    };
    if let Err(response) = require_admin_session(pool, &headers, &request_id).await {
        return response;
    }
    let Some(repo) = state.account_repository() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(
                50001,
                "Account repository is not initialized",
                (),
                request_id,
            ),
        )
        .into_response();
    };

    let mut deleted = 0u32;
    let mut not_found = Vec::new();
    for account_id in payload.ids {
        match repo.delete(&account_id).await {
            Ok(true) => {
                state.account_pool().lock().await.remove(&account_id);
                deleted += 1;
            }
            Ok(false) => not_found.push(account_id),
            Err(_) => {
                return AdminResponse::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    AdminEnvelope::new(50001, "Failed to delete account", (), request_id),
                )
                .into_response();
            }
        }
    }

    AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(BatchDeleteAccountsData { deleted, not_found }, request_id),
    )
    .into_response()
}

pub async fn batch_update_account_status(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<BatchUpdateAccountStatusRequest>,
) -> Response {
    let request_id = request_id.as_str().to_string();
    if payload.ids.is_empty() {
        return AdminResponse::new(
            StatusCode::BAD_REQUEST,
            AdminEnvelope::new(40001, "Account ids are required", (), request_id),
        )
        .into_response();
    }
    let status = match parse_admin_account_status(&payload.status) {
        Ok(status) => status,
        Err(message) => {
            return AdminResponse::new(
                StatusCode::BAD_REQUEST,
                AdminEnvelope::new(40001, message, (), request_id),
            )
            .into_response();
        }
    };
    let Some(pool) = state.db() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Database is not initialized", (), request_id),
        )
        .into_response();
    };
    if let Err(response) = require_admin_session(pool, &headers, &request_id).await {
        return response;
    }
    let Some(repo) = state.account_repository() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(
                50001,
                "Account repository is not initialized",
                (),
                request_id,
            ),
        )
        .into_response();
    };

    let mut updated = 0u32;
    let mut not_found = Vec::new();
    for account_id in payload.ids {
        match repo.set_status(&account_id, status).await {
            Ok(true) => {
                if sync_runtime_account_status(&state, &repo, &account_id, status)
                    .await
                    .is_err()
                {
                    return AdminResponse::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        AdminEnvelope::new(50001, "Failed to sync account status", (), request_id),
                    )
                    .into_response();
                }
                updated += 1;
            }
            Ok(false) => not_found.push(account_id),
            Err(_) => {
                return AdminResponse::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    AdminEnvelope::new(50001, "Failed to update account status", (), request_id),
                )
                .into_response();
            }
        }
    }

    AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            BatchUpdateAccountStatusData { updated, not_found },
            request_id,
        ),
    )
    .into_response()
}

pub async fn get_account_cookies(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
) -> Response {
    let request_id = request_id.as_str().to_string();
    let Some(pool) = state.db() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Database is not initialized", (), request_id),
        )
        .into_response();
    };
    if let Err(response) = require_admin_session(pool, &headers, &request_id).await {
        return response;
    }
    let Some(account_repo) = state.account_repository() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(
                50001,
                "Account repository is not initialized",
                (),
                request_id,
            ),
        )
        .into_response();
    };
    match account_repo.exists(&account_id).await {
        Ok(true) => {}
        Ok(false) => {
            return AdminResponse::new(
                StatusCode::NOT_FOUND,
                AdminEnvelope::new(40401, "Account not found", (), request_id),
            )
            .into_response();
        }
        Err(_) => {
            return AdminResponse::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                AdminEnvelope::new(50001, "Failed to inspect account", (), request_id),
            )
            .into_response();
        }
    }
    let Some(cookie_repo) = state.cookie_repository() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(
                50001,
                "Cookie repository is not initialized",
                (),
                request_id,
            ),
        )
        .into_response();
    };

    match cookie_repo.cookie_header(&account_id, "chatgpt.com").await {
        Ok(cookies) => AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(AccountCookiesData { cookies }, request_id),
        )
        .into_response(),
        Err(_) => AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Failed to load account cookies", (), request_id),
        )
        .into_response(),
    }
}

pub async fn set_account_cookies(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
    Json(payload): Json<SetAccountCookiesRequest>,
) -> Response {
    let request_id = request_id.as_str().to_string();
    let cookie_header = match admin_cookie_header(&payload.cookies) {
        Ok(cookie_header) => cookie_header,
        Err(message) => {
            return AdminResponse::new(
                StatusCode::BAD_REQUEST,
                AdminEnvelope::new(40001, message, (), request_id),
            )
            .into_response();
        }
    };
    let Some(pool) = state.db() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Database is not initialized", (), request_id),
        )
        .into_response();
    };
    if let Err(response) = require_admin_session(pool, &headers, &request_id).await {
        return response;
    }
    let Some(account_repo) = state.account_repository() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(
                50001,
                "Account repository is not initialized",
                (),
                request_id,
            ),
        )
        .into_response();
    };
    match account_repo.exists(&account_id).await {
        Ok(true) => {}
        Ok(false) => {
            return AdminResponse::new(
                StatusCode::NOT_FOUND,
                AdminEnvelope::new(40401, "Account not found", (), request_id),
            )
            .into_response();
        }
        Err(_) => {
            return AdminResponse::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                AdminEnvelope::new(50001, "Failed to inspect account", (), request_id),
            )
            .into_response();
        }
    }
    let Some(cookie_repo) = state.cookie_repository() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(
                50001,
                "Cookie repository is not initialized",
                (),
                request_id,
            ),
        )
        .into_response();
    };

    // 管理端粘贴的是浏览器 Cookie header；保存时逐项加密，读取时只回放当前账号自己的 Cookie。
    match cookie_repo
        .set_cookie_header(&account_id, &cookie_header)
        .await
    {
        Ok(0) => AdminResponse::new(
            StatusCode::BAD_REQUEST,
            AdminEnvelope::new(40001, "No valid cookies found", (), request_id),
        )
        .into_response(),
        Ok(_) => match cookie_repo.cookie_header(&account_id, "chatgpt.com").await {
            Ok(cookies) => AdminResponse::new(
                StatusCode::OK,
                AdminEnvelope::ok(AccountCookiesData { cookies }, request_id),
            )
            .into_response(),
            Err(_) => AdminResponse::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                AdminEnvelope::new(50001, "Failed to load account cookies", (), request_id),
            )
            .into_response(),
        },
        Err(_) => AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Failed to store account cookies", (), request_id),
        )
        .into_response(),
    }
}

pub async fn delete_account_cookies(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
) -> Response {
    let request_id = request_id.as_str().to_string();
    let Some(pool) = state.db() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Database is not initialized", (), request_id),
        )
        .into_response();
    };
    if let Err(response) = require_admin_session(pool, &headers, &request_id).await {
        return response;
    }
    let Some(account_repo) = state.account_repository() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(
                50001,
                "Account repository is not initialized",
                (),
                request_id,
            ),
        )
        .into_response();
    };
    match account_repo.exists(&account_id).await {
        Ok(true) => {}
        Ok(false) => {
            return AdminResponse::new(
                StatusCode::NOT_FOUND,
                AdminEnvelope::new(40401, "Account not found", (), request_id),
            )
            .into_response();
        }
        Err(_) => {
            return AdminResponse::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                AdminEnvelope::new(50001, "Failed to inspect account", (), request_id),
            )
            .into_response();
        }
    }
    let Some(cookie_repo) = state.cookie_repository() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(
                50001,
                "Cookie repository is not initialized",
                (),
                request_id,
            ),
        )
        .into_response();
    };

    match cookie_repo.delete_account_cookies(&account_id).await {
        Ok(_) => AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(DeleteAccountCookiesData { deleted: true }, request_id),
        )
        .into_response(),
        Err(_) => AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Failed to delete account cookies", (), request_id),
        )
        .into_response(),
    }
}

pub async fn import_accounts(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Response {
    let request_id = request_id.as_str().to_string();
    let Some(pool) = state.db() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Database is not initialized", (), request_id),
        )
        .into_response();
    };
    if let Err(response) = require_admin_session(pool, &headers, &request_id).await {
        return response;
    }
    let Some(repo) = state.account_repository() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(
                50001,
                "Account repository is not initialized",
                (),
                request_id,
            ),
        )
        .into_response();
    };
    let parsed = parse_account_import_payload(&payload);
    if parsed.accounts.is_empty() {
        return AdminResponse::new(
            StatusCode::BAD_REQUEST,
            AdminEnvelope::new(40001, "No importable accounts found", (), request_id),
        )
        .into_response();
    }

    let mut imported = 0u32;
    let mut skipped = 0u32;
    for entry in parsed.accounts {
        let token = entry.token.as_deref().unwrap_or_default().trim();
        if token.is_empty() {
            skipped += 1;
            continue;
        }
        let id = normalized_account_id(entry.id);
        match repo.exists(&id).await {
            Ok(true) => {
                skipped += 1;
                continue;
            }
            Ok(false) => {}
            Err(_) => {
                return AdminResponse::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    AdminEnvelope::new(50001, "Failed to inspect account", (), request_id),
                )
                .into_response();
            }
        }

        let status = match parse_import_status(entry.status.as_deref()) {
            Ok(status) => status,
            Err(message) => {
                return AdminResponse::new(
                    StatusCode::BAD_REQUEST,
                    AdminEnvelope::new(40001, message, (), request_id),
                )
                .into_response();
            }
        };
        let email = empty_to_none(entry.email);
        let account_id = empty_to_none(entry.account_id);
        let user_id = empty_to_none(entry.user_id);
        let label = empty_to_none(entry.label);
        let plan_type = empty_to_none(entry.plan_type);
        let refresh_token = empty_to_none(entry.refresh_token);
        let access_token = token.to_string();
        let pool_account = Account {
            id: id.clone(),
            email: email.clone(),
            account_id: account_id.clone(),
            user_id: user_id.clone(),
            label: label.clone(),
            plan_type: plan_type.clone(),
            access_token: access_token.clone(),
            refresh_token: refresh_token.clone(),
            access_token_expires_at: None,
            status,
            quota_limit_reached: false,
            quota_cooldown_until: None,
            cloudflare_cooldown_until: None,
            added_at: chrono::Utc::now().to_rfc3339(),
            last_used_at: None,
        };
        let account = NewAccount {
            id,
            email,
            account_id,
            user_id,
            label,
            plan_type,
            access_token: SecretString::new(access_token.into()),
            refresh_token: refresh_token.map(|token| SecretString::new(token.into())),
            access_token_expires_at: None,
            status,
        };
        if repo.insert(account).await.is_err() {
            return AdminResponse::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                AdminEnvelope::new(50001, "Failed to import account", (), request_id),
            )
            .into_response();
        }
        state.account_pool().lock().await.insert(pool_account);
        imported += 1;
    }

    AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            AccountImportData {
                imported,
                skipped,
                source_format: parsed.source_format.as_str().to_string(),
            },
            request_id,
        ),
    )
    .into_response()
}

pub async fn api_keys(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<ApiKeysQuery>,
) -> Response {
    let request_id = request_id.as_str().to_string();
    let Some(pool) = state.db() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Database is not initialized", (), request_id),
        )
        .into_response();
    };
    if let Err(response) = require_admin_session(pool, &headers, &request_id).await {
        return response;
    }
    let Some(repo) = state.client_api_key_repository() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(
                50001,
                "API key repository is not initialized",
                (),
                request_id,
            ),
        )
        .into_response();
    };

    let limit = clamp_limit(query.limit.unwrap_or(50));
    match repo.list(query.cursor, limit).await {
        Ok(page) => {
            let Page { items, next_cursor } = page;
            let page = Page {
                items: items.into_iter().map(ClientApiKeyData::from).collect(),
                next_cursor,
            };
            AdminResponse::new(
                StatusCode::OK,
                AdminPageEnvelope::ok(page, limit, request_id),
            )
            .into_response()
        }
        Err(_) => AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Failed to list API keys", (), request_id),
        )
        .into_response(),
    }
}

pub async fn create_api_key(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<CreateApiKeyRequest>,
) -> Response {
    let request_id = request_id.as_str().to_string();
    let Some(pool) = state.db() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Database is not initialized", (), request_id),
        )
        .into_response();
    };
    if let Err(response) = require_admin_session(pool, &headers, &request_id).await {
        return response;
    }
    let name = payload.name.trim();
    if name.is_empty() {
        return AdminResponse::new(
            StatusCode::BAD_REQUEST,
            AdminEnvelope::new(40001, "API key name is required", (), request_id),
        )
        .into_response();
    }
    let Some(repo) = state.client_api_key_repository() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(
                50001,
                "API key repository is not initialized",
                (),
                request_id,
            ),
        )
        .into_response();
    };
    let Some(hasher) = state.api_key_hasher() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "API key hasher is not initialized", (), request_id),
        )
        .into_response();
    };

    let generated = hasher.generate_client_api_key(name);
    let plaintext = generated.plaintext.clone();
    match repo.insert_generated(name, &generated).await {
        Ok(key) => AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(CreatedClientApiKeyData::new(key, plaintext), request_id),
        )
        .into_response(),
        Err(_) => AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Failed to create API key", (), request_id),
        )
        .into_response(),
    }
}

pub async fn batch_delete_api_keys(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<BatchDeleteClientApiKeysRequest>,
) -> Response {
    let request_id = request_id.as_str().to_string();
    if payload.ids.is_empty() {
        return AdminResponse::new(
            StatusCode::BAD_REQUEST,
            AdminEnvelope::new(40001, "API key ids are required", (), request_id),
        )
        .into_response();
    }
    let Some(pool) = state.db() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Database is not initialized", (), request_id),
        )
        .into_response();
    };
    if let Err(response) = require_admin_session(pool, &headers, &request_id).await {
        return response;
    }
    let Some(repo) = state.client_api_key_repository() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(
                50001,
                "API key repository is not initialized",
                (),
                request_id,
            ),
        )
        .into_response();
    };

    let mut deleted = 0u32;
    let mut not_found = Vec::new();
    for key_id in payload.ids {
        match repo.delete(&key_id).await {
            Ok(true) => deleted += 1,
            Ok(false) => not_found.push(key_id),
            Err(_) => {
                return AdminResponse::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    AdminEnvelope::new(50001, "Failed to delete API key", (), request_id),
                )
                .into_response();
            }
        }
    }

    AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            BatchDeleteClientApiKeysData { deleted, not_found },
            request_id,
        ),
    )
    .into_response()
}

pub async fn update_api_key_label(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(key_id): Path<String>,
    Json(payload): Json<UpdateClientApiKeyLabelRequest>,
) -> Response {
    let request_id = request_id.as_str().to_string();
    if payload
        .label
        .as_ref()
        .is_some_and(|label| label.chars().count() > 64)
    {
        return AdminResponse::new(
            StatusCode::BAD_REQUEST,
            AdminEnvelope::new(
                40001,
                "API key label must be 64 characters or fewer",
                (),
                request_id,
            ),
        )
        .into_response();
    }
    let Some(pool) = state.db() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Database is not initialized", (), request_id),
        )
        .into_response();
    };
    if let Err(response) = require_admin_session(pool, &headers, &request_id).await {
        return response;
    }
    let Some(repo) = state.client_api_key_repository() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(
                50001,
                "API key repository is not initialized",
                (),
                request_id,
            ),
        )
        .into_response();
    };

    // label 是后台显示备注，不能影响 cpr_ key 的 hash、prefix 或启用状态。
    match repo.set_label(&key_id, payload.label).await {
        Ok(Some(key)) => AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(ClientApiKeyData::from(key), request_id),
        )
        .into_response(),
        Ok(None) => AdminResponse::new(
            StatusCode::NOT_FOUND,
            AdminEnvelope::new(40401, "API key not found", (), request_id),
        )
        .into_response(),
        Err(_) => AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Failed to update API key label", (), request_id),
        )
        .into_response(),
    }
}

pub async fn update_api_key_status(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(key_id): Path<String>,
    Json(payload): Json<UpdateClientApiKeyStatusRequest>,
) -> Response {
    let request_id = request_id.as_str().to_string();
    let Some(pool) = state.db() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Database is not initialized", (), request_id),
        )
        .into_response();
    };
    if let Err(response) = require_admin_session(pool, &headers, &request_id).await {
        return response;
    }
    let enabled = match parse_client_api_key_enabled_status(&payload.status) {
        Ok(enabled) => enabled,
        Err(message) => {
            return AdminResponse::new(
                StatusCode::BAD_REQUEST,
                AdminEnvelope::new(40001, message, (), request_id),
            )
            .into_response();
        }
    };
    let Some(repo) = state.client_api_key_repository() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(
                50001,
                "API key repository is not initialized",
                (),
                request_id,
            ),
        )
        .into_response();
    };

    match repo.set_enabled(&key_id, enabled).await {
        Ok(true) => AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(
                UpdateClientApiKeyStatusData {
                    id: key_id,
                    enabled,
                },
                request_id,
            ),
        )
        .into_response(),
        Ok(false) => AdminResponse::new(
            StatusCode::NOT_FOUND,
            AdminEnvelope::new(40401, "API key not found", (), request_id),
        )
        .into_response(),
        Err(_) => AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Failed to update API key status", (), request_id),
        )
        .into_response(),
    }
}

pub async fn delete_api_key(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(key_id): Path<String>,
) -> Response {
    let request_id = request_id.as_str().to_string();
    let Some(pool) = state.db() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Database is not initialized", (), request_id),
        )
        .into_response();
    };
    if let Err(response) = require_admin_session(pool, &headers, &request_id).await {
        return response;
    }
    let Some(repo) = state.client_api_key_repository() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(
                50001,
                "API key repository is not initialized",
                (),
                request_id,
            ),
        )
        .into_response();
    };

    match repo.delete(&key_id).await {
        Ok(true) => AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(DeleteClientApiKeyData { deleted: true }, request_id),
        )
        .into_response(),
        Ok(false) => AdminResponse::new(
            StatusCode::NOT_FOUND,
            AdminEnvelope::new(40401, "API key not found", (), request_id),
        )
        .into_response(),
        Err(_) => AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Failed to delete API key", (), request_id),
        )
        .into_response(),
    }
}

#[derive(Debug)]
struct AdminUserRow {
    id: String,
    password_hash: String,
}

async fn load_first_admin(pool: &sqlx::SqlitePool) -> Result<Option<AdminUserRow>, sqlx::Error> {
    let row =
        sqlx::query("select id, password_hash from admin_users order by created_at asc limit 1")
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|row| AdminUserRow {
        id: row.get("id"),
        password_hash: row.get("password_hash"),
    }))
}

async fn create_admin_session(
    pool: &sqlx::SqlitePool,
    session_id: &str,
    user_id: &str,
    expires_at: chrono::DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "insert into admin_sessions (id, user_id, expires_at, created_at) values (?, ?, ?, ?)",
    )
    .bind(session_id)
    .bind(user_id)
    .bind(expires_at.to_rfc3339())
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

fn admin_session_set_cookie(session_id: &str, ttl_minutes: u64) -> Option<HeaderValue> {
    let max_age = ttl_minutes.checked_mul(60)?;
    let cookie = format!(
        "cpr_admin_session={session_id}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}"
    );
    HeaderValue::from_str(&cookie).ok()
}

async fn validate_admin_session(
    pool: &sqlx::SqlitePool,
    headers: &HeaderMap,
) -> Result<bool, sqlx::Error> {
    let Some(session_id) = admin_session_cookie(headers) else {
        return Ok(false);
    };
    let now = Utc::now().to_rfc3339();
    let count: (i64,) =
        sqlx::query_as("select count(*) from admin_sessions where id = ? and expires_at > ?")
            .bind(session_id)
            .bind(now)
            .fetch_one(pool)
            .await?;
    Ok(count.0 > 0)
}

fn admin_session_cookie(headers: &HeaderMap) -> Option<&str> {
    admin_session_id(headers)
}

async fn require_admin_session(
    pool: &sqlx::SqlitePool,
    headers: &HeaderMap,
    request_id: &str,
) -> Result<(), Response> {
    match validate_admin_session(pool, headers).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(AdminResponse::new(
            StatusCode::UNAUTHORIZED,
            AdminEnvelope::new(40101, "Admin session required", (), request_id),
        )
        .into_response()),
        Err(_) => Err(AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Failed to validate admin session", (), request_id),
        )
        .into_response()),
    }
}

async fn sync_runtime_account_status(
    state: &AppState,
    repo: &crate::accounts::repository::AccountRepository,
    account_id: &str,
    status: AccountStatus,
) -> Result<(), AccountRepositoryError> {
    let updated = state
        .account_pool()
        .lock()
        .await
        .set_status(account_id, status);
    if updated || status != AccountStatus::Active {
        return Ok(());
    }

    if let Some(account) = repo.get(account_id).await? {
        state
            .account_pool()
            .lock()
            .await
            .insert(pool_account_from_stored(account));
    }
    Ok(())
}

fn pool_account_from_stored(account: StoredAccount) -> Account {
    Account {
        id: account.id,
        email: account.email,
        account_id: account.account_id,
        user_id: account.user_id,
        label: account.label,
        plan_type: account.plan_type,
        access_token: account.access_token.expose_secret().to_string(),
        refresh_token: account
            .refresh_token
            .as_ref()
            .map(|token| token.expose_secret().to_string()),
        access_token_expires_at: account.access_token_expires_at,
        status: account.status,
        quota_limit_reached: false,
        quota_cooldown_until: None,
        cloudflare_cooldown_until: None,
        added_at: account.added_at.to_rfc3339(),
        last_used_at: None,
    }
}

fn normalized_account_id(id: Option<String>) -> String {
    id.and_then(|id| empty_to_none(Some(id)))
        .unwrap_or_else(|| format!("acct_{}", Uuid::new_v4().simple()))
}

fn account_export_ids(value: Option<&str>) -> Vec<String> {
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

fn distinct_active_plan_accounts(accounts: Vec<Account>) -> Vec<(String, Account)> {
    let mut by_plan = std::collections::BTreeMap::new();
    for account in accounts {
        if account.status != AccountStatus::Active {
            continue;
        }
        let plan_type = account
            .plan_type
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        // 同一 plan 的模型列表一致，刷新时只需要取一个账号，避免重复打上游。
        by_plan.entry(plan_type).or_insert(account);
    }
    by_plan.into_iter().collect()
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

fn first_string(value: &Value, paths: &[&[&str]]) -> Option<String> {
    paths.iter().find_map(|path| string_at(value, path))
}

fn string_at(value: &Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
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

fn parse_import_status(status: Option<&str>) -> Result<AccountStatus, String> {
    let normalized = status.unwrap_or("active").trim().to_ascii_lowercase();
    match normalized.as_str() {
        "active" => Ok(AccountStatus::Active),
        "expired" => Ok(AccountStatus::Expired),
        "quota_exhausted" => Ok(AccountStatus::QuotaExhausted),
        "refreshing" => Ok(AccountStatus::Refreshing),
        "disabled" => Ok(AccountStatus::Disabled),
        "banned" => Ok(AccountStatus::Banned),
        other => Err(format!("Unsupported account status: {other}")),
    }
}

fn parse_admin_account_status(status: &str) -> Result<AccountStatus, String> {
    match status.trim().to_ascii_lowercase().as_str() {
        "active" => Ok(AccountStatus::Active),
        "disabled" => Ok(AccountStatus::Disabled),
        other => Err(format!("Unsupported account status: {other}")),
    }
}

fn parse_client_api_key_enabled_status(status: &str) -> Result<bool, String> {
    match status.trim().to_ascii_lowercase().as_str() {
        "active" => Ok(true),
        "disabled" => Ok(false),
        other => Err(format!("Unsupported API key status: {other}")),
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

fn account_status_value(status: AccountStatus) -> &'static str {
    match status {
        AccountStatus::Active => "active",
        AccountStatus::Expired => "expired",
        AccountStatus::QuotaExhausted => "quota_exhausted",
        AccountStatus::Refreshing => "refreshing",
        AccountStatus::Disabled => "disabled",
        AccountStatus::Banned => "banned",
    }
}
