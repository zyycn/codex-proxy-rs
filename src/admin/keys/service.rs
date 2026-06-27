//! 客户端 API Key 端口定义、业务服务与 SQLite 存储适配器。

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use sqlx::{Row, SqlitePool};
use thiserror::Error;
use uuid::Uuid;

use crate::infra::{
    identity::generate_client_api_key,
    json::{decode_cursor, encode_cursor, Page},
};

// ---------------------------------------------------------------------------
// 端口定义
// ---------------------------------------------------------------------------

/// 客户端 API Key 存储错误。
#[derive(Debug, Error)]
pub enum ClientKeyStoreError {
    /// 底层存储失败。
    #[error("client key store operation failed: {message}")]
    OperationFailed {
        /// 错误说明。
        message: String,
    },
}

/// 客户端 API Key 存储结果类型。
pub type ClientKeyStoreResult<T> = Result<T, ClientKeyStoreError>;

/// 提供客户端 API Key 验证能力的端口。
#[async_trait]
pub trait ClientKeyStore: Send + Sync + 'static {
    /// 验证明文客户端 API Key，并在成功时记录使用时间。
    async fn verify_and_touch(&self, plaintext: &str) -> ClientKeyStoreResult<bool>;
}

// ---------------------------------------------------------------------------
// 业务服务
// ---------------------------------------------------------------------------

/// 客户端 API Key 服务。
#[derive(Clone)]
pub struct ClientKeyService {
    store: Arc<dyn ClientKeyStore>,
}

impl ClientKeyService {
    /// 构造服务。
    pub fn new(store: Arc<dyn ClientKeyStore>) -> Self {
        Self { store }
    }

    /// 验证客户端 API Key。
    pub async fn verify(&self, plaintext: &str) -> ClientKeyStoreResult<bool> {
        if !plaintext.starts_with("sk_") {
            return Ok(false);
        }
        self.store.verify_and_touch(plaintext).await
    }
}

// ---------------------------------------------------------------------------
// 管理端业务服务
// ---------------------------------------------------------------------------

/// 管理端客户端 API Key 服务。
#[derive(Clone)]
pub struct AdminClientKeyService {
    store: SqliteClientKeyStore,
}

impl AdminClientKeyService {
    /// 构造服务。
    pub fn new(store: SqliteClientKeyStore) -> Self {
        Self { store }
    }

    /// 创建客户端 API Key。
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

    /// 按 ID 读取客户端 API Key 元数据。
    pub async fn get(
        &self,
        key_id: &str,
    ) -> Result<Option<AdminStoredClientApiKey>, AdminClientKeyError> {
        self.store
            .get(key_id)
            .await
            .map(|key| key.map(AdminStoredClientApiKey::from))
            .map_err(|_| AdminClientKeyError::List)
    }

    /// 更新客户端 API Key 标签。
    pub async fn update_label(
        &self,
        key_id: &str,
        label: Option<String>,
    ) -> Result<Option<AdminStoredClientApiKey>, AdminClientKeyError> {
        if label.as_ref().is_some_and(|l| l.chars().count() > 64) {
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

    /// 导出客户端 API Key 元数据。
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
}

/// 管理端可见的客户端 API Key 元数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminStoredClientApiKey {
    pub id: String,
    pub name: String,
    pub label: Option<String>,
    pub prefix: String,
    pub key: String,
    pub enabled: bool,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

/// 管理端创建客户端 API Key 的一次性返回。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminCreatedClientApiKey {
    pub id: String,
    pub name: String,
    pub label: Option<String>,
    pub prefix: String,
    pub key: String,
    pub enabled: bool,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

/// 客户端 API Key 状态更新结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatedClientApiKeyStatus {
    pub id: String,
    pub enabled: bool,
}

/// 客户端 API Key 批量删除结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchDeleteClientApiKeys {
    pub deleted: u32,
    pub not_found: Vec<String>,
}

