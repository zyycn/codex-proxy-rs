use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension, Json,
};
use serde::Deserialize;

use crate::{platform::http::request_id::RequestId, runtime::state::AppState};

use super::super::{require_admin_session, AdminEnvelope, AdminError, AdminResponse};
use super::{admin_account_data_from_stored, validated_account_import_error};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateAccountRequest {
    pub token: Option<String>,
    pub refresh_token: Option<String>,
}

pub async fn create_account(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<CreateAccountRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let stored = state
        .services
        .accounts
        .import_validated(payload.token, payload.refresh_token)
        .await
        .map_err(|error| validated_account_import_error(error, &request_id))?;

    // 手动添加账号的响应只返回可展示元数据，OAuth token 永不回显。
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(admin_account_data_from_stored(stored), request_id),
    ))
}
