//! SQLite 客户端 API Key 存储适配器。

use async_trait::async_trait;
use chrono::Utc;
use sqlx::{Row, SqlitePool};
use thiserror::Error;
use uuid::Uuid;

use codex_proxy_core::admin::ports::{ClientKeyStore, ClientKeyStoreError, ClientKeyStoreResult};
use codex_proxy_platform::{
    identity::{ApiKeyHasher, AuthError},
    json::{decode_cursor, encode_cursor, Page},
};

/// SQLite 客户端 API Key 存储错误。
#[derive(Debug, Error)]
pub enum SqliteClientKeyStoreError {
    /// 数据库错误。
    #[error("sqlite client key database error: {0}")]
    Database(#[from] sqlx::Error),
    /// API Key 验证错误。
    #[error("sqlite client key auth error: {0}")]
    Auth(#[from] AuthError),
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
    /// 是否允许用于 `/v1` 认证。
    pub enabled: bool,
    /// 创建时间。
    pub created_at: String,
    /// 最近一次成功使用时间。
    pub last_used_at: Option<String>,
    /// 仅返回一次的明文 API Key。
    pub plaintext: String,
}

/// SQLite 客户端 API Key 存储。
#[derive(Clone)]
pub struct SqliteClientKeyStore {
    pool: SqlitePool,
    hasher: ApiKeyHasher,
}

impl SqliteClientKeyStore {
    /// 构造存储适配器。
    pub fn new(pool: SqlitePool, hasher: ApiKeyHasher) -> Self {
        Self { pool, hasher }
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
        let generated = self.hasher.generate_client_api_key(name);
        let id = format!("key_{}", Uuid::new_v4().simple());
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "insert into client_api_keys (id, name, label, prefix, key_hash, enabled, created_at, last_used_at) values (?, ?, null, ?, ?, 1, ?, null)",
        )
        .bind(&id)
        .bind(name)
        .bind(&generated.prefix)
        .bind(&generated.key_hash)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        Ok(CreatedClientApiKey {
            id,
            name: name.to_string(),
            label: None,
            prefix: generated.prefix,
            enabled: true,
            created_at: now,
            last_used_at: None,
            plaintext: generated.plaintext,
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
                "select id, name, label, prefix, enabled, created_at, last_used_at from client_api_keys where created_at < ? or (created_at = ? and id < ?) order by created_at desc, id desc limit ?",
            )
            .bind(&created_at)
            .bind(created_at)
            .bind(id)
            .bind(fetch_limit)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                "select id, name, label, prefix, enabled, created_at, last_used_at from client_api_keys order by created_at desc, id desc limit ?",
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
            .map(|row| key_from_row(&row))
            .collect::<Vec<_>>();
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
            "select id, name, label, prefix, enabled, created_at, last_used_at from client_api_keys where id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|row| key_from_row(&row)))
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
}

#[async_trait]
impl ClientKeyStore for SqliteClientKeyStore {
    async fn verify_and_touch(&self, plaintext: &str) -> ClientKeyStoreResult<bool> {
        verify_and_touch(&self.pool, &self.hasher, plaintext)
            .await
            .map_err(map_client_key_store_error)
    }
}

async fn verify_and_touch(
    pool: &SqlitePool,
    hasher: &ApiKeyHasher,
    plaintext: &str,
) -> Result<bool, SqliteClientKeyStoreError> {
    let prefix = plaintext.chars().take(12).collect::<String>();
    let rows =
        sqlx::query("select id, key_hash from client_api_keys where prefix = ? and enabled = 1")
            .bind(prefix)
            .fetch_all(pool)
            .await?;

    for row in rows {
        let key_hash = row.get::<String, _>("key_hash");
        if hasher.verify_client_api_key(plaintext, &key_hash)? {
            let id = row.get::<String, _>("id");
            sqlx::query("update client_api_keys set last_used_at = ? where id = ?")
                .bind(Utc::now().to_rfc3339())
                .bind(id)
                .execute(pool)
                .await?;
            return Ok(true);
        }
    }

    Ok(false)
}

fn key_from_row(row: &sqlx::sqlite::SqliteRow) -> StoredClientApiKey {
    StoredClientApiKey {
        id: row.get("id"),
        name: row.get("name"),
        label: row.get("label"),
        prefix: row.get("prefix"),
        enabled: row.get::<i64, _>("enabled") != 0,
        created_at: row.get("created_at"),
        last_used_at: row.get("last_used_at"),
    }
}

fn map_client_key_store_error(error: SqliteClientKeyStoreError) -> ClientKeyStoreError {
    ClientKeyStoreError::OperationFailed {
        message: error.to_string(),
    }
}
