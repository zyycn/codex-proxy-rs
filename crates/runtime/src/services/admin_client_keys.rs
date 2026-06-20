use super::*;

/// 管理端客户端 API Key 服务。
#[derive(Clone)]
pub struct AdminClientKeyService {
    store: SqliteClientKeyStore,
}

impl AdminClientKeyService {
    /// 构造管理端客户端 API Key 服务。
    pub fn new(store: SqliteClientKeyStore) -> Self {
        Self { store }
    }

    /// 创建新的客户端 API Key。
    pub async fn create(
        &self,
        name: &str,
    ) -> Result<AdminCreatedClientApiKey, AdminClientKeyError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(AdminClientKeyError::EmptyName);
        }
        self.store
            .create(name)
            .await
            .map(AdminCreatedClientApiKey::from)
            .map_err(|_| AdminClientKeyError::Create)
    }

    /// 分页列出客户端 API Key。
    pub async fn list(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<Page<AdminStoredClientApiKey>, AdminClientKeyError> {
        let page = self
            .store
            .list(cursor, limit)
            .await
            .map_err(|_| AdminClientKeyError::List)?;
        Ok(Page {
            items: page
                .items
                .into_iter()
                .map(AdminStoredClientApiKey::from)
                .collect(),
            next_cursor: page.next_cursor,
        })
    }

    /// 更新客户端 API Key 标签。
    pub async fn update_label(
        &self,
        key_id: &str,
        label: Option<String>,
    ) -> Result<Option<AdminStoredClientApiKey>, AdminClientKeyError> {
        if label
            .as_ref()
            .is_some_and(|label| label.chars().count() > 64)
        {
            return Err(AdminClientKeyError::LabelTooLong);
        }
        self.store
            .set_label(key_id, label)
            .await
            .map(|key| key.map(AdminStoredClientApiKey::from))
            .map_err(|_| AdminClientKeyError::UpdateLabel)
    }

    /// 更新客户端 API Key 启用状态。
    pub async fn update_status(
        &self,
        key_id: &str,
        status: &str,
    ) -> Result<Option<UpdatedClientApiKeyStatus>, AdminClientKeyError> {
        let enabled = parse_client_key_status(status)?;
        match self.store.set_enabled(key_id, enabled).await {
            Ok(true) => Ok(Some(UpdatedClientApiKeyStatus {
                id: key_id.to_string(),
                enabled,
            })),
            Ok(false) => Ok(None),
            Err(_) => Err(AdminClientKeyError::UpdateStatus),
        }
    }

    /// 删除客户端 API Key。
    pub async fn delete(&self, key_id: &str) -> Result<bool, AdminClientKeyError> {
        self.store
            .delete(key_id)
            .await
            .map_err(|_| AdminClientKeyError::Delete)
    }

    /// 批量删除客户端 API Key。
    pub async fn batch_delete(
        &self,
        ids: Vec<String>,
    ) -> Result<BatchDeleteClientApiKeys, AdminClientKeyError> {
        if ids.is_empty() {
            return Err(AdminClientKeyError::EmptyIds);
        }

        let mut deleted = 0u32;
        let mut not_found = Vec::new();
        for id in ids {
            match self.store.delete(&id).await {
                Ok(true) => deleted += 1,
                Ok(false) => not_found.push(id),
                Err(_) => return Err(AdminClientKeyError::Delete),
            }
        }

        Ok(BatchDeleteClientApiKeys { deleted, not_found })
    }

    /// 导出客户端 API Key 元数据，不包含明文和哈希材料。
    pub async fn export(
        &self,
        ids: Vec<String>,
    ) -> Result<Vec<AdminStoredClientApiKey>, AdminClientKeyError> {
        if ids.is_empty() {
            let mut all_keys = Vec::new();
            let mut cursor = None;
            loop {
                let page = self
                    .store
                    .list(cursor, 200)
                    .await
                    .map_err(|_| AdminClientKeyError::Export)?;
                all_keys.extend(page.items.into_iter().map(AdminStoredClientApiKey::from));
                if page.next_cursor.is_none() {
                    return Ok(all_keys);
                }
                cursor = page.next_cursor;
            }
        }

        let mut keys = Vec::with_capacity(ids.len());
        for id in ids {
            match self.store.get(&id).await {
                Ok(Some(key)) => keys.push(AdminStoredClientApiKey::from(key)),
                Ok(None) => {}
                Err(_) => return Err(AdminClientKeyError::Export),
            }
        }
        Ok(keys)
    }

    /// 导入导出的客户端 API Key 元数据，并轮换为新的本地明文。
    pub async fn import(
        &self,
        payload: &Value,
    ) -> Result<ImportedClientApiKeys, AdminClientKeyError> {
        let entries = parse_client_key_import_payload(payload);
        if entries.is_empty() {
            return Err(AdminClientKeyError::NoImportableKeys);
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
                return Err(AdminClientKeyError::LabelTooLong);
            }

            let mut created = self.create(name).await?;
            if entry.label.is_some() || !entry.enabled {
                self.store
                    .set_label(&created.id, entry.label)
                    .await
                    .map_err(|_| AdminClientKeyError::Import)?;
                if !entry.enabled {
                    self.store
                        .set_enabled(&created.id, false)
                        .await
                        .map_err(|_| AdminClientKeyError::Import)?;
                }
                let Some(stored) = self
                    .store
                    .get(&created.id)
                    .await
                    .map_err(|_| AdminClientKeyError::Import)?
                else {
                    return Err(AdminClientKeyError::Import);
                };
                created.label = stored.label;
                created.enabled = stored.enabled;
            }
            imported += 1;
            keys.push(ImportedClientApiKey {
                source_id: entry.source_id,
                source_prefix: entry.source_prefix,
                key: created,
            });
        }

        Ok(ImportedClientApiKeys {
            imported,
            skipped,
            keys,
        })
    }
}

