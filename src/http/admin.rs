use std::time::{Duration as StdDuration, Instant};

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{
        header::{HeaderValue, SET_COOKIE},
        HeaderMap, StatusCode,
    },
    response::{IntoResponse, Response},
    Extension, Json,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use futures::{stream, StreamExt};
use reqwest::Url;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sqlx::Row;
use tokio::time::sleep;
use uuid::Uuid;

use crate::{
    accounts::{
        model::{Account, AccountStatus},
        repository::{
            AccountClaimsUpdate, AccountRepositoryError, AccountUsageListRecord,
            AccountUsageSummary, NewAccount, StoredAccount, StoredAccountMetadata, TokenUpdate,
        },
    },
    auth::api_key_repository::StoredClientApiKey,
    auth::{
        admin_session::verify_admin_password,
        cli_import::{default_codex_home, read_cli_auth_from_home},
        refresh::RefreshFailure,
        token::TokenPair,
    },
    codex::client::{
        build_reqwest_client, CodexBackendClient, CodexClientError, CodexRequestContext,
    },
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
pub struct ApiKeyExportQuery {
    pub ids: Option<String>,
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAuthStatusData {
    pub authenticated: bool,
    pub user: Option<AdminAuthUserData>,
    pub pool: AdminAuthPoolSummaryData,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAuthUserData {
    pub id: String,
    pub email: Option<String>,
    pub account_id: Option<String>,
    pub user_id: Option<String>,
    pub label: Option<String>,
    pub plan_type: Option<String>,
    pub status: String,
    pub access_token_expires_at: Option<String>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAuthPoolSummaryData {
    pub total: usize,
    pub active: usize,
    pub expired: usize,
    pub quota_exhausted: usize,
    pub refreshing: usize,
    pub disabled: usize,
    pub banned: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAuthLogoutData {
    pub success: bool,
    pub deleted: u64,
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

#[derive(Debug, Clone)]
struct ClientApiKeyImportEntry {
    pub source_id: Option<String>,
    pub source_prefix: Option<String>,
    pub name: String,
    pub label: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientApiKeyExportData {
    pub source_format: &'static str,
    pub rotation_required: bool,
    pub api_keys: Vec<ClientApiKeyExportEntry>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientApiKeyExportEntry {
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
pub struct ClientApiKeyImportData {
    pub imported: u32,
    pub skipped: u32,
    pub rotated: bool,
    pub keys: Vec<ImportedClientApiKeyData>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedClientApiKeyData {
    pub source_id: Option<String>,
    pub source_prefix: Option<String>,
    pub id: String,
    pub name: String,
    pub label: Option<String>,
    pub prefix: String,
    pub enabled: bool,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub plaintext: String,
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

impl From<StoredClientApiKey> for ClientApiKeyExportEntry {
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

impl ImportedClientApiKeyData {
    fn new(
        key: StoredClientApiKey,
        plaintext: String,
        source_id: Option<String>,
        source_prefix: Option<String>,
    ) -> Self {
        Self {
            source_id,
            source_prefix,
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

fn account_auth_user(account: &StoredAccountMetadata) -> AdminAuthUserData {
    AdminAuthUserData {
        id: account.id.clone(),
        email: account.email.clone(),
        account_id: account.account_id.clone(),
        user_id: account.user_id.clone(),
        label: account.label.clone(),
        plan_type: account.plan_type.clone(),
        status: account_status_value(account.status).to_string(),
        access_token_expires_at: account
            .access_token_expires_at
            .map(|value| value.to_rfc3339()),
    }
}

fn account_auth_pool_summary(accounts: &[StoredAccountMetadata]) -> AdminAuthPoolSummaryData {
    let mut summary = AdminAuthPoolSummaryData {
        total: accounts.len(),
        ..AdminAuthPoolSummaryData::default()
    };
    for account in accounts {
        match account.status {
            AccountStatus::Active => summary.active += 1,
            AccountStatus::Expired => summary.expired += 1,
            AccountStatus::QuotaExhausted => summary.quota_exhausted += 1,
            AccountStatus::Refreshing => summary.refreshing += 1,
            AccountStatus::Disabled => summary.disabled += 1,
            AccountStatus::Banned => summary.banned += 1,
        }
    }
    summary
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

pub async fn auth_status(
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

    match repo.list_all_metadata().await {
        Ok(accounts) => {
            let summary = account_auth_pool_summary(&accounts);
            let user = accounts.first().map(account_auth_user);
            AdminResponse::new(
                StatusCode::OK,
                AdminEnvelope::ok(
                    AdminAuthStatusData {
                        authenticated: summary.total > 0,
                        user,
                        pool: summary,
                    },
                    request_id,
                ),
            )
            .into_response()
        }
        Err(_) => AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(
                50001,
                "Failed to inspect account auth status",
                (),
                request_id,
            ),
        )
        .into_response(),
    }
}

pub async fn auth_logout(
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

    match repo.delete_all().await {
        Ok(deleted) => {
            // 账号 logout 要同时清掉调度池；SQLite 外键会级联 usage/cookies 等账号附属数据。
            state.account_pool().lock().await.clear();
            AdminResponse::new(
                StatusCode::OK,
                AdminEnvelope::ok(
                    AdminAuthLogoutData {
                        success: true,
                        deleted,
                    },
                    request_id,
                ),
            )
            .into_response()
        }
        Err(_) => AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Failed to clear accounts", (), request_id),
        )
        .into_response(),
    }
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

    let stored =
        match store_validated_account_import(&state, &repo, payload.token, payload.refresh_token)
            .await
        {
            Ok(stored) => stored,
            Err(error) => return validated_account_import_error_response(error, request_id),
        };

    // 手动添加账号的响应只返回可展示元数据，OAuth token 永不回显。
    AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(admin_account_data_from_stored(stored), request_id),
    )
    .into_response()
}

pub async fn health_check_accounts(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    payload: Option<Json<HealthCheckRequest>>,
) -> Response {
    let request_id = request_id.as_str().to_string();
    let payload = payload.map(|Json(payload)| payload).unwrap_or_default();
    if payload.ids.as_ref().is_some_and(Vec::is_empty) {
        return AdminResponse::new(
            StatusCode::BAD_REQUEST,
            AdminEnvelope::new(40001, "Account ids must not be empty", (), request_id),
        )
        .into_response();
    }
    if payload
        .stagger_ms
        .is_some_and(|value| !(500..=30_000).contains(&value))
    {
        return AdminResponse::new(
            StatusCode::BAD_REQUEST,
            AdminEnvelope::new(
                40001,
                "staggerMs must be between 500 and 30000",
                (),
                request_id,
            ),
        )
        .into_response();
    }
    if payload
        .concurrency
        .is_some_and(|value| !(1..=10).contains(&value))
    {
        return AdminResponse::new(
            StatusCode::BAD_REQUEST,
            AdminEnvelope::new(
                40001,
                "concurrency must be between 1 and 10",
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

    let accounts = match repo.list_all().await {
        Ok(accounts) => filter_health_check_accounts(accounts, payload.ids.as_deref()),
        Err(_) => {
            return AdminResponse::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                AdminEnvelope::new(50001, "Failed to list accounts", (), request_id),
            )
            .into_response();
        }
    };
    let concurrency = usize::from(payload.concurrency.unwrap_or(2));
    let stagger_ms = payload.stagger_ms.unwrap_or(3_000);
    let results = stream::iter(accounts.into_iter().enumerate())
        .map(|(index, account)| {
            let state = state.clone();
            let repo = repo.clone();
            let request_id = request_id.clone();
            async move {
                if stagger_ms > 0 && index > 0 {
                    let multiplier = index.min(concurrency);
                    sleep(StdDuration::from_millis(
                        stagger_ms.saturating_mul(multiplier as u64),
                    ))
                    .await;
                }
                probe_account_with_codex_backend(&state, &repo, account, &request_id).await
            }
        })
        .buffer_unordered(concurrency)
        .collect::<Vec<_>>()
        .await;
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

    AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(HealthCheckData { summary, results }, request_id),
    )
    .into_response()
}

pub async fn refresh_account(
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
    let account = match repo.get(&account_id).await {
        Ok(Some(account)) => account,
        Ok(None) => {
            return AdminResponse::new(
                StatusCode::NOT_FOUND,
                AdminEnvelope::new(40401, "Account not found", (), request_id),
            )
            .into_response();
        }
        Err(_) => {
            return AdminResponse::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                AdminEnvelope::new(50001, "Failed to load account", (), request_id),
            )
            .into_response();
        }
    };
    if account.status == AccountStatus::Disabled {
        return AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(
                skipped_probe_result(&account, "manually disabled"),
                request_id,
            ),
        )
        .into_response();
    }
    let Some(refresh_token) = account.refresh_token.as_ref() else {
        return AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(
                skipped_probe_result(&account, "no refresh token"),
                request_id,
            ),
        )
        .into_response();
    };
    let Some(refresher) = state.token_refresher() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Token refresher is not initialized", (), request_id),
        )
        .into_response();
    };

    let started_at = Instant::now();
    let previous_status = account_status_value(account.status).to_string();
    match refresher.refresh(refresh_token.expose_secret()).await {
        Ok(tokens) => {
            match persist_admin_refreshed_account(&state, &repo, &account.id, tokens).await {
                Ok(updated) => AdminResponse::new(
                    StatusCode::OK,
                    AdminEnvelope::ok(
                        AccountProbeData {
                            id: updated.id,
                            email: updated.email,
                            previous_status,
                            result: "alive".to_string(),
                            status: Some(account_status_value(updated.status).to_string()),
                            error: None,
                            duration_ms: Some(started_at.elapsed().as_millis()),
                        },
                        request_id,
                    ),
                )
                .into_response(),
                Err(_) => AdminResponse::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    AdminEnvelope::new(50001, "Failed to store refreshed account", (), request_id),
                )
                .into_response(),
            }
        }
        Err(failure) => {
            let status = apply_refresh_failure_status(&state, &repo, &account, failure).await;
            AdminResponse::new(
                StatusCode::OK,
                AdminEnvelope::ok(
                    AccountProbeData {
                        id: account.id,
                        email: account.email,
                        previous_status,
                        result: "dead".to_string(),
                        status: status.map(account_status_value).map(ToString::to_string),
                        error: Some(public_refresh_failure(failure).to_string()),
                        duration_ms: Some(started_at.elapsed().as_millis()),
                    },
                    request_id,
                ),
            )
            .into_response()
        }
    }
}

pub async fn reset_account_usage(
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
    let Some(usage_repo) = state.account_usage_repository() else {
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
    if usage_repo.reset_account(&account_id).await.is_err() {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Failed to reset account usage", (), request_id),
        )
        .into_response();
    }
    state.account_pool().lock().await.reset_usage(&account_id);

    AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            ResetAccountUsageData {
                id: account_id,
                reset: true,
            },
            request_id,
        ),
    )
    .into_response()
}

pub async fn account_quota(
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
    let account = match repo.get(&account_id).await {
        Ok(Some(account)) => account,
        Ok(None) => {
            return AdminResponse::new(
                StatusCode::NOT_FOUND,
                AdminEnvelope::new(40401, "Account not found", (), request_id),
            )
            .into_response();
        }
        Err(_) => {
            return AdminResponse::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                AdminEnvelope::new(50001, "Failed to load account", (), request_id),
            )
            .into_response();
        }
    };
    if account.status != AccountStatus::Active {
        return AdminResponse::new(
            StatusCode::CONFLICT,
            AdminEnvelope::new(
                40901,
                format!(
                    "Account is {}, cannot query quota",
                    account_status_value(account.status)
                ),
                (),
                request_id,
            ),
        )
        .into_response();
    }

    match fetch_account_usage(&state, &account, &request_id).await {
        Ok(raw) => {
            let quota = quota_from_usage(&raw);
            if repo
                .update_quota_json(&account.id, &quota.to_string())
                .await
                .is_err()
            {
                return AdminResponse::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    AdminEnvelope::new(50001, "Failed to store account quota", (), request_id),
                )
                .into_response();
            }
            AdminResponse::new(
                StatusCode::OK,
                AdminEnvelope::ok(AccountQuotaData { quota, raw }, request_id),
            )
            .into_response()
        }
        Err(error) => {
            apply_codex_account_error(&state, &repo, &account, &error).await;
            AdminResponse::new(
                StatusCode::BAD_GATEWAY,
                AdminEnvelope::new(
                    50201,
                    "Failed to fetch quota from Codex API",
                    json!({ "error": public_codex_error(&error) }),
                    request_id,
                ),
            )
            .into_response()
        }
    }
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
        match store_import_account_entry(&state, &repo, entry).await {
            Ok(StoredImportAccount::Imported) => {
                imported += 1;
            }
            Ok(StoredImportAccount::Skipped) => {
                skipped += 1;
            }
            Err(StoreImportAccountError::Inspect) => {
                return AdminResponse::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    AdminEnvelope::new(50001, "Failed to inspect account", (), request_id),
                )
                .into_response();
            }
            Err(StoreImportAccountError::Invalid(message)) => {
                return AdminResponse::new(
                    StatusCode::BAD_REQUEST,
                    AdminEnvelope::new(40001, message, (), request_id),
                )
                .into_response();
            }
            Err(StoreImportAccountError::Insert) => {
                return AdminResponse::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    AdminEnvelope::new(50001, "Failed to import account", (), request_id),
                )
                .into_response();
            }
        }
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

pub async fn import_cli_auth(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Bytes,
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
    let payload = if body.is_empty() {
        ImportCliAuthRequest::default()
    } else {
        match serde_json::from_slice::<ImportCliAuthRequest>(&body) {
            Ok(payload) => payload,
            Err(_) => {
                return AdminResponse::new(
                    StatusCode::BAD_REQUEST,
                    AdminEnvelope::new(40001, "Invalid CLI import request", (), request_id),
                )
                .into_response();
            }
        }
    };
    let codex_home = match empty_to_none(payload.codex_home) {
        Some(path) => std::path::PathBuf::from(path),
        None => match default_codex_home() {
            Ok(path) => path,
            Err(error) => {
                return AdminResponse::new(
                    StatusCode::BAD_REQUEST,
                    AdminEnvelope::new(40001, error.to_string(), (), request_id),
                )
                .into_response();
            }
        },
    };
    let cli_auth = match read_cli_auth_from_home(&codex_home) {
        Ok(auth) => auth,
        Err(error) => {
            return AdminResponse::new(
                StatusCode::BAD_REQUEST,
                AdminEnvelope::new(40001, error.to_string(), (), request_id),
            )
            .into_response();
        }
    };
    let _stored = match store_validated_account_import(
        &state,
        &repo,
        Some(cli_auth.access_token().to_string()),
        cli_auth.refresh_token().map(str::to_string),
    )
    .await
    {
        Ok(stored) => stored,
        Err(error) => return validated_account_import_error_response(error, request_id),
    };

    AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            AccountImportData {
                imported: 1,
                skipped: 0,
                source_format: AccountImportFormat::CodexCli.as_str().to_string(),
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

pub async fn export_api_keys(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<ApiKeyExportQuery>,
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

    let ids = account_export_ids(query.ids.as_deref());
    let keys = if ids.is_empty() {
        match repo.list_all().await {
            Ok(keys) => keys,
            Err(_) => {
                return AdminResponse::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    AdminEnvelope::new(50001, "Failed to export API keys", (), request_id),
                )
                .into_response();
            }
        }
    } else {
        let mut keys = Vec::with_capacity(ids.len());
        for id in ids {
            match repo.get(&id).await {
                Ok(Some(key)) => keys.push(key),
                Ok(None) => {}
                Err(_) => {
                    return AdminResponse::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        AdminEnvelope::new(50001, "Failed to export API keys", (), request_id),
                    )
                    .into_response();
                }
            }
        }
        keys
    };

    // 安全边界：本地 cpr_ key 只导出可展示元数据，绝不导出 plaintext、key_hash 或 pepper。
    let data = ClientApiKeyExportData {
        source_format: "rustLocalClientApiKeys",
        rotation_required: true,
        api_keys: keys
            .into_iter()
            .map(ClientApiKeyExportEntry::from)
            .collect(),
    };
    AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data, request_id)).into_response()
}

pub async fn import_api_keys(
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

    let entries = parse_client_api_key_import_payload(&payload);
    if entries.is_empty() {
        return AdminResponse::new(
            StatusCode::BAD_REQUEST,
            AdminEnvelope::new(40001, "No importable API keys found", (), request_id),
        )
        .into_response();
    }

    let mut imported = 0u32;
    let mut skipped = 0u32;
    let mut keys = Vec::with_capacity(entries.len());
    for entry in entries {
        let name = entry.name.trim();
        if name.is_empty() {
            skipped += 1;
            continue;
        }
        if entry
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

        let generated = hasher.generate_client_api_key(name);
        let plaintext = generated.plaintext.clone();
        let source_id = entry.source_id;
        let source_prefix = entry.source_prefix;
        match repo
            .insert_generated_with_metadata(name, entry.label.as_deref(), entry.enabled, &generated)
            .await
        {
            Ok(key) => {
                imported += 1;
                keys.push(ImportedClientApiKeyData::new(
                    key,
                    plaintext,
                    source_id,
                    source_prefix,
                ));
            }
            Err(_) => {
                return AdminResponse::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    AdminEnvelope::new(50001, "Failed to import API key", (), request_id),
                )
                .into_response();
            }
        }
    }

    AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            ClientApiKeyImportData {
                imported,
                skipped,
                rotated: true,
                keys,
            },
            request_id,
        ),
    )
    .into_response()
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

