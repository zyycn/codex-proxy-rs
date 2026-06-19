//! 客户端 key 导出处理器。

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};
use codex_proxy_runtime::state::AppState;

use crate::{
    admin_api::{
        client_keys::{
            client_key_error, ApiKeyExportQuery, ClientApiKeyExportData, ClientApiKeyExportEntry,
        },
        require_admin_session, AdminEnvelope, AdminError, AdminResponse,
    },
    middleware::request_id::RequestId,
};

/// 导出客户端 API Key 元数据。
pub async fn export_api_keys(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<ApiKeyExportQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    let ids = query
        .ids
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();

    match state.services.admin_client_keys.export(ids).await {
        Ok(keys) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(
                ClientApiKeyExportData {
                    source_format: "rustLocalClientApiKeys",
                    rotation_required: true,
                    api_keys: keys
                        .into_iter()
                        .map(ClientApiKeyExportEntry::from)
                        .collect(),
                },
                request_id,
            ),
        )),
        Err(error) => Err(client_key_error(error, request_id)),
    }
}