/// 管理端可见的客户端 API Key 元数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminStoredClientApiKey {
    /// API Key 记录 ID。
    pub id: String,
    /// API Key 名称。
    pub name: String,
    /// 管理员可见标签。
    pub label: Option<String>,
    /// 明文 API Key 的短前缀。
    pub prefix: String,
    /// 是否允许用于 `/v1` 认证。
    pub enabled: bool,
    /// 创建时间。
    pub created_at: String,
    /// 最近一次成功使用时间。
    pub last_used_at: Option<String>,
}

/// 新建客户端 API Key 后的一次性结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminCreatedClientApiKey {
    /// API Key 记录 ID。
    pub id: String,
    /// API Key 名称。
    pub name: String,
    /// 管理员可见标签。
    pub label: Option<String>,
    /// 明文 API Key 的短前缀。
    pub prefix: String,
    /// 是否允许用于 `/v1` 认证。
    pub enabled: bool,
    /// 创建时间。
    pub created_at: String,
    /// 最近一次成功使用时间。
    pub last_used_at: Option<String>,
    /// 仅返回一次的明文 API Key。
    pub plaintext: String,
}

/// 客户端 API Key 状态更新结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatedClientApiKeyStatus {
    /// API Key 记录 ID。
    pub id: String,
    /// 是否启用。
    pub enabled: bool,
}

/// 批量删除客户端 API Key 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchDeleteClientApiKeys {
    /// 成功删除数量。
    pub deleted: u32,
    /// 未找到的 ID。
    pub not_found: Vec<String>,
}

/// 导入后的客户端 API Key。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedClientApiKey {
    /// 来源 ID。
    pub source_id: Option<String>,
    /// 来源短前缀。
    pub source_prefix: Option<String>,
    /// 新建的本地 API Key。
    pub key: AdminCreatedClientApiKey,
}