fn filter_health_check_accounts(
    accounts: Vec<StoredAccount>,
    ids: Option<&[String]>,
) -> Vec<StoredAccount> {
    let Some(ids) = ids else {
        return accounts;
    };
    accounts
        .into_iter()
        .filter(|account| ids.iter().any(|id| id == &account.id))
        .collect()
}

fn skipped_probe_result(account: &StoredAccount, error: &str) -> AccountProbeData {
    AccountProbeData {
        id: account.id.clone(),
        email: account.email.clone(),
        previous_status: account_status_value(account.status).to_string(),
        result: "skipped".to_string(),
        status: Some(account_status_value(account.status).to_string()),
        error: Some(error.to_string()),
        duration_ms: None,
    }
}

async fn probe_account_with_codex_backend(
    state: &AppState,
    repo: &crate::accounts::repository::AccountRepository,
    account: StoredAccount,
    request_id: &str,
) -> AccountProbeData {
    if account.status == AccountStatus::Disabled {
        return skipped_probe_result(&account, "manually disabled");
    }

    let started_at = Instant::now();
    let previous_status = account_status_value(account.status).to_string();
    match fetch_account_usage(state, &account, request_id).await {
        Ok(raw) => {
            let quota = quota_from_usage(&raw);
            let _ = repo
                .update_quota_json(&account.id, &quota.to_string())
                .await;
            if account.status != AccountStatus::Active {
                let _ = repo.set_status(&account.id, AccountStatus::Active).await;
                state
                    .account_pool()
                    .lock()
                    .await
                    .set_status(&account.id, AccountStatus::Active);
            }
            AccountProbeData {
                id: account.id,
                email: account.email,
                previous_status,
                result: "alive".to_string(),
                status: Some(account_status_value(AccountStatus::Active).to_string()),
                error: None,
                duration_ms: Some(started_at.elapsed().as_millis()),
            }
        }
        Err(error) => {
            let status = apply_codex_account_error(state, repo, &account, &error).await;
            AccountProbeData {
                id: account.id,
                email: account.email,
                previous_status,
                result: "dead".to_string(),
                status: status.map(account_status_value).map(ToString::to_string),
                error: Some(public_codex_error(&error)),
                duration_ms: Some(started_at.elapsed().as_millis()),
            }
        }
    }
}

