//! 客户端 API Key PostgreSQL 存储。

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, QueryBuilder, Row};
use thiserror::Error;
use uuid::Uuid;

use crate::infra::{
    identity::generate_client_api_key,
    json::{NumberedPage, page_offset},
};
use crate::keys::types::{ClientApiKeyListSort, ClientApiKeySortField};

const CLIENT_KEY_SELECT_SQL: &str = r"select
  id, name, label, prefix, key, enabled, created_at, last_used_at
from client_api_keys";

#[derive(Debug, Error)]
pub enum PgClientKeyStoreError {
    #[error("PostgreSQL client key operation failed: {0}")]
    Database(#[from] sqlx::Error),
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

    pub async fn list_page(
        &self,
        page: u32,
        page_size: u32,
        search: Option<&str>,
        sort: Option<ClientApiKeyListSort>,
    ) -> Result<NumberedPage<StoredClientApiKey>, PgClientKeyStoreError> {
        let page = page.max(1);
        let page_size = page_size.clamp(1, 200);
        let search = search.map(str::trim).filter(|value| !value.is_empty());

        let mut count_builder =
            QueryBuilder::<Postgres>::new("select count(*) from client_api_keys");
        push_search(&mut count_builder, search);
        let total = count_builder
            .build_query_scalar::<i64>()
            .fetch_one(&self.pool)
            .await?
            .max(0) as u64;

        let mut builder = QueryBuilder::<Postgres>::new(CLIENT_KEY_SELECT_SQL);
        push_search(&mut builder, search);
        push_order(&mut builder, sort);
        builder.push(" limit ");
        builder.push_bind(i64::from(page_size));
        builder.push(" offset ");
        builder.push_bind(page_offset(page, page_size).min(i64::MAX as u64) as i64);
        let rows = builder.build().fetch_all(&self.pool).await?;
        let items = rows.iter().map(Self::key_from_row).collect::<Vec<_>>();
        Ok(NumberedPage {
            items,
            total,
            page,
            page_size,
        })
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

fn push_order(builder: &mut QueryBuilder<Postgres>, sort: Option<ClientApiKeyListSort>) {
    let Some(sort) = sort else {
        builder.push(" order by created_at desc, id desc");
        return;
    };

    builder.push(" order by ");
    match sort.field {
        ClientApiKeySortField::Name => builder.push("lower(name)"),
        ClientApiKeySortField::Enabled => builder.push("enabled"),
        ClientApiKeySortField::CreatedAt => builder.push("created_at"),
        ClientApiKeySortField::LastUsedAt => builder.push("last_used_at"),
    };
    let direction = match sort.direction {
        crate::infra::json::SortDirection::Asc => " asc",
        crate::infra::json::SortDirection::Desc => " desc",
    };
    builder.push(direction);
    builder.push(" nulls last, id");
    builder.push(direction);
}

fn push_search(builder: &mut QueryBuilder<Postgres>, search: Option<&str>) {
    let Some(search) = search else {
        return;
    };
    let pattern = format!("%{search}%");
    builder.push(" where (name ilike ");
    builder.push_bind(pattern.clone());
    builder.push(" or label ilike ");
    builder.push_bind(pattern.clone());
    builder.push(" or id ilike ");
    builder.push_bind(pattern);
    builder.push(")");
}
