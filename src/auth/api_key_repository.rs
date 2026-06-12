use chrono::Utc;
use sqlx::{Row, SqlitePool};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    auth::api_key::{ApiKeyHasher, GeneratedClientApiKey},
    pagination::{decode_cursor, encode_cursor, Page},
};

#[derive(Debug, Error)]
pub enum ClientApiKeyRepositoryError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("api key verification error: {0}")]
    Auth(#[from] crate::auth::error::AuthError),
    #[error("invalid pagination cursor")]
    InvalidCursor,
}

pub type ClientApiKeyRepositoryResult<T> = Result<T, ClientApiKeyRepositoryError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredClientApiKey {
    pub id: String,
    pub name: String,
    pub prefix: String,
    pub enabled: bool,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

#[derive(Clone)]
pub struct ClientApiKeyRepository {
    pool: SqlitePool,
}

impl ClientApiKeyRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn insert_generated(
        &self,
        name: &str,
        generated: &GeneratedClientApiKey,
    ) -> ClientApiKeyRepositoryResult<StoredClientApiKey> {
        let id = format!("key_{}", Uuid::new_v4().simple());
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "insert into client_api_keys (id, name, prefix, key_hash, enabled, created_at, last_used_at) values (?, ?, ?, ?, 1, ?, null)",
        )
        .bind(&id)
        .bind(name)
        .bind(&generated.prefix)
        .bind(&generated.key_hash)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(StoredClientApiKey {
            id,
            name: name.to_string(),
            prefix: generated.prefix.clone(),
            enabled: true,
            created_at: now,
            last_used_at: None,
        })
    }

    pub async fn list(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> ClientApiKeyRepositoryResult<Page<StoredClientApiKey>> {
        let fetch_limit = i64::from(limit) + 1;
        let rows = if let Some(cursor) = cursor {
            let (created_at, id) =
                decode_cursor(&cursor).ok_or(ClientApiKeyRepositoryError::InvalidCursor)?;
            sqlx::query(
                "select id, name, prefix, enabled, created_at, last_used_at from client_api_keys where created_at < ? or (created_at = ? and id < ?) order by created_at desc, id desc limit ?",
            )
            .bind(&created_at)
            .bind(created_at)
            .bind(id)
            .bind(fetch_limit)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                "select id, name, prefix, enabled, created_at, last_used_at from client_api_keys order by created_at desc, id desc limit ?",
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

    pub async fn verify_and_touch(
        &self,
        plaintext: &str,
        hasher: &ApiKeyHasher,
    ) -> ClientApiKeyRepositoryResult<bool> {
        let prefix = plaintext.chars().take(12).collect::<String>();
        let rows = sqlx::query(
            "select id, key_hash from client_api_keys where prefix = ? and enabled = 1",
        )
        .bind(prefix)
        .fetch_all(&self.pool)
        .await?;
        for row in rows {
            let key_hash = row.get::<String, _>("key_hash");
            if hasher.verify_client_api_key(plaintext, &key_hash)? {
                let id = row.get::<String, _>("id");
                sqlx::query("update client_api_keys set last_used_at = ? where id = ?")
                    .bind(Utc::now().to_rfc3339())
                    .bind(id)
                    .execute(&self.pool)
                    .await?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub async fn set_enabled(&self, id: &str, enabled: bool) -> ClientApiKeyRepositoryResult<bool> {
        let result = sqlx::query("update client_api_keys set enabled = ? where id = ?")
            .bind(if enabled { 1_i64 } else { 0_i64 })
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn delete(&self, id: &str) -> ClientApiKeyRepositoryResult<bool> {
        let result = sqlx::query("delete from client_api_keys where id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }
}

fn key_from_row(row: &sqlx::sqlite::SqliteRow) -> StoredClientApiKey {
    StoredClientApiKey {
        id: row.get("id"),
        name: row.get("name"),
        prefix: row.get("prefix"),
        enabled: row.get::<i64, _>("enabled") != 0,
        created_at: row.get("created_at"),
        last_used_at: row.get("last_used_at"),
    }
}