async fn fetch_account_usage(
    state: &AppState,
    account: &StoredAccount,
    request_id: &str,
) -> Result<Value, CodexClientError> {
    let cookie_header = account_cookie_header(state, &account.id).await;
    let client = CodexBackendClient::new(
        build_reqwest_client(state.config().tls.force_http11)?,
        state.config().api.base_url.clone(),
        Fingerprint::default_codex_desktop(),
    );
    client
        .fetch_usage(CodexRequestContext {
            access_token: account.access_token.expose_secret(),
            account_id: account.account_id.as_deref(),
            request_id,
            turn_state: None,
            turn_metadata: None,
            beta_features: None,
            include_timing_metrics: None,
            version: None,
            codex_window_id: None,
            parent_thread_id: None,
            cookie_header: cookie_header.as_deref(),
        })
        .await
}

async fn account_cookie_header(state: &AppState, account_id: &str) -> Option<String> {
    let domain = request_domain(&state.config().api.base_url)?;
    state
        .cookie_repository()?
        .cookie_header(account_id, &domain)
        .await
        .ok()
        .flatten()
}

async fn persist_admin_refreshed_account(
    state: &AppState,
    repo: &crate::accounts::repository::AccountRepository,
    account_id: &str,
    tokens: TokenPair,
) -> Result<Account, AccountRepositoryError> {
    let access_token = tokens.access_token;
    let refresh_token = tokens.refresh_token;
    repo.update_tokens(
        account_id,
        TokenUpdate {
            access_token: SecretString::new(access_token.into()),
            refresh_token: refresh_token.map(|token| SecretString::new(token.into())),
            access_token_expires_at: None,
        },
    )
    .await?;
    let account = repo
        .get(account_id)
        .await?
        .ok_or(AccountRepositoryError::Database(sqlx::Error::RowNotFound))?;
    let account = pool_account_from_stored(account);
    state.account_pool().lock().await.insert(account.clone());
    Ok(account)
}

