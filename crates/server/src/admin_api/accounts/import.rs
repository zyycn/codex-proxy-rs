//! 账号导入处理器。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension, Json,
};
use codex_proxy_runtime::state::AppState;
use serde::Serialize;
use serde_json::Value;

use crate::{
    admin_api::{
        accounts::account_error, require_admin_session, AdminEnvelope, AdminError, AdminResponse,
    },
    middleware::request_id::RequestId,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountImportData {
    imported: u32,
    skipped: u32,
    source_format: String,
}

/// `POST /api/admin/accounts/import`
pub async fn import_accounts(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    match state.services.admin_accounts.import(&payload).await {
        Ok(imported) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(
                AccountImportData {
                    imported: imported.imported,
                    skipped: imported.skipped,
                    source_format: imported.source_format.to_string(),
                },
                request_id,
            ),
        )),
        Err(error) => Err(account_error(error, request_id)),
    }
}
