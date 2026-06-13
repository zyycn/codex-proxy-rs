use serde_json::Value;

use crate::{
    platform::identity::{
        api_key::ApiKeyHasher,
        api_key_repository::{ClientApiKeyRepository, StoredClientApiKey},
    },
    utils::{json::first_string, pagination::Page},
};

#[derive(Clone)]
pub struct ApiKeyService {
    repository: Option<ClientApiKeyRepository>,
    hasher: Option<ApiKeyHasher>,
}

#[derive(Debug)]
pub enum ApiKeyServiceError {
    RepositoryUnavailable,
    HasherUnavailable,
    List,
    Export,
    Import,
    Create,
    Delete,
    UpdateLabel,
    UpdateStatus,
    Verify,
    InvalidStatus(String),
    EmptyName,
    EmptyIds,
    LabelTooLong,
    NoImportableKeys,
}

#[derive(Debug)]
pub struct CreatedClientApiKey {
    pub key: StoredClientApiKey,
    pub plaintext: String,
}

#[derive(Debug)]
pub struct ImportedClientApiKey {
    pub source_id: Option<String>,
    pub source_prefix: Option<String>,
    pub key: StoredClientApiKey,
    pub plaintext: String,
}

#[derive(Debug)]
pub struct ImportedClientApiKeys {
    pub imported: u32,
    pub skipped: u32,
    pub keys: Vec<ImportedClientApiKey>,
}

#[derive(Debug)]
pub struct BatchDeleteClientApiKeys {
    pub deleted: u32,
    pub not_found: Vec<String>,
}

#[derive(Debug)]
pub struct UpdateClientApiKeyStatus {
    pub id: String,
    pub enabled: bool,
}

#[derive(Debug, Clone)]
struct ClientApiKeyImportEntry {
    source_id: Option<String>,
    source_prefix: Option<String>,
    name: String,
    label: Option<String>,
    enabled: bool,
}

impl ApiKeyService {
    pub fn new(repository: Option<ClientApiKeyRepository>, hasher: Option<ApiKeyHasher>) -> Self {
        Self { repository, hasher }
    }

    pub async fn list(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<Page<StoredClientApiKey>, ApiKeyServiceError> {
        self.repository()?
            .list(cursor, limit)
            .await
            .map_err(|_| ApiKeyServiceError::List)
    }

    pub async fn export(
        &self,
        ids: Vec<String>,
    ) -> Result<Vec<StoredClientApiKey>, ApiKeyServiceError> {
        let repo = self.repository()?;
        if ids.is_empty() {
            return repo
                .list_all()
                .await
                .map_err(|_| ApiKeyServiceError::Export);
        }

        let mut keys = Vec::with_capacity(ids.len());
        for id in ids {
            match repo.get(&id).await {
                Ok(Some(key)) => keys.push(key),
                Ok(None) => {}
                Err(_) => return Err(ApiKeyServiceError::Export),
            }
        }
        Ok(keys)
    }

    pub async fn import(
        &self,
        payload: &Value,
    ) -> Result<ImportedClientApiKeys, ApiKeyServiceError> {
        let repo = self.repository()?;
        let hasher = self.hasher()?;
        let entries = parse_client_api_key_import_payload(payload);
        if entries.is_empty() {
            return Err(ApiKeyServiceError::NoImportableKeys);
        }

        let mut imported = 0u32;
        let mut skipped = 0u32;
        let mut keys = Vec::with_capacity(entries.len());
        for entry in entries {
            let name = entry.name.trim();
            if name.is_empty() {
                skipped += 1;
                continue;
            }
            if entry
                .label
                .as_ref()
                .is_some_and(|label| label.chars().count() > 64)
            {
                return Err(ApiKeyServiceError::LabelTooLong);
            }

            let generated = hasher.generate_client_api_key(name);
            let plaintext = generated.plaintext.clone();
            let source_id = entry.source_id;
            let source_prefix = entry.source_prefix;
            match repo
                .insert_generated_with_metadata(
                    name,
                    entry.label.as_deref(),
                    entry.enabled,
                    &generated,
                )
                .await
            {
                Ok(key) => {
                    imported += 1;
                    keys.push(ImportedClientApiKey {
                        source_id,
                        source_prefix,
                        key,
                        plaintext,
                    });
                }
                Err(_) => return Err(ApiKeyServiceError::Import),
            }
        }

        Ok(ImportedClientApiKeys {
            imported,
            skipped,
            keys,
        })
    }

    pub async fn create(&self, name: &str) -> Result<CreatedClientApiKey, ApiKeyServiceError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(ApiKeyServiceError::EmptyName);
        }
        let repo = self.repository()?;
        let hasher = self.hasher()?;
        let generated = hasher.generate_client_api_key(name);
        let plaintext = generated.plaintext.clone();
        repo.insert_generated(name, &generated)
            .await
            .map(|key| CreatedClientApiKey { key, plaintext })
            .map_err(|_| ApiKeyServiceError::Create)
    }

    pub async fn batch_delete(
        &self,
        ids: Vec<String>,
    ) -> Result<BatchDeleteClientApiKeys, ApiKeyServiceError> {
        if ids.is_empty() {
            return Err(ApiKeyServiceError::EmptyIds);
        }
        let repo = self.repository()?;
        let mut deleted = 0u32;
        let mut not_found = Vec::new();
        for key_id in ids {
            match repo.delete(&key_id).await {
                Ok(true) => deleted += 1,
                Ok(false) => not_found.push(key_id),
                Err(_) => return Err(ApiKeyServiceError::Delete),
            }
        }
        Ok(BatchDeleteClientApiKeys { deleted, not_found })
    }

    pub async fn update_label(
        &self,
        key_id: &str,
        label: Option<String>,
    ) -> Result<Option<StoredClientApiKey>, ApiKeyServiceError> {
        if label
            .as_ref()
            .is_some_and(|label| label.chars().count() > 64)
        {
            return Err(ApiKeyServiceError::LabelTooLong);
        }
        self.repository()?
            .set_label(key_id, label)
            .await
            .map_err(|_| ApiKeyServiceError::UpdateLabel)
    }

    pub async fn update_status(
        &self,
        key_id: String,
        status: &str,
    ) -> Result<Option<UpdateClientApiKeyStatus>, ApiKeyServiceError> {
        let enabled = parse_client_api_key_enabled_status(status)?;
        match self.repository()?.set_enabled(&key_id, enabled).await {
            Ok(true) => Ok(Some(UpdateClientApiKeyStatus {
                id: key_id,
                enabled,
            })),
            Ok(false) => Ok(None),
            Err(_) => Err(ApiKeyServiceError::UpdateStatus),
        }
    }

    pub async fn delete(&self, key_id: &str) -> Result<bool, ApiKeyServiceError> {
        self.repository()?
            .delete(key_id)
            .await
            .map_err(|_| ApiKeyServiceError::Delete)
    }

    pub async fn verify(&self, plaintext: &str) -> Result<bool, ApiKeyServiceError> {
        let repo = self.repository()?;
        let hasher = self.hasher()?;
        repo.verify_and_touch(plaintext, hasher)
            .await
            .map_err(|_| ApiKeyServiceError::Verify)
    }

    fn repository(&self) -> Result<&ClientApiKeyRepository, ApiKeyServiceError> {
        self.repository
            .as_ref()
            .ok_or(ApiKeyServiceError::RepositoryUnavailable)
    }

    fn hasher(&self) -> Result<&ApiKeyHasher, ApiKeyServiceError> {
        self.hasher
            .as_ref()
            .ok_or(ApiKeyServiceError::HasherUnavailable)
    }
}