async fn apply_refresh_failure_status(
    state: &AppState,
    repo: &crate::accounts::repository::AccountRepository,
    account: &StoredAccount,
    failure: RefreshFailure,
) -> Option<AccountStatus> {
    let status = status_for_refresh_failure(failure)?;
    let _ = repo.set_status(&account.id, status).await;
    state
        .account_pool()
        .lock()
        .await
        .set_status(&account.id, status);
    Some(status)
}

fn status_for_refresh_failure(failure: RefreshFailure) -> Option<AccountStatus> {
    match failure {
        RefreshFailure::InvalidGrant => Some(AccountStatus::Expired),
        RefreshFailure::QuotaExhausted => Some(AccountStatus::QuotaExhausted),
        RefreshFailure::Banned => Some(AccountStatus::Banned),
        RefreshFailure::Disabled => Some(AccountStatus::Disabled),
        RefreshFailure::Transport => None,
    }
}

fn public_refresh_failure(failure: RefreshFailure) -> &'static str {
    match failure {
        RefreshFailure::InvalidGrant => "invalidGrant",
        RefreshFailure::QuotaExhausted => "quotaExhausted",
        RefreshFailure::Banned => "banned",
        RefreshFailure::Disabled => "disabled",
        RefreshFailure::Transport => "transport",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexAccountErrorAction {
    SetStatus(AccountStatus),
    RateLimited { retry_after_seconds: u64 },
    CloudflareChallenge { cooldown_seconds: u64 },
}

async fn apply_codex_account_error(
    state: &AppState,
    repo: &crate::accounts::repository::AccountRepository,
    account: &StoredAccount,
    error: &CodexClientError,
) -> Option<AccountStatus> {
    match classify_codex_account_error(error) {
        Some(CodexAccountErrorAction::SetStatus(status)) => {
            let _ = repo.set_status(&account.id, status).await;
            state
                .account_pool()
                .lock()
                .await
                .set_status(&account.id, status);
            Some(status)
        }
        Some(CodexAccountErrorAction::RateLimited {
            retry_after_seconds,
        }) => {
            let cooldown_until = Utc::now() + Duration::seconds(retry_after_seconds as i64);
            state
                .account_pool()
                .lock()
                .await
                .mark_quota_limited_until(&account.id, cooldown_until);
            None
        }
        Some(CodexAccountErrorAction::CloudflareChallenge { cooldown_seconds }) => {
            let cooldown_until = Utc::now() + Duration::seconds(cooldown_seconds as i64);
            state
                .account_pool()
                .lock()
                .await
                .set_cloudflare_cooldown_until(&account.id, cooldown_until);
            None
        }
        None => None,
    }
}

fn classify_codex_account_error(error: &CodexClientError) -> Option<CodexAccountErrorAction> {
    const DEFAULT_RATE_LIMIT_BACKOFF_SECONDS: u64 = 60;
    const MAX_RATE_LIMIT_BACKOFF_SECONDS: u64 = 3_600;
    const CLOUDFLARE_CHALLENGE_COOLDOWN_SECONDS: u64 = 120;

    let CodexClientError::Upstream {
        status,
        body,
        retry_after_seconds,
    } = error
    else {
        return None;
    };
    let lower = body.to_ascii_lowercase();
    if *status == StatusCode::UNAUTHORIZED
        || lower.contains("invalid_grant")
        || lower.contains("invalid_token")
        || lower.contains("access_denied")
        || lower.contains("refresh_token_expired")
        || lower.contains("token_revoked")
    {
        return Some(CodexAccountErrorAction::SetStatus(AccountStatus::Expired));
    }
    if *status == StatusCode::PAYMENT_REQUIRED || lower.contains("quota") {
        return Some(CodexAccountErrorAction::SetStatus(
            AccountStatus::QuotaExhausted,
        ));
    }
    if *status == StatusCode::TOO_MANY_REQUESTS {
        return Some(CodexAccountErrorAction::RateLimited {
            retry_after_seconds: retry_after_seconds
                .unwrap_or(DEFAULT_RATE_LIMIT_BACKOFF_SECONDS)
                .min(MAX_RATE_LIMIT_BACKOFF_SECONDS),
        });
    }
    if *status == StatusCode::FORBIDDEN {
        if is_cloudflare_challenge(body) {
            return Some(CodexAccountErrorAction::CloudflareChallenge {
                cooldown_seconds: CLOUDFLARE_CHALLENGE_COOLDOWN_SECONDS,
            });
        }
        return Some(CodexAccountErrorAction::SetStatus(AccountStatus::Banned));
    }
    if lower.contains("account has been deactivated")
        || lower.contains("deactivated")
        || lower.contains("banned")
        || lower.contains("suspended")
    {
        return Some(CodexAccountErrorAction::SetStatus(AccountStatus::Banned));
    }
    None
}

fn public_codex_error(error: &CodexClientError) -> String {
    match error {
        CodexClientError::Upstream { status, .. } => {
            format!("upstream returned status {}", status.as_u16())
        }
        CodexClientError::Http(_) => "upstream transport failed".to_string(),
        CodexClientError::InvalidHeaderName(_) | CodexClientError::InvalidHeaderValue(_) => {
            "invalid upstream request headers".to_string()
        }
        CodexClientError::UnsupportedTransport(_)
        | CodexClientError::WebSocket(_)
        | CodexClientError::InvalidSse(_)
        | CodexClientError::ModelsUnavailable => "Codex backend request failed".to_string(),
    }
}

fn is_cloudflare_challenge(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("cf-mitigated")
        || lower.contains("cf-chl-bypass")
        || lower.contains("_cf_chl")
        || lower.contains("cf_chl")
        || lower.contains("attention required")
        || lower.contains("just a moment")
}

fn quota_from_usage(usage: &Value) -> Value {
    let additional = usage
        .get("additional_rate_limits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut rate_limits_by_limit_id = Map::new();
    for item in &additional {
        let Some(limit_id) = item
            .get("metered_feature")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let quota = quota_from_rate_limit(item.get("rate_limit"));
        if quota.is_null() {
            continue;
        }
        rate_limits_by_limit_id.insert(
            limit_id.to_string(),
            json!({
                "limit_id": limit_id,
                "limit_name": item.get("limit_name").cloned().unwrap_or(Value::Null),
                "allowed": quota.get("allowed").cloned().unwrap_or(Value::Null),
                "limit_reached": quota.get("limit_reached").cloned().unwrap_or(Value::Null),
                "used_percent": quota.get("used_percent").cloned().unwrap_or(Value::Null),
                "remaining_percent": quota.get("remaining_percent").cloned().unwrap_or(Value::Null),
                "reset_at": quota.get("reset_at").cloned().unwrap_or(Value::Null),
                "limit_window_seconds": quota.get("limit_window_seconds").cloned().unwrap_or(Value::Null),
                "secondary_rate_limit": secondary_quota_from_rate_limit(item.get("rate_limit")),
            }),
        );
    }
    let additional_review = additional.iter().find(|item| {
        is_review_limit_id(item.get("metered_feature").and_then(Value::as_str))
            || is_review_limit_id(item.get("limit_name").and_then(Value::as_str))
    });
    let code_review_rate_limit = match quota_from_rate_limit(usage.get("code_review_rate_limit")) {
        Value::Null => {
            quota_from_rate_limit(additional_review.and_then(|item| item.get("rate_limit")))
        }
        quota => quota,
    };

    json!({
        "plan_type": usage.get("plan_type").cloned().unwrap_or(Value::Null),
        "rate_limit": quota_from_rate_limit(usage.get("rate_limit")),
        "secondary_rate_limit": secondary_quota_from_rate_limit(usage.get("rate_limit")),
        "code_review_rate_limit": code_review_rate_limit,
        "rate_limits_by_limit_id": if rate_limits_by_limit_id.is_empty() {
            Value::Null
        } else {
            Value::Object(rate_limits_by_limit_id)
        },
        "credits": normalize_quota_credits(usage.get("credits")),
    })
}

fn quota_from_rate_limit(rate_limit: Option<&Value>) -> Value {
    let Some(rate_limit) = rate_limit.filter(|value| !value.is_null()) else {
        return Value::Null;
    };
    let primary = rate_limit.get("primary_window");
    let used_percent = primary
        .and_then(|window| window.get("used_percent"))
        .cloned()
        .unwrap_or(Value::Null);
    json!({
        "allowed": rate_limit.get("allowed").cloned().unwrap_or(Value::Null),
        "limit_reached": rate_limit.get("limit_reached").cloned().unwrap_or(Value::Null),
        "used_percent": used_percent,
        "remaining_percent": remaining_percent(primary.and_then(|window| window.get("used_percent"))),
        "reset_at": primary.and_then(|window| window.get("reset_at")).cloned().unwrap_or(Value::Null),
        "limit_window_seconds": primary.and_then(|window| window.get("limit_window_seconds")).cloned().unwrap_or(Value::Null),
    })
}

fn secondary_quota_from_rate_limit(rate_limit: Option<&Value>) -> Value {
    let Some(secondary) = rate_limit
        .and_then(|rate_limit| rate_limit.get("secondary_window"))
        .filter(|value| !value.is_null())
    else {
        return Value::Null;
    };
    let used_percent = secondary
        .get("used_percent")
        .cloned()
        .unwrap_or(Value::Null);
    let limit_reached = secondary
        .get("used_percent")
        .and_then(Value::as_f64)
        .map(|used| used >= 100.0)
        .map(Value::Bool)
        .or_else(|| {
            rate_limit
                .and_then(|rate_limit| rate_limit.get("limit_reached"))
                .cloned()
        })
        .unwrap_or(Value::Null);
    json!({
        "limit_reached": limit_reached,
        "used_percent": used_percent,
        "remaining_percent": remaining_percent(secondary.get("used_percent")),
        "reset_at": secondary.get("reset_at").cloned().unwrap_or(Value::Null),
        "limit_window_seconds": secondary.get("limit_window_seconds").cloned().unwrap_or(Value::Null),
    })
}

fn normalize_quota_credits(raw: Option<&Value>) -> Value {
    let Some(raw) = raw.filter(|value| !value.is_null()) else {
        return Value::Null;
    };
    let Some(balance) = raw
        .get("balance")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite())
    else {
        return Value::Null;
    };
    json!({
        "has_credits": raw.get("has_credits").and_then(Value::as_bool).unwrap_or(false),
        "unlimited": raw.get("unlimited").and_then(Value::as_bool).unwrap_or(false),
        "overage_limit_reached": raw.get("overage_limit_reached").and_then(Value::as_bool).unwrap_or(false),
        "balance": balance,
    })
}

fn remaining_percent(used_percent: Option<&Value>) -> Value {
    let Some(used_percent) = used_percent.and_then(Value::as_f64) else {
        return Value::Null;
    };
    json!((100.0 - used_percent.clamp(0.0, 100.0)).round() as i64)
}

fn is_review_limit_id(value: Option<&str>) -> bool {
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

fn request_domain(base_url: &str) -> Option<String> {
    Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(ToString::to_string))
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

#[derive(Debug, Clone)]
struct ManualAccountClaims {
    account_id: String,
    user_id: Option<String>,
    email: Option<String>,
    plan_type: Option<String>,
    expires_at: DateTime<Utc>,
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

#[derive(Debug)]
enum ValidatedAccountImportError {
    TokenRequired,
    TokenRefresherUnavailable,
    RefreshTransport,
    RefreshRejected,
    InvalidToken(&'static str),
    Inspect,
    NotFound,
    Update,
    Insert,
    Load,
}

async fn store_validated_account_import(
    state: &AppState,
    repo: &crate::accounts::repository::AccountRepository,
    token: Option<String>,
    refresh_token: Option<String>,
) -> Result<StoredAccount, ValidatedAccountImportError> {
    let (access_token, refresh_token_update, new_account_refresh_token) = match (
        empty_to_none(token.map(normalize_bearer_token)),
        empty_to_none(refresh_token),
    ) {
        (Some(token), refresh_token) => (token, refresh_token.clone(), refresh_token),
        (None, Some(refresh_token)) => {
            let Some(refresher) = state.token_refresher() else {
                return Err(ValidatedAccountImportError::TokenRefresherUnavailable);
            };
            let tokens = match refresher.refresh(&refresh_token).await {
                Ok(tokens) => tokens,
                Err(RefreshFailure::Transport) => {
                    return Err(ValidatedAccountImportError::RefreshTransport);
                }
                Err(_) => return Err(ValidatedAccountImportError::RefreshRejected),
            };
            let access_token = normalize_bearer_token(tokens.access_token);
            let rotated_refresh_token = empty_to_none(tokens.refresh_token);
            let new_account_refresh_token = rotated_refresh_token.clone().or(Some(refresh_token));
            (
                access_token,
                rotated_refresh_token,
                new_account_refresh_token,
            )
        }
        (None, None) => return Err(ValidatedAccountImportError::TokenRequired),
    };

    // 手动和 CLI 导入都只信任 ChatGPT JWT claim；请求体里的展示字段不能参与账号身份判定。
    let claims = manual_account_claims(&access_token, Utc::now())
        .map_err(ValidatedAccountImportError::InvalidToken)?;
    let existing = repo
        .find_by_chatgpt_identity(&claims.account_id, claims.user_id.as_deref())
        .await
        .map_err(|_| ValidatedAccountImportError::Inspect)?;

    let account_id = if let Some(existing) = existing {
        let updated = repo
            .update_from_claims(
                &existing.id,
                AccountClaimsUpdate {
                    email: claims.email.clone(),
                    account_id: Some(claims.account_id.clone()),
                    user_id: claims.user_id.clone(),
                    plan_type: claims.plan_type.clone(),
                    access_token: SecretString::new(access_token.into()),
                    refresh_token: refresh_token_update
                        .map(|token| SecretString::new(token.into())),
                    access_token_expires_at: Some(claims.expires_at),
                    status: AccountStatus::Active,
                },
            )
            .await
            .map_err(|_| ValidatedAccountImportError::Update)?;
        if !updated {
            return Err(ValidatedAccountImportError::NotFound);
        }
        existing.id
    } else {
        let id = normalized_account_id(None);
        let account = NewAccount {
            id: id.clone(),
            email: claims.email.clone(),
            account_id: Some(claims.account_id.clone()),
            user_id: claims.user_id.clone(),
            label: None,
            plan_type: claims.plan_type.clone(),
            access_token: SecretString::new(access_token.into()),
            refresh_token: new_account_refresh_token.map(|token| SecretString::new(token.into())),
            access_token_expires_at: Some(claims.expires_at),
            status: AccountStatus::Active,
        };
        repo.insert(account)
            .await
            .map_err(|_| ValidatedAccountImportError::Insert)?;
        id
    };

    let stored = repo
        .get(&account_id)
        .await
        .map_err(|_| ValidatedAccountImportError::Load)?
        .ok_or(ValidatedAccountImportError::NotFound)?;
    state
        .account_pool()
        .lock()
        .await
        .insert(pool_account_from_stored(stored.clone()));
    Ok(stored)
}

fn validated_account_import_error_response(
    error: ValidatedAccountImportError,
    request_id: String,
) -> Response {
    match error {
        ValidatedAccountImportError::TokenRequired => AdminResponse::new(
            StatusCode::BAD_REQUEST,
            AdminEnvelope::new(
                40001,
                "Either token or refreshToken is required",
                (),
                request_id,
            ),
        )
        .into_response(),
        ValidatedAccountImportError::TokenRefresherUnavailable => AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Token refresher is not initialized", (), request_id),
        )
        .into_response(),
        ValidatedAccountImportError::RefreshTransport => AdminResponse::new(
            StatusCode::BAD_GATEWAY,
            AdminEnvelope::new(50201, "Refresh token exchange failed", (), request_id),
        )
        .into_response(),
        ValidatedAccountImportError::RefreshRejected => AdminResponse::new(
            StatusCode::BAD_REQUEST,
            AdminEnvelope::new(40001, "Refresh token exchange failed", (), request_id),
        )
        .into_response(),
        ValidatedAccountImportError::InvalidToken(message) => AdminResponse::new(
            StatusCode::BAD_REQUEST,
            AdminEnvelope::new(40001, message, (), request_id),
        )
        .into_response(),
        ValidatedAccountImportError::Inspect => AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Failed to inspect account", (), request_id),
        )
        .into_response(),
        ValidatedAccountImportError::NotFound => AdminResponse::new(
            StatusCode::NOT_FOUND,
            AdminEnvelope::new(40401, "Account not found", (), request_id),
        )
        .into_response(),
        ValidatedAccountImportError::Update => AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Failed to update account", (), request_id),
        )
        .into_response(),
        ValidatedAccountImportError::Insert => AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Failed to create account", (), request_id),
        )
        .into_response(),
        ValidatedAccountImportError::Load => AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Failed to load account", (), request_id),
        )
        .into_response(),
    }
}

