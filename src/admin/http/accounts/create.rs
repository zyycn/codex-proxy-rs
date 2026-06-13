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
    platform::http::middleware::RequestId,
    runtime::state::AppState,
    utils::json::{first_string, string_at},
};

use super::super::{require_admin_session, AdminEnvelope, AdminError, AdminResponse};
use super::{admin_account_data_from_stored, validated_account_import_error};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountImportFormat {
    Native,
    Sub2Api,
    CodexCli,
}

impl AccountImportFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Sub2Api => "sub2api",
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateAccountRequest {
    pub token: Option<String>,
    pub refresh_token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportCliAuthRequest {
    pub codex_home: Option<String>,
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
    if let Some(accounts) = parse_sub2api_oauth_payload(payload) {
        return ParsedAccountImportPayload {
            accounts,
            source_format: AccountImportFormat::Sub2Api,
        };
    }

    parse_native_account_payload(payload)
}

fn parse_sub2api_oauth_payload(payload: &Value) -> Option<Vec<AccountImportEntry>> {
    let accounts = payload.get("accounts")?.as_array()?;
    let looks_like_sub2api = string_at(payload, &["type"]).as_deref() == Some("sub2api-data")
        || payload.get("proxies").is_some()
        || accounts
            .iter()
            .any(|account| account.get("credentials").is_some());
    if !looks_like_sub2api {
        return None;
    }

    Some(
        accounts
            .iter()
            .filter_map(sub2api_oauth_account_entry)
            .collect(),
    )
}

fn sub2api_oauth_account_entry(account: &Value) -> Option<AccountImportEntry> {
    let platform = string_at(account, &["platform"])?.to_ascii_lowercase();
    let account_type = string_at(account, &["type"])?.to_ascii_lowercase();
    if platform != "openai" || account_type != "oauth" {
        return None;
    }
    let credentials = account.get("credentials")?;
    let fallback_label = normalized_label(string_at(account, &["name"]));
    let mut entry = account_entry_from_value(credentials, fallback_label);
    if entry.token.is_none() && entry.refresh_token.is_none() {
        return None;
    }
    if entry.email.is_none() {
        entry.email = string_at(credentials, &["email"]);
    }
    if entry.account_id.is_none() {
        entry.account_id = first_string(
            credentials,
            &[&["chatgpt_account_id"], &["account_id"], &["accountId"]],
        );
    }
    if entry.user_id.is_none() {
        entry.user_id = first_string(
            credentials,
            &[&["chatgpt_user_id"], &["user_id"], &["userId"]],
        );
    }
    if entry.plan_type.is_none() {
        entry.plan_type = first_string(credentials, &[&["plan_type"], &["planType"]]);
    }
    Some(entry)
}

fn parse_native_account_payload(payload: &Value) -> ParsedAccountImportPayload {
    if let Some(accounts) = payload.as_array() {
        return ParsedAccountImportPayload {
            accounts: accounts
                .iter()
                .filter_map(|account| {
                    let entry = account_entry_from_value(account, None);
                    (entry.token.is_some() || entry.refresh_token.is_some()).then_some(entry)
                })
                .collect(),
            source_format: AccountImportFormat::Native,
        };
    }

    if let Some(accounts) = payload.get("accounts").and_then(Value::as_array) {
        let source_format = if looks_like_sub2api_native_export(accounts) {
            AccountImportFormat::Sub2Api
        } else {
            AccountImportFormat::Native
        };
        return ParsedAccountImportPayload {
            accounts: accounts
                .iter()
                .filter_map(|account| {
                    let entry = account_entry_from_value(account, None);
                    (entry.token.is_some() || entry.refresh_token.is_some()).then_some(entry)
                })
                .collect(),
            source_format,
        };
    }

    let entry = account_entry_from_value(payload, None);
    let accounts = if entry.token.is_some() || entry.refresh_token.is_some() {
        vec![entry]
    } else {
        Vec::new()
    };
    ParsedAccountImportPayload {
        accounts,
        source_format: AccountImportFormat::Native,
    }
}

fn looks_like_sub2api_native_export(accounts: &[Value]) -> bool {
    accounts.iter().any(|account| {
        // sub2api 兼容导出会携带代理/配额运行态字段；这里只用于格式识别，代理数据不进入 Rust 服务。
        account.get("proxyApiKey").is_some()
            || account.get("cachedQuota").is_some()
            || account.get("quotaVerifyRequired").is_some()
    })
}

fn account_entry_from_value(value: &Value, fallback_label: Option<String>) -> AccountImportEntry {
    let token = first_string(
        value,
        &[
            &["token"],
            &["accessToken"],
            &["access_token"],
            &["tokens", "accessToken"],
            &["tokens", "access_token"],
            &["credentials", "token"],
            &["credentials", "accessToken"],
            &["credentials", "access_token"],
        ],
    )
    .map(normalize_bearer_token);
    let refresh_token = first_string(
        value,
        &[
            &["refreshToken"],
            &["refresh_token"],
            &["tokens", "refreshToken"],
            &["tokens", "refresh_token"],
            &["credentials", "refreshToken"],
            &["credentials", "refresh_token"],
        ],
    );

    AccountImportEntry {
        id: first_string(value, &[&["id"]]),
        email: first_string(value, &[&["email"], &["credentials", "email"]]),
        account_id: first_string(
            value,
            &[
                &["accountId"],
                &["account_id"],
                &["chatgpt_account_id"],
                &["credentials", "accountId"],
                &["credentials", "account_id"],
                &["credentials", "chatgpt_account_id"],
            ],
        ),
        user_id: first_string(
            value,
            &[
                &["userId"],
                &["user_id"],
                &["chatgpt_user_id"],
                &["credentials", "userId"],
                &["credentials", "user_id"],
                &["credentials", "chatgpt_user_id"],
            ],
        ),
        label: label_from_value(value).or(fallback_label),
        plan_type: first_string(
            value,
            &[
                &["planType"],
                &["plan_type"],
                &["credentials", "planType"],
                &["credentials", "plan_type"],
            ],
        ),
        token,
        refresh_token,
        access_token_expires_at: first_string(
            value,
            &[
                &["accessTokenExpiresAt"],
                &["access_token_expires_at"],
                &["credentials", "accessTokenExpiresAt"],
                &["credentials", "access_token_expires_at"],
            ],
        ),
        status: first_string(value, &[&["status"]]),
    }
}

fn label_from_value(value: &Value) -> Option<String> {
    normalized_label(first_string(
        value,
        &[
            &["label"],
            &["name"],
            &["account_name"],
            &["accountName"],
            &["account_note"],
            &["accountNote"],
            &["note"],
        ],
    ))
}

fn normalized_label(value: Option<String>) -> Option<String> {
    value.map(|label| label.chars().take(64).collect())
}

fn normalize_bearer_token(value: String) -> String {
    let trimmed = value.trim();
    trimmed
        .strip_prefix("Bearer ")
        .or_else(|| trimmed.strip_prefix("bearer "))
        .unwrap_or(trimmed)
        .trim()
        .to_string()
}

fn empty_to_none(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