fn parse_client_api_key_import_payload(payload: &Value) -> Vec<ClientApiKeyImportEntry> {
    let payload = payload
        .get("data")
        .filter(|data| data.get("apiKeys").is_some() || data.get("keys").is_some())
        .unwrap_or(payload);

    if let Some(keys) = payload.get("apiKeys").and_then(Value::as_array) {
        return keys
            .iter()
            .filter_map(client_api_key_import_entry_from_value)
            .collect();
    }
    if let Some(keys) = payload.get("keys").and_then(Value::as_array) {
        return keys
            .iter()
            .filter_map(client_api_key_import_entry_from_value)
            .collect();
    }
    if let Some(keys) = payload.as_array() {
        return keys
            .iter()
            .filter_map(client_api_key_import_entry_from_value)
            .collect();
    }

    client_api_key_import_entry_from_value(payload)
        .into_iter()
        .collect()
}

fn client_api_key_import_entry_from_value(value: &Value) -> Option<ClientApiKeyImportEntry> {
    value.as_object()?;
    let name = first_string(value, &[&["name"]])?;
    Some(ClientApiKeyImportEntry {
        source_id: first_string(value, &[&["id"], &["sourceId"]]),
        source_prefix: first_string(value, &[&["prefix"], &["sourcePrefix"]]),
        name,
        label: first_string(value, &[&["label"]]),
        enabled: client_api_key_import_enabled(value),
    })
}

fn client_api_key_import_enabled(value: &Value) -> bool {
    if let Some(enabled) = value.get("enabled").and_then(Value::as_bool) {
        return enabled;
    }
    !first_string(value, &[&["status"]])
        .unwrap_or_else(|| "active".to_string())
        .trim()
        .eq_ignore_ascii_case("disabled")
}

fn parse_client_api_key_enabled_status(status: &str) -> Result<bool, ApiKeyServiceError> {
    match status.trim().to_ascii_lowercase().as_str() {
        "active" => Ok(true),
        "disabled" => Ok(false),
        other => Err(ApiKeyServiceError::InvalidStatus(format!(
            "Unsupported API key status: {other}"
        ))),
    }
}
