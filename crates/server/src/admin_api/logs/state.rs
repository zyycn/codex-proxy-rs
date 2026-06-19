//! 日志状态处理器。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension, Json,
};
use codex_proxy_runtime::state::AppState;
use serde::Deserialize;

use crate::{
    admin_api::{
        logs::{log_error, ClearLogsData, LogStateData},
        require_admin_session, AdminEnvelope, AdminError, AdminResponse,
    },
    middleware::request_id::RequestId,
};

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

/// 读取日志状态。
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

/// 更新日志状态。
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

/// 清空事件日志。
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

impl From<UpdateLogStateRequest> for codex_proxy_runtime::services::AdminLogStateUpdate {
    fn from(request: UpdateLogStateRequest) -> Self {
        Self {
            enabled: request.enabled,
            capacity: request.capacity,
            capture_body: request.capture_body,
        }
    }
}
