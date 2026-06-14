use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};

use crate::{platform::http::request_id::RequestId, runtime::state::AppState};

use super::{
    api_key_service_error, ApiKeyExportQuery, ClientApiKeyExportData, ClientApiKeyExportEntry,
};
use crate::admin::api::{
    account_export_ids, require_admin_session, AdminEnvelope, AdminError, AdminResponse,
};

pub async fn export_api_keys(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<ApiKeyExportQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    let ids = account_export_ids(query.ids.as_deref());
    let keys = match state.services.api_keys.export(ids).await {
        Ok(keys) => keys,
        Err(error) => return Err(api_key_service_error(error, request_id)),
    };

    // 安全边界：本地 cpr_ key 只导出可展示元数据，绝不导出 plaintext、key_hash 或 pepper。
    let data = ClientApiKeyExportData {
        source_format: "rustLocalClientApiKeys",
        rotation_required: true,
        api_keys: keys
            .into_iter()
            .map(ClientApiKeyExportEntry::from)
            .collect(),
    };
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(data, request_id),
    ))
}
