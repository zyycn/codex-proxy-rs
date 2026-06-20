//! 管理端用量处理器。

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};
use codex_proxy_platform::json::{clamp_limit, Page};
use codex_proxy_runtime::{
    services::{AdminUsageError, AdminUsageRecord, AdminUsageSummary},
    state::AppState,
};
use serde::{Deserialize, Serialize};

use crate::{
    admin_api::{
        require_admin_session, AdminEnvelope, AdminError, AdminPageEnvelope, AdminResponse,
    },
    middleware::request_id::RequestId,
};

/// 用量统计查询参数。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageStatsQuery {
    /// 分页游标。
    pub cursor: Option<String>,
    /// 分页大小。
    pub limit: Option<u32>,
}

/// 管理端账号用量响应。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminUsageStatsData {
    /// 账号 ID。
    pub account_id: String,
    /// 邮箱。
    pub email: Option<String>,
    /// 管理标签。
    pub label: Option<String>,
    /// 计划类型。
    pub plan_type: Option<String>,
    /// 请求数。
    pub request_count: i64,
    /// 空响应数。
    pub empty_response_count: i64,
    /// 输入 token 数。
    pub input_tokens: i64,
    /// 输出 token 数。
    pub output_tokens: i64,
    /// 缓存 token 数。
    pub cached_tokens: i64,
    /// reasoning token 数。
    pub reasoning_tokens: i64,
    /// 上游返回的总 token 数。
    pub total_tokens: i64,
    /// 图片输入 token 数。
    pub image_input_tokens: i64,
    /// 图片输出 token 数。
    pub image_output_tokens: i64,
    /// 图片请求数。
    pub image_request_count: i64,
    /// 图片请求失败数。
    pub image_request_failed_count: i64,
    /// 最近使用时间。
    pub last_used_at: Option<String>,
}

/// 管理端账号用量汇总响应。
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminUsageStatsSummaryData {
    /// 有用量记录的账号数。
    pub account_count: i64,
    /// 请求总数。
    pub request_count: i64,
    /// 空响应总数。
    pub empty_response_count: i64,
    /// 输入 token 总数。
    pub input_tokens: i64,
    /// 输出 token 总数。
    pub output_tokens: i64,
    /// 缓存 token 总数。
    pub cached_tokens: i64,
    /// reasoning token 总数。
    pub reasoning_tokens: i64,
    /// 上游返回 token 总数。
    pub total_tokens: i64,
    /// 图片输入 token 总数。
    pub image_input_tokens: i64,
    /// 图片输出 token 总数。
    pub image_output_tokens: i64,
    /// 图片请求总数。
    pub image_request_count: i64,
    /// 图片请求失败总数。
    pub image_request_failed_count: i64,
}

/// `GET /api/admin/usage-stats`
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

/// `GET /api/admin/usage-stats/summary`
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
            last_used_at: usage.last_used_at.map(|value| value.to_rfc3339()),
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
