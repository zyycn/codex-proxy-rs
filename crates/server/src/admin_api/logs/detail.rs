//! 日志详情处理器。

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};
use codex_proxy_runtime::state::AppState;

use crate::{
    admin_api::{logs::log_error, require_admin_session, AdminEnvelope, AdminError, AdminResponse},
    middleware::request_id::RequestId,
};

/// 查询单条事件日志。
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
