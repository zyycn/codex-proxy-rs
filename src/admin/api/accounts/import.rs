use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    codex::{
        accounts::service::{AccountImportEntry, StoreImportAccountError},
        gateway::oauth::{default_codex_home, read_cli_auth_from_home},
    },
    platform::http::request_id::RequestId,
    runtime::state::AppState,
    utils::json::first_string,
};

use super::super::{require_admin_session, AdminEnvelope, AdminError, AdminResponse};
use super::validated_account_import_error;

const NATIVE_IMPORT_CONTAINER_KEYS: [&str; 1] = ["accounts"];
const NATIVE_IMPORT_ACCOUNT_KEYS: [&str; 10] = [
    "id",
    "email",
    "accountId",
    "userId",
    "label",
    "planType",
    "token",
    "refreshToken",
    "accessTokenExpiresAt",
    "status",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountImportFormat {
    Native,
    CodexCli,
}

impl AccountImportFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::CodexCli => "codex_cli",
        }
    }
}

#[derive(Debug, Clone)]
struct ParsedAccountImportPayload {
    accounts: Vec<AccountImportEntry>,
    source_format: AccountImportFormat,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountImportData {
    pub imported: u32,
    pub skipped: u32,
    pub source_format: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportCliAuthRequest {
    pub codex_home: Option<String>,
}

pub async fn import_accounts(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    if !state.services.accounts.has_repository() {
        return Err(repository_unavailable(&request_id));
    }
    let parsed = parse_account_import_payload(&payload);
    if parsed.accounts.is_empty() {
        return Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "No importable accounts found",
            request_id,
        ));
    }

    let counts = state
        .services
        .accounts
        .import_entries(parsed.accounts)
        .await
        .map_err(|error| store_import_account_error(error, &request_id))?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            AccountImportData {
                imported: counts.imported,
                skipped: counts.skipped,
                source_format: parsed.source_format.as_str().to_string(),
            },
            request_id,
        ),
    ))
}

pub async fn import_cli_auth(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let payload = if body.is_empty() {
        ImportCliAuthRequest::default()
    } else {
        match serde_json::from_slice::<ImportCliAuthRequest>(&body) {
            Ok(payload) => payload,
            Err(_) => {
                return Err(AdminError::new(
                    StatusCode::BAD_REQUEST,
                    40001,
                    "Invalid CLI import request",
                    request_id,
                ));
            }
        }
    };
    let codex_home = match empty_to_none(payload.codex_home) {
        Some(path) => std::path::PathBuf::from(path),
        None => match default_codex_home() {
            Ok(path) => path,
            Err(error) => {
                return Err(AdminError::new(
                    StatusCode::BAD_REQUEST,
                    40001,
                    error.to_string(),
                    request_id,
                ));
            }
        },
    };
    let cli_auth = match read_cli_auth_from_home(&codex_home) {
        Ok(auth) => auth,
        Err(error) => {
            return Err(AdminError::new(
                StatusCode::BAD_REQUEST,
                40001,
                error.to_string(),
                request_id,
            ));
        }
    };
    let _stored = state
        .services
        .accounts
        .import_validated(
            Some(cli_auth.access_token().to_string()),
            cli_auth.refresh_token().map(str::to_string),
        )
        .await
        .map_err(|error| validated_account_import_error(error, &request_id))?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            AccountImportData {
                imported: 1,
                skipped: 0,
                source_format: AccountImportFormat::CodexCli.as_str().to_string(),
            },
            request_id,
        ),
    ))
}

fn repository_unavailable(request_id: &str) -> AdminError {
    AdminError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        50001,
        "Account repository is not initialized",
        request_id,
    )
}

fn store_import_account_error(error: StoreImportAccountError, request_id: &str) -> AdminError {
    match error {
        StoreImportAccountError::RepositoryUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Account repository is not initialized",
            request_id,
        ),
        StoreImportAccountError::Inspect => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to inspect account",
            request_id,
        ),
        StoreImportAccountError::Invalid(message) => {
            AdminError::new(StatusCode::BAD_REQUEST, 40001, message, request_id)
        }
        StoreImportAccountError::Insert => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to import account",
            request_id,
        ),
    }
}

fn parse_account_import_payload(payload: &Value) -> ParsedAccountImportPayload {
    ParsedAccountImportPayload {
        accounts: parse_native_account_payload(payload),
        source_format: AccountImportFormat::Native,
    }
}

fn parse_native_account_payload(payload: &Value) -> Vec<AccountImportEntry> {
    if !has_only_allowed_keys(payload, &NATIVE_IMPORT_CONTAINER_KEYS) {
        return Vec::new();
    }
    let Some(accounts) = payload.get("accounts").and_then(Value::as_array) else {
        return Vec::new();
    };
    accounts
        .iter()
        .filter_map(account_entry_from_value)
        .collect()
}

fn account_entry_from_value(value: &Value) -> Option<AccountImportEntry> {
    if !has_only_allowed_keys(value, &NATIVE_IMPORT_ACCOUNT_KEYS) {
        return None;
    }

    let entry = AccountImportEntry {
        id: first_string(value, &[&["id"]]),
        email: first_string(value, &[&["email"]]),
        account_id: first_string(value, &[&["accountId"]]),
        user_id: first_string(value, &[&["userId"]]),
        label: label_from_value(value),
        plan_type: first_string(value, &[&["planType"]]),
        token: first_string(value, &[&["token"]]),
        refresh_token: first_string(value, &[&["refreshToken"]]),
        access_token_expires_at: first_string(value, &[&["accessTokenExpiresAt"]]),
        status: first_string(value, &[&["status"]]),
    };
    (entry.token.is_some() || entry.refresh_token.is_some()).then_some(entry)
}

fn has_only_allowed_keys(value: &Value, allowed: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|object| object.keys().all(|key| allowed.contains(&key.as_str())))
}

fn label_from_value(value: &Value) -> Option<String> {
    normalized_label(first_string(value, &[&["label"]]))
}

fn normalized_label(value: Option<String>) -> Option<String> {
    value.map(|label| label.chars().take(64).collect())
}

fn empty_to_none(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
