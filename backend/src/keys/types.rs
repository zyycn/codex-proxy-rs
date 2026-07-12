//! 客户端 API Key 领域类型。

use thiserror::Error;

use crate::infra::json::SortDirection;

use super::store::StoredClientApiKey;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientApiKeySortField {
    Name,
    Enabled,
    CreatedAt,
    LastUsedAt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientApiKeyListSort {
    pub field: ClientApiKeySortField,
    pub direction: SortDirection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedClientApiKey {
    pub id: String,
    pub name: String,
    pub label: Option<String>,
    pub prefix: String,
    pub key: String,
    pub enabled: bool,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchDeleteClientApiKeys {
    pub deleted: u32,
    pub not_found: Vec<String>,
}

#[derive(Debug, Error)]
pub enum KeyManageError {
    #[error("failed to list client API keys")]
    List,
    #[error("failed to create client API key")]
    Create,
    #[error("failed to delete client API key")]
    Delete,
    #[error("failed to update client API key label")]
    UpdateLabel,
    #[error("failed to update client API key status")]
    UpdateStatus,
    #[error("unsupported client API key status: {0}")]
    InvalidStatus(String),
    #[error("client API key name is required")]
    EmptyName,
    #[error("client API key ids are required")]
    EmptyIds,
    #[error("client API key label must be 64 characters or fewer")]
    LabelTooLong,
}

pub(super) fn parse_client_key_status(status: &str) -> Result<bool, KeyManageError> {
    match status.trim().to_ascii_lowercase().as_str() {
        "active" => Ok(true),
        "disabled" => Ok(false),
        other => Err(KeyManageError::InvalidStatus(other.to_string())),
    }
}

impl From<StoredClientApiKey> for ManagedClientApiKey {
    fn from(key: StoredClientApiKey) -> Self {
        Self {
            id: key.id,
            name: key.name,
            label: key.label,
            prefix: key.prefix,
            key: key.key,
            enabled: key.enabled,
            created_at: key.created_at.to_rfc3339(),
            last_used_at: key.last_used_at.map(|value| value.to_rfc3339()),
        }
    }
}