fn manual_account_claims(
    token: &str,
    now: DateTime<Utc>,
) -> Result<ManualAccountClaims, &'static str> {
    let payload = decode_jwt_payload(token).ok_or("Invalid JWT format")?;
    let exp = payload
        .get("exp")
        .and_then(Value::as_i64)
        .ok_or("Token is expired")?;
    if now.timestamp() >= exp {
        return Err("Token is expired");
    }
    let expires_at = DateTime::<Utc>::from_timestamp(exp, 0).ok_or("Invalid JWT exp claim")?;
    let auth = payload
        .get("https://api.openai.com/auth")
        .and_then(Value::as_object)
        .ok_or("Token missing chatgpt_account_id claim")?;
    let account_id =
        string_claim(auth, "chatgpt_account_id").ok_or("Token missing chatgpt_account_id claim")?;
    let profile = payload
        .get("https://api.openai.com/profile")
        .and_then(Value::as_object);
    let user_id = string_claim(auth, "chatgpt_user_id")
        .or_else(|| profile.and_then(|profile| string_claim(profile, "chatgpt_user_id")));
    let plan_type = string_claim(auth, "chatgpt_plan_type")
        .or_else(|| profile.and_then(|profile| string_claim(profile, "chatgpt_plan_type")));
    let email = profile.and_then(|profile| string_claim(profile, "email"));

    Ok(ManualAccountClaims {
        account_id,
        user_id,
        email,
        plan_type,
        expires_at,
    })
}

