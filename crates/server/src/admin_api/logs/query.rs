//! 日志查询处理器。

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};
use codex_proxy_platform::json::clamp_limit;
use codex_proxy_runtime::{services::AdminLogFilter, state::AppState};
use serde::Deserialize;

use crate::{
    admin_api::{
        logs::{level_from_query, log_error, non_empty},
        require_admin_session, AdminError, AdminPageEnvelope, AdminResponse,
    },
    middleware::request_id::RequestId,
};

/// 日志查询参数。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogsQuery {
    /// 分页游标。
    pub cursor: Option<String>,
    /// 分页大小。
    pub limit: Option<u32>,
    /// 事件类别。
    pub kind: Option<String>,
    /// 事件等级。
    pub level: Option<String>,
    /// 请求 ID。
    pub request_id: Option<String>,
    /// 账号 ID。
    pub account_id: Option<String>,
    /// 路由。
    pub route: Option<String>,
    /// 模型。
    pub model: Option<String>,
    /// HTTP 状态码。
    pub status_code: Option<i64>,
    /// 上游传输方式。
    pub transport: Option<String>,
    /// 同一请求内的上游尝试序号。
    pub attempt_index: Option<i64>,
    /// 上游 HTTP 状态码。
    pub upstream_status_code: Option<i64>,
    /// 失败分类。
    pub failure_class: Option<String>,
    /// 上游响应 ID。
    pub response_id: Option<String>,
    /// 上游请求 ID。
    pub upstream_request_id: Option<String>,
    /// 搜索关键词。
    pub search: Option<String>,
}

/// 查询事件日志。
pub async fn logs(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<LogsQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    let limit = clamp_limit(query.limit.unwrap_or(50));
    let cursor = query.cursor.clone();
    let filter = filter_from_query(query, &request_id)?;

    match state.services.logs.list(cursor, limit, filter).await {
        Ok(page) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminPageEnvelope::ok(page, limit, request_id),
        )),
        Err(error) => Err(log_error(error, request_id)),
    }
}

fn filter_from_query(query: LogsQuery, request_id: &str) -> Result<AdminLogFilter, AdminError> {
    Ok(AdminLogFilter {
        kind: non_empty(query.kind),
        level: level_from_query(query.level).map_err(|message| {
            AdminError::new(StatusCode::BAD_REQUEST, 40001, message, request_id)
        })?,
        request_id: non_empty(query.request_id),
        account_id: non_empty(query.account_id),
        route: non_empty(query.route),
        model: non_empty(query.model),
        status_code: query.status_code,
        transport: non_empty(query.transport),
        attempt_index: query.attempt_index,
        upstream_status_code: query.upstream_status_code,
        failure_class: non_empty(query.failure_class),
        response_id: non_empty(query.response_id),
        upstream_request_id: non_empty(query.upstream_request_id),
        search: non_empty(query.search),
    })
}
