//! 客户端 API Key PostgreSQL 存储。

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use thiserror::Error;
use uuid::Uuid;

use crate::infra::{
    identity::generate_client_api_key,
    json::{decode_cursor, encode_cursor, Page},
    time::parse_rfc3339_utc,
};

#[derive(Debug, Error)]
pub enum PgClientKeyStoreError {
    #[error("PostgreSQL client key operation failed: {0}")]
    Database(#[from] sqlx::Error),
    #[error("invalid client key pagination cursor")]
    InvalidCursor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredClientApiKey {
    pub id: String,
    pub name: String,
    pub label: Option<String>,
    pub prefix: String,
    pub key: String,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Clone)]
pub struct PgClientKeyStore {
    pool: PgPool,
}

impl PgClientKeyStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub(super) async fn find_enabled_id_by_key(
        &self,
        key: &str,
    ) -> Result<Option<String>, PgClientKeyStoreError> {
        Ok(
            sqlx::query_scalar("select id from client_api_keys where key = $1 and enabled")
                .bind(key)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    pub(super) async fn create(
        &self,
        name: &str,
    ) -> Result<StoredClientApiKey, PgClientKeyStoreError> {
        let generated = generate_client_api_key();
        let stored = StoredClientApiKey {
            id: format!("key_{}", Uuid::new_v4().simple()),
            name: name.to_string(),
            label: None,
            prefix: generated.prefix,
            key: generated.key,
            enabled: true,
            created_at: Utc::now(),
            last_used_at: None,
        };
        sqlx::query(
            r"
insert into client_api_keys
  (id, name, label, prefix, key, enabled, created_at, last_used_at)
values ($1, $2, null, $3, $4, true, $5, null)",
        )
        .bind(&stored.id)
        .bind(&stored.name)
        .bind(&stored.prefix)
        .bind(&stored.key)
        .bind(stored.created_at)
        .execute(&self.pool)
        .await?;
        Ok(stored)
    }

    pub async fn list(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<Page<StoredClientApiKey>, PgClientKeyStoreError> {
        let fetch_limit = i64::from(limit) + 1;
        let rows = if let Some(cursor) = cursor {
            let (created_at, id) =
                decode_cursor(&cursor).ok_or(PgClientKeyStoreError::InvalidCursor)?;
            let created_at =
                parse_rfc3339_utc(&created_at).map_err(|_| PgClientKeyStoreError::InvalidCursor)?;
            sqlx::query(
                r"
select id, name, label, prefix, key, enabled, created_at, last_used_at
from client_api_keys
where created_at < $1 or (created_at = $1 and id < $2)
order by created_at desc, id desc
limit $3",
            )
            .bind(created_at)
            .bind(id)
            .bind(fetch_limit)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                r"
select id, name, label, prefix, key, enabled, created_at, last_used_at
from client_api_keys
order by created_at desc, id desc
limit $1",
            )
            .bind(fetch_limit)
            .fetch_all(&self.pool)
            .await?
        };
        let has_next = rows.len() > limit as usize;
        let items = rows
            .into_iter()
            .take(limit as usize)
            .map(|row| Self::key_from_row(&row))
            .collect::<Vec<_>>();
        let next_cursor = has_next.then(|| {
            let key = items.last().expect("next page requires at least one key");
            encode_cursor(&key.created_at.to_rfc3339(), &key.id)
        });
        Ok(Page { items, next_cursor })
    }

    pub async fn get(&self, id: &str) -> Result<Option<StoredClientApiKey>, PgClientKeyStoreError> {
        let row = sqlx::query(
            r"
select id, name, label, prefix, key, enabled, created_at, last_used_at
from client_api_keys
where id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| Self::key_from_row(&row)))
    }

    pub async fn set_enabled(
        &self,
        id: &str,
        enabled: bool,
    ) -> Result<bool, PgClientKeyStoreError> {
        let result = sqlx::query("update client_api_keys set enabled = $1 where id = $2")
            .bind(enabled)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn set_label(
        &self,
        id: &str,
        label: Option<String>,
    ) -> Result<Option<StoredClientApiKey>, PgClientKeyStoreError> {
        let result = sqlx::query("update client_api_keys set label = $1 where id = $2")
            .bind(label)
            .bind(id)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Ok(None);
        }
        self.get(id).await
    }

    pub async fn delete(&self, id: &str) -> Result<bool, PgClientKeyStoreError> {
        let result = sqlx::query("delete from client_api_keys where id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub(super) async fn touch_last_used_batch(
        &self,
        updates: &BTreeMap<String, DateTime<Utc>>,
    ) -> Result<(), PgClientKeyStoreError> {
        if updates.is_empty() {
            return Ok(());
        }
        let ids = updates.keys().cloned().collect::<Vec<_>>();
        let timestamps = updates.values().copied().collect::<Vec<_>>();
        sqlx::query(
            r"
update client_api_keys as keys
set last_used_at = greatest(coalesce(keys.last_used_at, touched.used_at), touched.used_at)
from unnest($1::text[], $2::timestamptz[]) as touched(id, used_at)
where keys.id = touched.id",
        )
        .bind(ids)
        .bind(timestamps)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    fn key_from_row(row: &sqlx::postgres::PgRow) -> StoredClientApiKey {
        StoredClientApiKey {
            id: row.get("id"),
            name: row.get("name"),
            label: row.get("label"),
            prefix: row.get("prefix"),
            key: row.get("key"),
            enabled: row.get("enabled"),
            created_at: row.get("created_at"),
            last_used_at: row.get("last_used_at"),
        }
    }
}
