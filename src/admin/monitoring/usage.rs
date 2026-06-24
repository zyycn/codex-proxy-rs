//! 管理端用量处理器。

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};
use serde::{Deserialize, Serialize};

use crate::{
    admin::auth::session::require_admin_session,
    admin::monitoring::service::{AdminUsageError, AdminUsageRecord, AdminUsageSummary},
    admin::response::{AdminEnvelope, AdminError, AdminPageEnvelope, AdminResponse},
    http::middleware::request_id::RequestId,
    infra::json::{clamp_limit, clamp_page, Page},
    runtime::state::AppState,
};

/// 用量统计查询参数。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageStatsQuery {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
    pub page: Option<u32>,
    pub page_size: Option<u32>,
}

/// 管理端账号用量响应。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminUsageStatsData {
    pub account_id: String,
    pub email: Option<String>,
    pub label: Option<String>,
    pub plan_type: Option<String>,
    pub request_count: i64,
    pub empty_response_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
    pub reasoning_tokens: i64,
    pub total_tokens: i64,
    pub image_input_tokens: i64,
    pub image_output_tokens: i64,
    pub image_request_count: i64,
    pub image_request_failed_count: i64,
    pub last_used_at: Option<String>,
}

/// 管理端账号用量汇总响应。
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminUsageStatsSummaryData {
    pub account_count: i64,
    pub request_count: i64,
    pub empty_response_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
    pub reasoning_tokens: i64,
    pub total_tokens: i64,
    pub image_input_tokens: i64,
    pub image_output_tokens: i64,
    pub image_request_count: i64,
    pub image_request_failed_count: i64,
}

/// `GET /api/admin/usage`
pub async fn usage_stats(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<UsageStatsQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let limit = clamp_limit(query.page_size.or(query.limit).unwrap_or(50));
    let use_numbered_page = query.page.is_some() || query.page_size.is_some();

    if use_numbered_page {
        return match state
            .services
            .usage
            .list_page(clamp_page(query.page.unwrap_or(1)), limit)
            .await
        {
            Ok(page) => {
                let page = crate::infra::json::NumberedPage {
                    items: page
                        .items
                        .into_iter()
                        .map(AdminUsageStatsData::from)
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
            Err(error) => Err(usage_error(error, request_id)),
        };
    }

    match state.services.usage.list(query.cursor, limit).await {
        Ok(page) => {
            let page = Page {
                items: page
                    .items
                    .into_iter()
                    .map(AdminUsageStatsData::from)
                    .collect(),
                next_cursor: page.next_cursor,
            };
            Ok(AdminResponse::new(
                StatusCode::OK,
                AdminPageEnvelope::ok(page, limit, request_id),
            ))
        }
        Err(error) => Err(usage_error(error, request_id)),
    }
}

/// `GET /api/admin/usage/summary`
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
        Err(error) => Err(usage_error(error, request_id)),
    }
}

fn usage_error(error: AdminUsageError, request_id: String) -> AdminError {
    AdminError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        50001,
        error.to_string(),
        request_id,
    )
}

impl From<AdminUsageRecord> for AdminUsageStatsData {
    fn from(usage: AdminUsageRecord) -> Self {
        Self {
            account_id: usage.account_id,
            email: usage.email,
            label: usage.label,
            plan_type: usage.plan_type,
            request_count: usage.request_count,
            empty_response_count: usage.empty_response_count,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cached_tokens: usage.cached_tokens,
            reasoning_tokens: usage.reasoning_tokens,
            total_tokens: usage.total_tokens,
            image_input_tokens: usage.image_input_tokens,
            image_output_tokens: usage.image_output_tokens,
            image_request_count: usage.image_request_count,
            image_request_failed_count: usage.image_request_failed_count,
            last_used_at: usage.last_used_at.map(|v| v.to_rfc3339()),
        }
    }
}

impl From<AdminUsageSummary> for AdminUsageStatsSummaryData {
    fn from(summary: AdminUsageSummary) -> Self {
        Self {
            account_count: summary.account_count,
            request_count: summary.request_count,
            empty_response_count: summary.empty_response_count,
            input_tokens: summary.input_tokens,
            output_tokens: summary.output_tokens,
            cached_tokens: summary.cached_tokens,
            reasoning_tokens: summary.reasoning_tokens,
            total_tokens: summary.total_tokens,
            image_input_tokens: summary.image_input_tokens,
            image_output_tokens: summary.image_output_tokens,
            image_request_count: summary.image_request_count,
            image_request_failed_count: summary.image_request_failed_count,
        }
    }
}