/// 管理端客户端 API Key 错误。
#[derive(Debug, Error)]
pub enum AdminClientKeyError {
    #[error("failed to list client API keys")]
    List,
    #[error("failed to export client API keys")]
    Export,
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

fn parse_client_key_status(status: &str) -> Result<bool, AdminClientKeyError> {
    match status.trim().to_ascii_lowercase().as_str() {
        "active" => Ok(true),
        "disabled" => Ok(false),
        other => Err(AdminClientKeyError::InvalidStatus(other.to_string())),
    }
}

impl From<StoredClientApiKey> for AdminStoredClientApiKey {
    fn from(key: StoredClientApiKey) -> Self {
        Self {
            id: key.id,
            name: key.name,
            label: key.label,
            prefix: key.prefix,
            key: key.key,
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
            key: key.key,
            enabled: key.enabled,
            created_at: key.created_at,
            last_used_at: key.last_used_at,
        }
    }
}

// ---------------------------------------------------------------------------
// SQLite 存储适配器
// ---------------------------------------------------------------------------

/// SQLite 客户端 API Key 存储错误。
#[derive(Debug, Error)]
pub enum SqliteClientKeyStoreError {
    /// 数据库错误。
    #[error("sqlite client key database error: {0}")]
    Database(#[from] sqlx::Error),
    /// 分页游标无效。
    #[error("invalid client key pagination cursor")]
    InvalidCursor,
}

/// 已持久化的客户端 API Key 元数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredClientApiKey {
    /// API Key 记录 ID。
    pub id: String,
    /// API Key 名称。
    pub name: String,
    /// 管理员可见标签。
    pub label: Option<String>,
    /// 明文 API Key 的短前缀。
    pub prefix: String,
    /// 管理端可复制的完整 API Key。
    pub key: String,
    /// 是否允许用于 `/v1` 认证。
    pub enabled: bool,
    /// 创建时间。
    pub created_at: String,
    /// 最近一次成功使用时间。
    pub last_used_at: Option<String>,
}

/// 新建客户端 API Key 后的一次性结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedClientApiKey {
    /// API Key 记录 ID。
    pub id: String,
    /// API Key 名称。
    pub name: String,
    /// 管理员可见标签。
    pub label: Option<String>,
    /// 明文 API Key 的短前缀。
    pub prefix: String,
    /// 管理端可复制的完整 API Key。
    pub key: String,
    /// 是否允许用于 `/v1` 认证。
    pub enabled: bool,
    /// 创建时间。
    pub created_at: String,
    /// 最近一次成功使用时间。
    pub last_used_at: Option<String>,
}

/// SQLite 客户端 API Key 存储。
#[derive(Clone)]
pub struct SqliteClientKeyStore {
    pool: SqlitePool,
}