fn decode_jwt_payload(token: &str) -> Option<Map<String, Value>> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    if payload.is_empty() {
        return None;
    }
    // OpenAI access token 这里按 TS reference 只解码 payload，不做本地签名验签。
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice::<Value>(&bytes)
        .ok()?
        .as_object()
        .cloned()
}

fn string_claim(map: &Map<String, Value>, key: &str) -> Option<String> {
    map.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

#[derive(Debug)]
enum StoredImportAccount {
    Imported,
    Skipped,
}

#[derive(Debug)]
enum StoreImportAccountError {
    Inspect,
    Invalid(String),
    Insert,
}

async fn store_import_account_entry(
    state: &AppState,
    repo: &crate::accounts::repository::AccountRepository,
    entry: AccountImportEntry,
) -> Result<StoredImportAccount, StoreImportAccountError> {
    let access_token = entry.token.as_deref().unwrap_or_default().trim();
    if access_token.is_empty() {
        return Ok(StoredImportAccount::Skipped);
    }
    if entry
        .label
        .as_ref()
        .is_some_and(|label| label.chars().count() > 64)
    {
        return Err(StoreImportAccountError::Invalid(
            "Account label must be 64 characters or fewer".to_string(),
        ));
    }

    let id = normalized_account_id(entry.id);
    match repo.exists(&id).await {
        Ok(true) => return Ok(StoredImportAccount::Skipped),
        Ok(false) => {}
        Err(_) => return Err(StoreImportAccountError::Inspect),
    }

    let status =
        parse_import_status(entry.status.as_deref()).map_err(StoreImportAccountError::Invalid)?;
    let email = empty_to_none(entry.email);
    let account_id = empty_to_none(entry.account_id);
    let user_id = empty_to_none(entry.user_id);
    let label = empty_to_none(entry.label);
    let plan_type = empty_to_none(entry.plan_type);
    let refresh_token = empty_to_none(entry.refresh_token);
    let access_token = access_token.to_string();
    let now = chrono::Utc::now().to_rfc3339();
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
        access_token: SecretString::new(access_token.into()),
        refresh_token: refresh_token.map(|token| SecretString::new(token.into())),
        access_token_expires_at: None,
        status,
    };
    repo.insert(account)
        .await
        .map_err(|_| StoreImportAccountError::Insert)?;
    state.account_pool().lock().await.insert(pool_account);

    Ok(StoredImportAccount::Imported)
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

fn parse_client_api_key_import_payload(payload: &Value) -> Vec<ClientApiKeyImportEntry> {
    let payload = payload
        .get("data")
        .filter(|data| data.get("apiKeys").is_some() || data.get("keys").is_some())
        .unwrap_or(payload);

    if let Some(keys) = payload.get("apiKeys").and_then(Value::as_array) {
        return keys
            .iter()
            .filter_map(client_api_key_import_entry_from_value)
            .collect();
    }
    if let Some(keys) = payload.get("keys").and_then(Value::as_array) {
        return keys
            .iter()
            .filter_map(client_api_key_import_entry_from_value)
            .collect();
    }
    if let Some(keys) = payload.as_array() {
        return keys
            .iter()
            .filter_map(client_api_key_import_entry_from_value)
            .collect();
    }

    client_api_key_import_entry_from_value(payload)
        .into_iter()
        .collect()
}

fn client_api_key_import_entry_from_value(value: &Value) -> Option<ClientApiKeyImportEntry> {
    value.as_object()?;
    let name = first_string(value, &[&["name"]])?;
    // 安全边界：导入即使收到 plaintext/keyHash，也只按元数据轮换生成新的本地 cpr_ key。
    Some(ClientApiKeyImportEntry {
        source_id: first_string(value, &[&["id"], &["sourceId"]]),
        source_prefix: first_string(value, &[&["prefix"], &["sourcePrefix"]]),
        name,
        label: first_string(value, &[&["label"]]),
        enabled: client_api_key_import_enabled(value),
    })
}

fn client_api_key_import_enabled(value: &Value) -> bool {
    if let Some(enabled) = value.get("enabled").and_then(Value::as_bool) {
        return enabled;
    }
    !first_string(value, &[&["status"]])
        .unwrap_or_else(|| "active".to_string())
        .trim()
        .eq_ignore_ascii_case("disabled")
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
