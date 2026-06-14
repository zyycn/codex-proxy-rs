use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::{
    admin::client_keys::service::{ApiKeyServiceError, ImportedClientApiKey},
    platform::identity::client_key_repository::StoredClientApiKey,
};

use super::AdminError;

pub mod create;
pub mod export;
pub mod import;
pub mod lifecycle;
pub mod list;

pub use create::create_api_key;
pub use export::export_api_keys;
pub use import::import_api_keys;
pub use lifecycle::{
    batch_delete_api_keys, delete_api_key, update_api_key_label, update_api_key_status,
};
pub use list::api_keys;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeysQuery {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyExportQuery {
    pub ids: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateApiKeyRequest {
    pub name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientApiKeyData {
    pub id: String,
    pub name: String,
    pub label: Option<String>,
    pub prefix: String,
    pub enabled: bool,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedClientApiKeyData {
    pub id: String,
    pub name: String,
    pub label: Option<String>,
    pub prefix: String,
    pub enabled: bool,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub plaintext: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateClientApiKeyStatusRequest {
    pub status: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateClientApiKeyLabelRequest {
    pub label: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateClientApiKeyStatusData {
    pub id: String,
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeleteClientApiKeysRequest {
    pub ids: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeleteClientApiKeysData {
    pub deleted: u32,
    pub not_found: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteClientApiKeyData {
    pub deleted: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientApiKeyExportData {
    pub source_format: &'static str,
    pub rotation_required: bool,
    pub api_keys: Vec<ClientApiKeyExportEntry>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientApiKeyExportEntry {
    pub id: String,
    pub name: String,
    pub label: Option<String>,
    pub prefix: String,
    pub enabled: bool,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientApiKeyImportData {
    pub imported: u32,
    pub skipped: u32,
    pub rotated: bool,
    pub keys: Vec<ImportedClientApiKeyData>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedClientApiKeyData {
    pub source_id: Option<String>,
    pub source_prefix: Option<String>,
    pub id: String,
    pub name: String,
    pub label: Option<String>,
    pub prefix: String,
    pub enabled: bool,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub plaintext: String,
}

impl From<StoredClientApiKey> for ClientApiKeyData {
    fn from(key: StoredClientApiKey) -> Self {
        Self {
            id: key.id,
            name: key.name,
            label: key.label,
            prefix: key.prefix,
            enabled: key.enabled,
            created_at: key.created_at,
            last_used_at: key.last_used_at,
        }
    }
}

impl From<StoredClientApiKey> for ClientApiKeyExportEntry {
    fn from(key: StoredClientApiKey) -> Self {
        Self {
            id: key.id,
            name: key.name,
            label: key.label,
            prefix: key.prefix,
            enabled: key.enabled,
            created_at: key.created_at,
            last_used_at: key.last_used_at,
        }
    }
}

impl CreatedClientApiKeyData {
    pub(super) fn new(key: StoredClientApiKey, plaintext: String) -> Self {
        Self {
            id: key.id,
            name: key.name,
            label: key.label,
            prefix: key.prefix,
            enabled: key.enabled,
            created_at: key.created_at,
            last_used_at: key.last_used_at,
            plaintext,
        }
    }
}

impl ImportedClientApiKeyData {
    pub(super) fn new(imported: ImportedClientApiKey) -> Self {
        let ImportedClientApiKey {
            key,
            plaintext,
            source_id,
            source_prefix,
        } = imported;
        Self {
            source_id,
            source_prefix,
            id: key.id,
            name: key.name,
            label: key.label,
            prefix: key.prefix,
            enabled: key.enabled,
            created_at: key.created_at,
            last_used_at: key.last_used_at,
            plaintext,
        }
    }
}

pub(super) fn api_key_service_error(error: ApiKeyServiceError, request_id: String) -> AdminError {
    match error {
        ApiKeyServiceError::RepositoryUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "API key repository is not initialized",
            request_id,
        ),
        ApiKeyServiceError::HasherUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "API key hasher is not initialized",
            request_id,
        ),
        ApiKeyServiceError::List => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to list API keys",
            request_id,
        ),
        ApiKeyServiceError::Export => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to export API keys",
            request_id,
        ),
        ApiKeyServiceError::Import => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to import API key",
            request_id,
        ),
        ApiKeyServiceError::Create => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to create API key",
            request_id,
        ),
        ApiKeyServiceError::Delete => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to delete API key",
            request_id,
        ),
        ApiKeyServiceError::UpdateLabel => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to update API key label",
            request_id,
        ),
        ApiKeyServiceError::UpdateStatus => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to update API key status",
            request_id,
        ),
        ApiKeyServiceError::Verify => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to verify API key",
            request_id,
        ),
        ApiKeyServiceError::InvalidStatus(message) => {
            AdminError::new(StatusCode::BAD_REQUEST, 40001, message, request_id)
        }
        ApiKeyServiceError::EmptyName => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "API key name is required",
            request_id,
        ),
        ApiKeyServiceError::EmptyIds => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "API key ids are required",
            request_id,
        ),
        ApiKeyServiceError::LabelTooLong => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "API key label must be 64 characters or fewer",
            request_id,
        ),
        ApiKeyServiceError::NoImportableKeys => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "No importable API keys found",
            request_id,
        ),
    }
}

pub(super) fn api_key_not_found(request_id: String) -> AdminError {
    AdminError::new(
        StatusCode::NOT_FOUND,
        40401,
        "API key not found",
        request_id,
    )
}
