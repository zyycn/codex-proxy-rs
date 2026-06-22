//! 事件日志 HTTP 处理器。

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension, Json,
};
use serde::{Deserialize, Serialize};

use crate::{
    admin::{
        response::{AdminEnvelope, AdminError, AdminPageEnvelope, AdminResponse},
        session::require_admin_session,
    },
    app::{
        services::{AdminLogFilter, AdminLogStateUpdate},
        state::AppState,
    },
    http::middleware::request_id::RequestId,
    infra::json::clamp_limit,
    telemetry::events::EventLevel,
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

/// 管理端日志状态响应。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogStateData {
    /// 是否启用。
    pub enabled: bool,
    /// 内存容量。
    pub capacity: u32,
    /// 是否捕获请求体。
    pub capture_body: bool,
    /// 已存储数量。
    pub stored_count: u64,
}

/// 清空日志响应。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearLogsData {
    /// 清理数量。
    pub cleared: u64,
}

/// 更新日志状态请求。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateLogStateRequest {
    /// 是否启用。
    pub enabled: Option<bool>,
    /// 日志容量。
    pub capacity: Option<u32>,
    /// 是否捕获请求体。
    pub capture_body: Option<bool>,
}

/// `GET /api/admin/logs`
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

/// `GET /api/admin/logs/{log_id}`
pub async fn log_detail(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(log_id): Path<String>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    match state.services.logs.get(&log_id).await {
        Ok(Some(log)) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(log, request_id),
        )),
        Ok(None) => Err(AdminError::new(
            StatusCode::NOT_FOUND,
            40401,
            "Log event not found",
            request_id,
        )),
        Err(error) => Err(log_error(error, request_id)),
    }
}

/// `DELETE /api/admin/logs`
pub async fn clear_logs(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    match state.services.logs.clear().await {
        Ok(cleared) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(
                ClearLogsData {
                    cleared: cleared.cleared,
                },
                request_id,
            ),
        )),
        Err(error) => Err(log_error(error, request_id)),
    }
}

/// `GET /api/admin/logs/state`
pub async fn logs_state(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    match state.services.logs.state().await {
        Ok(state) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(LogStateData::from(state), request_id),
        )),
        Err(error) => Err(log_error(error, request_id)),
    }
}

/// `PATCH /api/admin/logs/state`
pub async fn update_logs_state(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<UpdateLogStateRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    match state.services.logs.update_state(payload.into()).await {
        Ok(state) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(LogStateData::from(state), request_id),
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

fn log_error(error: crate::app::services::AdminLogError, request_id: String) -> AdminError {
    match error {
        crate::app::services::AdminLogError::List
        | crate::app::services::AdminLogError::Get
        | crate::app::services::AdminLogError::Count
        | crate::app::services::AdminLogError::Clear
        | crate::app::services::AdminLogError::Append
        | crate::app::services::AdminLogError::Trim => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            error.to_string(),
            request_id,
        ),
        crate::app::services::AdminLogError::InvalidCapacity => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            error.to_string(),
            request_id,
        ),
    }
}

fn level_from_query(value: Option<String>) -> Result<Option<EventLevel>, String> {
    let Some(value) = non_empty(value) else {
        return Ok(None);
    };
    match value.as_str() {
        "debug" => Ok(Some(EventLevel::Debug)),
        "info" => Ok(Some(EventLevel::Info)),
        "warn" => Ok(Some(EventLevel::Warn)),
        "error" => Ok(Some(EventLevel::Error)),
        other => Err(format!("Unsupported log level: {other}")),
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

impl From<crate::app::services::AdminLogState> for LogStateData {
    fn from(state: crate::app::services::AdminLogState) -> Self {
        Self {
            enabled: state.enabled,
            capacity: state.capacity,
            capture_body: state.capture_body,
            stored_count: state.stored_count,
        }
    }
}

impl From<UpdateLogStateRequest> for AdminLogStateUpdate {
    fn from(request: UpdateLogStateRequest) -> Self {
        Self {
            enabled: request.enabled,
            capacity: request.capacity,
            capture_body: request.capture_body,
        }
    }
}