/// 客户端 API Key 导入结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedClientApiKeys {
    /// 成功导入数量。
    pub imported: u32,
    /// 跳过数量。
    pub skipped: u32,
    /// 新建 API Key 列表。
    pub keys: Vec<ImportedClientApiKey>,
}

/// 管理端客户端 API Key 错误。
#[derive(Debug, Error)]
pub enum AdminClientKeyError {
    /// 列表失败。
    #[error("failed to list client API keys")]
    List,
    /// 导出失败。
    #[error("failed to export client API keys")]
    Export,
    /// 导入失败。
    #[error("failed to import client API keys")]
    Import,
    /// 创建失败。
    #[error("failed to create client API key")]
    Create,
    /// 删除失败。
    #[error("failed to delete client API key")]
    Delete,
    /// 更新标签失败。
    #[error("failed to update client API key label")]
    UpdateLabel,
    /// 更新状态失败。
    #[error("failed to update client API key status")]
    UpdateStatus,
    /// 状态值无效。
    #[error("unsupported client API key status: {0}")]
    InvalidStatus(String),
    /// 名称为空。
    #[error("client API key name is required")]
    EmptyName,
    /// ID 列表为空。
    #[error("client API key ids are required")]
    EmptyIds,
    /// 标签过长。
    #[error("client API key label must be 64 characters or fewer")]
    LabelTooLong,
    /// 没有可导入的 API Key。
    #[error("no importable client API keys found")]
    NoImportableKeys,
}

#[derive(Debug, Clone)]
struct ClientApiKeyImportEntry {
    source_id: Option<String>,
    source_prefix: Option<String>,
    name: String,
    label: Option<String>,
    enabled: bool,
}

fn parse_client_key_status(status: &str) -> Result<bool, AdminClientKeyError> {
    match status.trim().to_ascii_lowercase().as_str() {
        "active" => Ok(true),
        "disabled" => Ok(false),
        other => Err(AdminClientKeyError::InvalidStatus(other.to_string())),
    }
}

fn parse_client_key_import_payload(payload: &Value) -> Vec<ClientApiKeyImportEntry> {
    let payload = payload
        .get("data")
        .filter(|data| data.get("apiKeys").is_some() || data.get("keys").is_some())
        .unwrap_or(payload);

    if let Some(keys) = payload.get("apiKeys").and_then(Value::as_array) {
        return keys
            .iter()
            .filter_map(client_key_import_entry_from_value)
            .collect();
    }
    if let Some(keys) = payload.get("keys").and_then(Value::as_array) {
        return keys
            .iter()
            .filter_map(client_key_import_entry_from_value)
            .collect();
    }
    if let Some(keys) = payload.as_array() {
        return keys
            .iter()
            .filter_map(client_key_import_entry_from_value)
            .collect();
    }

    client_key_import_entry_from_value(payload)
        .into_iter()
        .collect()
}

fn client_key_import_entry_from_value(value: &Value) -> Option<ClientApiKeyImportEntry> {
    value.as_object()?;
    let name = first_string(value, &["name"])?;
    Some(ClientApiKeyImportEntry {
        source_id: first_string(value, &["id", "sourceId"]),
        source_prefix: first_string(value, &["prefix", "sourcePrefix"]),
        name,
        label: first_string(value, &["label"]),
        enabled: client_key_import_enabled(value),
    })
}

fn client_key_import_enabled(value: &Value) -> bool {
    if let Some(enabled) = value.get("enabled").and_then(Value::as_bool) {
        return enabled;
    }
    !first_string(value, &["status"])
        .unwrap_or_else(|| "active".to_string())
        .trim()
        .eq_ignore_ascii_case("disabled")
}

fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

impl From<StoredClientApiKey> for AdminStoredClientApiKey {
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

impl From<CreatedClientApiKey> for AdminCreatedClientApiKey {
    fn from(key: CreatedClientApiKey) -> Self {
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
