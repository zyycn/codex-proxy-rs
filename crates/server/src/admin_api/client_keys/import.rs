//! 客户端 key 导入处理器。

use axum::{
    extract::State, http::HeaderMap, http::StatusCode, response::IntoResponse, Extension, Json,
};
use codex_proxy_runtime::state::AppState;
use serde_json::Value;

use crate::{
    admin_api::{
        client_keys::{client_key_error, imported_client_key_data, ClientApiKeyImportData},
        require_admin_session, AdminEnvelope, AdminError, AdminResponse,
    },
    middleware::request_id::RequestId,
};

/// 导入客户端 API Key 元数据并轮换新密钥。
pub async fn import_api_keys(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    match state.services.admin_client_keys.import(&payload).await {
        Ok(imported) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(
                ClientApiKeyImportData {
                    imported: imported.imported,
                    skipped: imported.skipped,
                    rotated: true,
                    keys: imported
                        .keys
                        .into_iter()
                        .map(imported_client_key_data)
                        .collect(),
                },
                request_id,
            ),
        )),
        Err(error) => Err(client_key_error(error, request_id)),
    }
}
