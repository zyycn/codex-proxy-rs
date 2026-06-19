//! 客户端 key 处理器。

use axum::http::StatusCode;
use codex_proxy_runtime::services::{
    AdminClientKeyError, AdminCreatedClientApiKey, AdminStoredClientApiKey, ImportedClientApiKey,
};
use serde::{Deserialize, Serialize};

use crate::admin_api::AdminError;

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

/// API Key 列表查询。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeysQuery {
    /// 分页游标。
    pub cursor: Option<String>,
    /// 分页大小。
    pub limit: Option<u32>,
}

/// API Key 导出查询。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyExportQuery {
    /// 逗号分隔的 ID 列表。
    pub ids: Option<String>,
}

/// 创建 API Key 请求。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateApiKeyRequest {
    /// API Key 名称。
    pub name: String,
}

/// 客户端 API Key 元数据。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientApiKeyData {
    /// API Key 记录 ID。
    pub id: String,
    /// 名称。
    pub name: String,
    /// 标签。
    pub label: Option<String>,
    /// 前缀。
    pub prefix: String,
    /// 是否启用。
    pub enabled: bool,
    /// 创建时间。
    pub created_at: String,
    /// 最近使用时间。
    pub last_used_at: Option<String>,
}

/// 创建 API Key 响应数据。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedClientApiKeyData {
    /// API Key 记录 ID。
    pub id: String,
    /// 名称。
    pub name: String,
    /// 标签。
    pub label: Option<String>,
    /// 前缀。
    pub prefix: String,
    /// 是否启用。
    pub enabled: bool,
    /// 创建时间。
    pub created_at: String,
    /// 最近使用时间。
    pub last_used_at: Option<String>,
    /// 仅返回一次的明文。
    pub plaintext: String,
}

/// 更新状态请求。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateClientApiKeyStatusRequest {
    /// `active` 或 `disabled`。
    pub status: String,
}

/// 更新标签请求。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateClientApiKeyLabelRequest {
    /// 新标签。
    pub label: Option<String>,
}

/// 更新状态响应数据。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateClientApiKeyStatusData {
    /// API Key 记录 ID。
    pub id: String,
    /// 是否启用。
    pub enabled: bool,
}

/// 批量删除请求。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeleteClientApiKeysRequest {
    /// 待删除 ID。
    pub ids: Vec<String>,
}

/// 批量删除响应。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeleteClientApiKeysData {
    /// 成功删除数量。
    pub deleted: u32,
    /// 未找到 ID。
    pub not_found: Vec<String>,
}

/// 删除响应。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteClientApiKeyData {
    /// 是否删除。
    pub deleted: bool,
}

/// API Key 导出响应。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientApiKeyExportData {
    /// 来源格式。
    pub source_format: &'static str,
    /// 是否需要轮换。
    pub rotation_required: bool,
    /// 导出的 API Key 元数据。
    pub api_keys: Vec<ClientApiKeyExportEntry>,
}

/// API Key 导出条目。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientApiKeyExportEntry {
    /// API Key 记录 ID。
    pub id: String,
    /// 名称。
    pub name: String,
    /// 标签。
    pub label: Option<String>,
    /// 前缀。
    pub prefix: String,
    /// 是否启用。
    pub enabled: bool,
    /// 创建时间。
    pub created_at: String,
    /// 最近使用时间。
    pub last_used_at: Option<String>,
}

/// API Key 导入响应。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientApiKeyImportData {
    /// 导入数量。
    pub imported: u32,
    /// 跳过数量。
    pub skipped: u32,
    /// 是否轮换。
    pub rotated: bool,
    /// 新建 key 列表。
    pub keys: Vec<ImportedClientApiKeyData>,
}

/// API Key 导入条目。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedClientApiKeyData {
    /// 来源 ID。
    pub source_id: Option<String>,
    /// 来源前缀。
    pub source_prefix: Option<String>,
    /// API Key 记录 ID。
    pub id: String,
    /// 名称。
    pub name: String,
    /// 标签。
    pub label: Option<String>,
    /// 前缀。
    pub prefix: String,
    /// 是否启用。
    pub enabled: bool,
    /// 创建时间。
    pub created_at: String,
    /// 最近使用时间。
    pub last_used_at: Option<String>,
    /// 仅返回一次的新明文。
    pub plaintext: String,
}

impl From<AdminStoredClientApiKey> for ClientApiKeyData {
    fn from(key: AdminStoredClientApiKey) -> Self {
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

impl From<AdminStoredClientApiKey> for ClientApiKeyExportEntry {
    fn from(key: AdminStoredClientApiKey) -> Self {
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

impl From<AdminCreatedClientApiKey> for CreatedClientApiKeyData {
    fn from(key: AdminCreatedClientApiKey) -> Self {
        Self {
            id: key.id,
            name: key.name,
            label: key.label,
            prefix: key.prefix,
            enabled: key.enabled,
            created_at: key.created_at,
            last_used_at: key.last_used_at,
            plaintext: key.plaintext,
        }
    }
}

impl ImportedClientApiKeyData {
    fn new(imported: ImportedClientApiKey) -> Self {
        Self {
            source_id: imported.source_id,
            source_prefix: imported.source_prefix,
            id: imported.key.id,
            name: imported.key.name,
            label: imported.key.label,
            prefix: imported.key.prefix,
            enabled: imported.key.enabled,
            created_at: imported.key.created_at,
            last_used_at: imported.key.last_used_at,
            plaintext: imported.key.plaintext,
        }
    }
}

fn client_key_error(error: AdminClientKeyError, request_id: String) -> AdminError {
    match error {
        AdminClientKeyError::InvalidStatus(_)
        | AdminClientKeyError::EmptyName
        | AdminClientKeyError::EmptyIds
        | AdminClientKeyError::LabelTooLong
        | AdminClientKeyError::NoImportableKeys => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            error.to_string(),
            request_id,
        ),
        AdminClientKeyError::List
        | AdminClientKeyError::Export
        | AdminClientKeyError::Import
        | AdminClientKeyError::Create
        | AdminClientKeyError::Delete
        | AdminClientKeyError::UpdateLabel
        | AdminClientKeyError::UpdateStatus => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            error.to_string(),
            request_id,
        ),
    }
}

fn client_key_not_found(request_id: String) -> AdminError {
    AdminError::new(
        StatusCode::NOT_FOUND,
        40401,
        "Client API key not found",
        request_id,
    )
}

fn imported_client_key_data(imported: ImportedClientApiKey) -> ImportedClientApiKeyData {
    ImportedClientApiKeyData::new(imported)
}
