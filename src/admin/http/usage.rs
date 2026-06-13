use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};
use serde::{Deserialize, Serialize};

use crate::{
    codex::accounts::repository::{
        AccountRepositoryError, AccountUsageListRecord, AccountUsageSummary,
    },
    codex::usage::service::UsageServiceError,
    platform::http::middleware::RequestId,
    runtime::state::AppState,
    utils::pagination::{clamp_limit, Page},
};

use super::{require_admin_session, AdminEnvelope, AdminError, AdminPageEnvelope, AdminResponse};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageStatsQuery {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
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

pub async fn usage_stats(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<UsageStatsQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let limit = clamp_limit(query.limit.unwrap_or(50));
    match state.services.usage.list(query.cursor, limit).await {
        Ok(page) => {
            let Page { items, next_cursor } = page;
            let page = Page {
                items: items.into_iter().map(AdminUsageStatsData::from).collect(),
                next_cursor,
            };
            Ok(AdminResponse::new(
                StatusCode::OK,
                AdminPageEnvelope::ok(page, limit, request_id),
            ))
        }
        Err(error) => Err(usage_list_error(error, request_id)),
    }
}

fn usage_list_error(error: UsageServiceError, request_id: String) -> AdminError {
    match error {
        UsageServiceError::RepositoryUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Account usage repository is not initialized",
            request_id,
        ),
        UsageServiceError::Repository(AccountRepositoryError::InvalidCursor) => {
            AdminError::new(StatusCode::BAD_REQUEST, 40002, "Invalid cursor", request_id)
        }
        UsageServiceError::Repository(_) => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to list usage stats",
            request_id,
        ),
    }
}

pub async fn usage_stats_summary(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    match state.services.usage.summary().await {
        Ok(summary) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(AdminUsageStatsSummaryData::from(summary), request_id),
        )),
        Err(error) => Err(usage_summary_error(error, request_id)),
    }
}

fn usage_summary_error(error: UsageServiceError, request_id: String) -> AdminError {
    match error {
        UsageServiceError::RepositoryUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Account usage repository is not initialized",
            request_id,
        ),
        UsageServiceError::Repository(_) => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to summarize usage stats",
            request_id,
        ),
    }
}