impl SqliteClientKeyStore {
    /// 构造存储适配器。
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 暴露底层连接池，供集成测试和运行时组合层复用。
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// 创建新的本地客户端 API Key。
    pub async fn create(
        &self,
        name: &str,
    ) -> Result<CreatedClientApiKey, SqliteClientKeyStoreError> {
        let generated = generate_client_api_key();
        let id = format!("key_{}", Uuid::new_v4().simple());
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "insert into client_api_keys (id, name, label, prefix, key, enabled, created_at, last_used_at) values (?, ?, null, ?, ?, 1, ?, null)",
        )
        .bind(&id)
        .bind(name)
        .bind(&generated.prefix)
        .bind(&generated.key)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        Ok(CreatedClientApiKey {
            id,
            name: name.to_string(),
            label: None,
            prefix: generated.prefix,
            key: generated.key,
            enabled: true,
            created_at: now,
            last_used_at: None,
        })
    }

    /// 按创建时间倒序分页列出客户端 API Key。
    pub async fn list(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<Page<StoredClientApiKey>, SqliteClientKeyStoreError> {
        let fetch_limit = i64::from(limit) + 1;
        let rows = if let Some(cursor) = cursor {
            let (created_at, id) =
                decode_cursor(&cursor).ok_or(SqliteClientKeyStoreError::InvalidCursor)?;
            sqlx::query(
                "select id, name, label, prefix, key, enabled, created_at, last_used_at from client_api_keys where created_at < ? or (created_at = ? and id < ?) order by created_at desc, id desc limit ?",
            )
            .bind(&created_at)
            .bind(created_at)
            .bind(id)
            .bind(fetch_limit)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                "select id, name, label, prefix, key, enabled, created_at, last_used_at from client_api_keys order by created_at desc, id desc limit ?",
            )
            .bind(fetch_limit)
            .fetch_all(&self.pool)
            .await?
        };

        let has_next = rows.len() > limit as usize;
        let take_count = rows.len().min(limit as usize);
        let items = rows
            .into_iter()
            .take(take_count)
            .map(|row| self.key_from_row(&row))
            .collect::<Result<Vec<_>, _>>()?;
        let next_cursor = if has_next {
            items
                .last()
                .map(|key| encode_cursor(&key.created_at, &key.id))
        } else {
            None
        };

        Ok(Page { items, next_cursor })
    }

    /// 按 ID 读取客户端 API Key 元数据。
    pub async fn get(
        &self,
        id: &str,
    ) -> Result<Option<StoredClientApiKey>, SqliteClientKeyStoreError> {
        let row = sqlx::query(
            "select id, name, label, prefix, key, enabled, created_at, last_used_at from client_api_keys where id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        row.map(|row| self.key_from_row(&row)).transpose()
    }

    /// 更新客户端 API Key 启用状态。
    pub async fn set_enabled(
        &self,
        id: &str,
        enabled: bool,
    ) -> Result<bool, SqliteClientKeyStoreError> {
        let result = sqlx::query("update client_api_keys set enabled = ? where id = ?")
            .bind(if enabled { 1_i64 } else { 0_i64 })
            .bind(id)
            .execute(&self.pool)
            .await?;

        Ok(result.rows_affected() > 0)
    }

    /// 更新客户端 API Key 标签并返回最新元数据。
    pub async fn set_label(
        &self,
        id: &str,
        label: Option<String>,
    ) -> Result<Option<StoredClientApiKey>, SqliteClientKeyStoreError> {
        let result = sqlx::query("update client_api_keys set label = ? where id = ?")
            .bind(label)
            .bind(id)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Ok(None);
        }

        self.get(id).await
    }

    /// 删除客户端 API Key。
    pub async fn delete(&self, id: &str) -> Result<bool, SqliteClientKeyStoreError> {
        let result = sqlx::query("delete from client_api_keys where id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;

        Ok(result.rows_affected() > 0)
    }

    fn key_from_row(
        &self,
        row: &sqlx::sqlite::SqliteRow,
    ) -> Result<StoredClientApiKey, SqliteClientKeyStoreError> {
        Ok(StoredClientApiKey {
            id: row.get("id"),
            name: row.get("name"),
            label: row.get("label"),
            prefix: row.get("prefix"),
            key: row.get("key"),
            enabled: row.get::<i64, _>("enabled") != 0,
            created_at: row.get("created_at"),
            last_used_at: row.get("last_used_at"),
        })
    }
}

#[async_trait]
impl ClientKeyStore for SqliteClientKeyStore {
    async fn verify_and_touch(&self, plaintext: &str) -> ClientKeyStoreResult<bool> {
        verify_and_touch(&self.pool, plaintext)
            .await
            .map_err(map_client_key_store_error)
    }
}

async fn verify_and_touch(
    pool: &SqlitePool,
    plaintext: &str,
) -> Result<bool, SqliteClientKeyStoreError> {
    let row = sqlx::query("select id from client_api_keys where key = ? and enabled = 1")
        .bind(plaintext)
        .fetch_optional(pool)
        .await?;

    if let Some(row) = row {
        let id = row.get::<String, _>("id");
        sqlx::query("update client_api_keys set last_used_at = ? where id = ?")
            .bind(Utc::now().to_rfc3339())
            .bind(id)
            .execute(pool)
            .await?;
        return Ok(true);
    }

    Ok(false)
}

fn map_client_key_store_error(error: SqliteClientKeyStoreError) -> ClientKeyStoreError {
    ClientKeyStoreError::OperationFailed {
        message: error.to_string(),
    }
}
