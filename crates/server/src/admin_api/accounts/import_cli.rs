//! Codex CLI 账号导入处理器。

use std::path::{Path, PathBuf};

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension, Json,
};
use codex_proxy_runtime::{services::AdminAccountError, state::AppState};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    admin_api::{
        accounts::account_error, require_admin_session, AdminEnvelope, AdminError, AdminResponse,
    },
    middleware::request_id::RequestId,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportCliRequest {
    codex_home: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountImportCliData {
    imported: u32,
    skipped: u32,
    source_format: &'static str,
}

/// `POST /api/admin/accounts/import-cli`
pub async fn import_cli_account(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<ImportCliRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let auth_path = PathBuf::from(payload.codex_home).join("auth.json");
    let auth_payload =
        read_auth_json(&auth_path).map_err(|error| account_error(error, request_id.clone()))?;

    match state
        .services
        .admin_accounts
        .import_codex_cli_auth(&auth_payload)
        .await
    {
        Ok(_) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(
                AccountImportCliData {
                    imported: 1,
                    skipped: 0,
                    source_format: "codex_cli",
                },
                request_id,
            ),
        )),
        Err(error) => Err(account_error(error, request_id)),
    }
}

fn read_auth_json(path: &Path) -> Result<Value, AdminAccountError> {
    let content =
        std::fs::read_to_string(path).map_err(|_| AdminAccountError::NoImportableAccounts)?;
    serde_json::from_str(&content).map_err(|_| AdminAccountError::NoImportableAccounts)
}
