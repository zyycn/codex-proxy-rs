use axum::{
    extract::{Query, State},
    http::{
        header::{HeaderValue, SET_COOKIE},
        HeaderMap, StatusCode,
    },
    response::{IntoResponse, Response},
    Extension, Json,
};
use chrono::{Duration, Utc};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::{
    accounts::{
        model::{Account, AccountStatus},
        repository::{NewAccount, StoredAccountMetadata},
    },
    auth::admin_session::verify_admin_password,
    config::{AppConfig, QuotaWarningThresholds},
    http::{auth::admin_session_id, middleware::RequestId},
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountImportPayload {
    pub accounts: Vec<AccountImportEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountImportEntry {
    pub id: Option<String>,
    pub email: Option<String>,
    pub account_id: Option<String>,
    pub user_id: Option<String>,
    pub label: Option<String>,
    pub plan_type: Option<String>,
    pub token: String,
    pub refresh_token: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountImportData {
    pub imported: u32,
    pub skipped: u32,
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

pub async fn import_accounts(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<AccountImportPayload>,
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
    if payload.accounts.is_empty() {
        return AdminResponse::new(
            StatusCode::BAD_REQUEST,
            AdminEnvelope::new(40001, "No importable accounts found", (), request_id),
        )
        .into_response();
    }

    let mut imported = 0u32;
    let mut skipped = 0u32;
    for entry in payload.accounts {
        let token = entry.token.trim();
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
        AdminEnvelope::ok(AccountImportData { imported, skipped }, request_id),
    )
    .into_response()
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

fn normalized_account_id(id: Option<String>) -> String {
    id.and_then(|id| empty_to_none(Some(id)))
        .unwrap_or_else(|| format!("acct_{}", Uuid::new_v4().simple()))
}

fn empty_to_none(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_import_status(status: Option<&str>) -> Result<AccountStatus, String> {
    match status.unwrap_or("active") {
        "active" => Ok(AccountStatus::Active),
        "expired" => Ok(AccountStatus::Expired),
        "quota_exhausted" => Ok(AccountStatus::QuotaExhausted),
        "refreshing" => Ok(AccountStatus::Refreshing),
        "disabled" => Ok(AccountStatus::Disabled),
        "banned" => Ok(AccountStatus::Banned),
        other => Err(format!("Unsupported account status: {other}")),
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
