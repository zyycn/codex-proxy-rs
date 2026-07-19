//! 明文 `client_api_keys` 的 PostgreSQL owner。

use std::fmt;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gateway_core::policy::{ClientApiKeyId, PlaintextClientApiKey, RateLimits};
use sqlx::{PgPool, Postgres, QueryBuilder, Transaction};

use crate::{StoreError, StoreResult, postgres_unavailable, require_nonempty};

const ENTITY: &str = "client API key";
const KEY_LENGTH: usize = 46;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientApiKeySnapshot {
    pub id: ClientApiKeyId,
    pub plaintext_key: PlaintextClientApiKey,
    pub provider_kind: String,
    pub limits: RateLimits,
}

impl ClientApiKeySnapshot {
    pub(crate) fn from_persisted(
        id: String,
        key: String,
        provider_kind: String,
        max_concurrency: i64,
        requests_per_minute: i64,
        tokens_per_minute: i64,
    ) -> StoreResult<Self> {
        Ok(Self {
            id: ClientApiKeyId::new(id).map_err(|_| invalid("persisted key ID is invalid"))?,
            plaintext_key: PlaintextClientApiKey::new(key)
                .map_err(|_| invalid("persisted plaintext key is invalid"))?,
            provider_kind,
            limits: RateLimits {
                max_concurrency: to_u64(max_concurrency)?,
                requests_per_minute: to_u64(requests_per_minute)?,
                tokens_per_minute: to_u64(tokens_per_minute)?,
            },
        })
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ClientApiKeySecret {
    pub id: String,
    pub key: String,
    pub provider_kind: String,
    pub enabled: bool,
    pub max_concurrency: u64,
    pub requests_per_minute: u64,
    pub tokens_per_minute: u64,
}

impl fmt::Debug for ClientApiKeySecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClientApiKeySecret")
            .field("id", &self.id)
            .field("key", &"[REDACTED]")
            .field("enabled", &self.enabled)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientApiKeyRecord {
    pub id: String,
    pub name: String,
    pub label: Option<String>,
    pub provider_kind: String,
    pub prefix: String,
    pub enabled: bool,
    pub max_concurrency: u64,
    pub requests_per_minute: u64,
    pub tokens_per_minute: u64,
    pub last_used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ClientApiKeySortField {
    Name,
    Enabled,
    #[default]
    CreatedAt,
    LastUsedAt,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ClientApiKeySortDirection {
    Asc,
    #[default]
    Desc,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClientApiKeySort {
    pub field: ClientApiKeySortField,
    pub direction: ClientApiKeySortDirection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientApiKeyCursorValue {
    Name(String),
    Enabled(bool),
    CreatedAt(DateTime<Utc>),
    LastUsedAt(Option<DateTime<Utc>>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientApiKeyCursor {
    pub sort: ClientApiKeySort,
    pub value: ClientApiKeyCursorValue,
    pub id: String,
}

impl ClientApiKeyCursor {
    pub fn new(
        sort: ClientApiKeySort,
        value: ClientApiKeyCursorValue,
        id: impl Into<String>,
    ) -> StoreResult<Self> {
        let cursor = Self {
            sort,
            value,
            id: id.into(),
        };
        cursor.validate()?;
        Ok(cursor)
    }

    fn from_record(sort: ClientApiKeySort, record: &ClientApiKeyRecord) -> Self {
        let value = match sort.field {
            ClientApiKeySortField::Name => ClientApiKeyCursorValue::Name(record.name.clone()),
            ClientApiKeySortField::Enabled => ClientApiKeyCursorValue::Enabled(record.enabled),
            ClientApiKeySortField::CreatedAt => {
                ClientApiKeyCursorValue::CreatedAt(record.created_at)
            }
            ClientApiKeySortField::LastUsedAt => {
                ClientApiKeyCursorValue::LastUsedAt(record.last_used_at)
            }
        };
        Self {
            sort,
            value,
            id: record.id.clone(),
        }
    }

    fn validate(&self) -> StoreResult<()> {
        require_nonempty(ENTITY, "cursor id", &self.id)?;
        let matches_sort = matches!(
            (self.sort.field, &self.value),
            (
                ClientApiKeySortField::Name,
                ClientApiKeyCursorValue::Name(_)
            ) | (
                ClientApiKeySortField::Enabled,
                ClientApiKeyCursorValue::Enabled(_)
            ) | (
                ClientApiKeySortField::CreatedAt,
                ClientApiKeyCursorValue::CreatedAt(_)
            ) | (
                ClientApiKeySortField::LastUsedAt,
                ClientApiKeyCursorValue::LastUsedAt(_)
            )
        );
        if !matches_sort {
            return Err(invalid("cursor value does not match its sort field"));
        }
        if let ClientApiKeyCursorValue::Name(name) = &self.value {
            require_nonempty(ENTITY, "cursor name", name)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientApiKeyListQuery {
    pub cursor: Option<ClientApiKeyCursor>,
    pub page_size: u16,
    pub search: Option<String>,
    pub sort: ClientApiKeySort,
}

impl ClientApiKeyListQuery {
    pub fn validate(&self) -> StoreResult<()> {
        if self.page_size == 0 || self.page_size > 200 {
            return Err(invalid("page size must be between 1 and 200"));
        }
        if self.search.as_deref().is_some_and(|search| {
            search.trim().is_empty() || search.len() > 256 || search.chars().any(char::is_control)
        }) {
            return Err(invalid(
                "search must be a non-empty safe string at most 256 bytes",
            ));
        }
        if let Some(cursor) = &self.cursor {
            cursor.validate()?;
            if cursor.sort != self.sort {
                return Err(invalid("cursor sort does not match the requested sort"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientApiKeyPage {
    pub items: Vec<ClientApiKeyRecord>,
    pub total: u64,
    pub next_cursor: Option<ClientApiKeyCursor>,
}

#[derive(Clone)]
pub struct NewClientApiKey {
    pub id: String,
    pub name: String,
    pub label: Option<String>,
    pub provider_kind: String,
    pub key: String,
    pub max_concurrency: u64,
    pub requests_per_minute: u64,
    pub tokens_per_minute: u64,
}

impl fmt::Debug for NewClientApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NewClientApiKey")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("key", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl NewClientApiKey {
    pub fn validate(&self) -> StoreResult<()> {
        require_nonempty(ENTITY, "id", &self.id)?;
        require_nonempty(ENTITY, "name", &self.name)?;
        require_nonempty(ENTITY, "provider_kind", &self.provider_kind)?;
        validate_key(&self.key)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateClientApiKey {
    pub id: String,
    pub name: String,
    pub label: Option<String>,
    pub provider_kind: String,
    pub enabled: bool,
    pub max_concurrency: u64,
    pub requests_per_minute: u64,
    pub tokens_per_minute: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateClientApiKeyDetails {
    pub id: String,
    pub name: String,
    pub label: Option<String>,
    pub provider_kind: String,
    pub max_concurrency: u64,
    pub requests_per_minute: u64,
    pub tokens_per_minute: u64,
}

impl UpdateClientApiKeyDetails {
    pub fn validate(&self) -> StoreResult<()> {
        require_nonempty(ENTITY, "id", &self.id)?;
        require_nonempty(ENTITY, "name", &self.name)?;
        require_nonempty(ENTITY, "provider_kind", &self.provider_kind)?;
        to_i64(self.max_concurrency)?;
        to_i64(self.requests_per_minute)?;
        to_i64(self.tokens_per_minute)?;
        Ok(())
    }
}

#[async_trait]
pub trait ClientApiKeyRepository: Send + Sync {
    async fn authenticate_client_api_key(
        &self,
        key: &str,
    ) -> StoreResult<Option<ClientApiKeySecret>>;
    async fn list_client_api_keys(
        &self,
        query: ClientApiKeyListQuery,
    ) -> StoreResult<ClientApiKeyPage>;
    async fn reveal_client_api_key(&self, id: &str) -> StoreResult<Option<ClientApiKeySecret>>;
    async fn insert_client_api_key(&self, key: NewClientApiKey) -> StoreResult<()>;
    async fn update_client_api_key(&self, key: UpdateClientApiKey) -> StoreResult<bool>;
    async fn touch_client_api_keys(&self, ids: &[String]) -> StoreResult<u64>;
}

#[derive(Clone)]
pub struct PgClientApiKeyRepository {
    pool: PgPool,
}

impl PgClientApiKeyRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ClientApiKeyRepository for PgClientApiKeyRepository {
    async fn authenticate_client_api_key(
        &self,
        key: &str,
    ) -> StoreResult<Option<ClientApiKeySecret>> {
        validate_key(key)?;
        let row = sqlx::query_as::<_, (String, String, String, bool, i64, i64, i64)>(
            "select id, key, provider_kind, enabled, max_concurrency, requests_per_minute, tokens_per_minute
             from client_api_keys where key = $1",
        )
        .bind(key)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("authenticate client API key"))?;
        row.map(client_secret_from_row).transpose()
    }

    async fn list_client_api_keys(
        &self,
        query: ClientApiKeyListQuery,
    ) -> StoreResult<ClientApiKeyPage> {
        query.validate()?;
        let total = count_client_api_keys(&self.pool, query.search.as_deref()).await?;
        let mut statement = QueryBuilder::<Postgres>::new(
            "select id, name, label, provider_kind, left(key, 10) as prefix, enabled, max_concurrency,
                    requests_per_minute, tokens_per_minute, last_used_at, created_at, updated_at
             from client_api_keys where true",
        );
        push_client_key_search(&mut statement, query.search.as_deref());
        if let Some(cursor) = &query.cursor {
            push_client_key_cursor(&mut statement, cursor);
        }
        push_client_key_order(&mut statement, query.sort);
        statement.push(" limit ");
        statement.push_bind(i64::from(query.page_size) + 1);
        let rows = statement
            .build_query_as::<ClientRecordRow>()
            .fetch_all(&self.pool)
            .await
            .map_err(|_| postgres_unavailable("list client API keys"))?;
        let mut items = rows
            .into_iter()
            .map(client_record_from_row)
            .collect::<StoreResult<Vec<_>>>()?;
        let has_more = items.len() > usize::from(query.page_size);
        if has_more {
            items.pop();
        }
        let next_cursor = if has_more {
            items
                .last()
                .map(|item| ClientApiKeyCursor::from_record(query.sort, item))
        } else {
            None
        };
        Ok(ClientApiKeyPage {
            items,
            total,
            next_cursor,
        })
    }

    async fn reveal_client_api_key(&self, id: &str) -> StoreResult<Option<ClientApiKeySecret>> {
        require_nonempty(ENTITY, "id", id)?;
        sqlx::query_as::<_, (String, String, String, bool, i64, i64, i64)>(
            "select id, key, provider_kind, enabled, max_concurrency, requests_per_minute, tokens_per_minute
             from client_api_keys where id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("reveal client API key"))?
        .map(client_secret_from_row)
        .transpose()
    }

    async fn insert_client_api_key(&self, key: NewClientApiKey) -> StoreResult<()> {
        key.validate()?;
        sqlx::query(
            "insert into client_api_keys (
               id, name, label, provider_kind, key, enabled, max_concurrency, requests_per_minute,
               tokens_per_minute, last_used_at, created_at, updated_at
             ) values ($1, $2, $3, $4, $5, true, $6, $7, $8, null, now(), now())",
        )
        .bind(key.id)
        .bind(key.name)
        .bind(key.label)
        .bind(key.provider_kind)
        .bind(key.key)
        .bind(to_i64(key.max_concurrency)?)
        .bind(to_i64(key.requests_per_minute)?)
        .bind(to_i64(key.tokens_per_minute)?)
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("insert client API key"))?;
        Ok(())
    }

    async fn update_client_api_key(&self, key: UpdateClientApiKey) -> StoreResult<bool> {
        require_nonempty(ENTITY, "id", &key.id)?;
        require_nonempty(ENTITY, "name", &key.name)?;
        let result = sqlx::query(
            "update client_api_keys
             set name = $2, label = $3, provider_kind = $4, enabled = $5, max_concurrency = $6,
                 requests_per_minute = $7, tokens_per_minute = $8, updated_at = now()
             where id = $1",
        )
        .bind(key.id)
        .bind(key.name)
        .bind(key.label)
        .bind(key.provider_kind)
        .bind(key.enabled)
        .bind(to_i64(key.max_concurrency)?)
        .bind(to_i64(key.requests_per_minute)?)
        .bind(to_i64(key.tokens_per_minute)?)
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("update client API key"))?;
        Ok(result.rows_affected() == 1)
    }

    async fn touch_client_api_keys(&self, ids: &[String]) -> StoreResult<u64> {
        if ids.is_empty() {
            return Ok(0);
        }
        let result = sqlx::query(
            "update client_api_keys set last_used_at = now(), updated_at = greatest(updated_at, now())
             where id = any($1)",
        )
        .bind(ids)
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("touch client API keys"))?;
        Ok(result.rows_affected())
    }
}

pub(crate) async fn insert_client_api_key_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    key: &NewClientApiKey,
) -> StoreResult<()> {
    key.validate()?;
    sqlx::query(
        "insert into client_api_keys (
           id, name, label, provider_kind, key, enabled, max_concurrency, requests_per_minute,
           tokens_per_minute, last_used_at, created_at, updated_at
         ) values ($1, $2, $3, $4, $5, true, $6, $7, $8, null, now(), now())",
    )
    .bind(&key.id)
    .bind(&key.name)
    .bind(&key.label)
    .bind(&key.provider_kind)
    .bind(&key.key)
    .bind(to_i64(key.max_concurrency)?)
    .bind(to_i64(key.requests_per_minute)?)
    .bind(to_i64(key.tokens_per_minute)?)
    .execute(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("insert client API key in transaction"))?;
    Ok(())
}

pub(crate) async fn update_client_api_key_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    key: &UpdateClientApiKeyDetails,
) -> StoreResult<()> {
    key.validate()?;
    let result = sqlx::query(
        "update client_api_keys
         set name = $2, label = $3, provider_kind = $4, max_concurrency = $5,
             requests_per_minute = $6, tokens_per_minute = $7, updated_at = now()
         where id = $1",
    )
    .bind(&key.id)
    .bind(&key.name)
    .bind(&key.label)
    .bind(&key.provider_kind)
    .bind(to_i64(key.max_concurrency)?)
    .bind(to_i64(key.requests_per_minute)?)
    .bind(to_i64(key.tokens_per_minute)?)
    .execute(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("update client API key in transaction"))?;
    require_changed(result.rows_affected(), &key.id)
}

pub(crate) async fn set_client_api_key_enabled_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    id: &str,
    enabled: bool,
) -> StoreResult<()> {
    require_nonempty(ENTITY, "id", id)?;
    let result =
        sqlx::query("update client_api_keys set enabled = $2, updated_at = now() where id = $1")
            .bind(id)
            .bind(enabled)
            .execute(&mut **transaction)
            .await
            .map_err(|_| postgres_unavailable("set client API key state in transaction"))?;
    require_changed(result.rows_affected(), id)
}

pub(crate) async fn delete_client_api_key_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    id: &str,
) -> StoreResult<()> {
    require_nonempty(ENTITY, "id", id)?;
    let result = sqlx::query("delete from client_api_keys where id = $1")
        .bind(id)
        .execute(&mut **transaction)
        .await
        .map_err(|_| postgres_unavailable("delete client API key in transaction"))?;
    require_changed(result.rows_affected(), id)
}

fn require_changed(rows_affected: u64, id: &str) -> StoreResult<()> {
    if rows_affected == 1 {
        Ok(())
    } else {
        Err(StoreError::NotFound {
            entity: ENTITY,
            id: id.to_owned(),
        })
    }
}

fn validate_key(key: &str) -> StoreResult<()> {
    let valid = key.len() == KEY_LENGTH
        && key.starts_with("sk_")
        && key[3..]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if valid {
        Ok(())
    } else {
        Err(invalid(
            "key must be sk_ followed by 43 URL-safe characters",
        ))
    }
}

fn client_secret_from_row(
    row: (String, String, String, bool, i64, i64, i64),
) -> StoreResult<ClientApiKeySecret> {
    Ok(ClientApiKeySecret {
        id: row.0,
        key: row.1,
        provider_kind: row.2,
        enabled: row.3,
        max_concurrency: to_u64(row.4)?,
        requests_per_minute: to_u64(row.5)?,
        tokens_per_minute: to_u64(row.6)?,
    })
}

type ClientRecordRow = (
    String,
    String,
    Option<String>,
    String,
    String,
    bool,
    i64,
    i64,
    i64,
    Option<DateTime<Utc>>,
    DateTime<Utc>,
    DateTime<Utc>,
);

fn client_record_from_row(row: ClientRecordRow) -> StoreResult<ClientApiKeyRecord> {
    Ok(ClientApiKeyRecord {
        id: row.0,
        name: row.1,
        label: row.2,
        provider_kind: row.3,
        prefix: row.4,
        enabled: row.5,
        max_concurrency: to_u64(row.6)?,
        requests_per_minute: to_u64(row.7)?,
        tokens_per_minute: to_u64(row.8)?,
        last_used_at: row.9,
        created_at: row.10,
        updated_at: row.11,
    })
}

async fn count_client_api_keys(pool: &PgPool, search: Option<&str>) -> StoreResult<u64> {
    let mut statement =
        QueryBuilder::<Postgres>::new("select count(*)::bigint from client_api_keys where true");
    push_client_key_search(&mut statement, search);
    let count = statement
        .build_query_scalar::<i64>()
        .fetch_one(pool)
        .await
        .map_err(|_| postgres_unavailable("count client API keys"))?;
    to_u64(count)
}

fn push_client_key_search(statement: &mut QueryBuilder<Postgres>, search: Option<&str>) {
    if let Some(search) = search {
        statement.push(" and (lower(name) like ");
        statement.push_bind(format!("%{}%", search.to_lowercase()));
        statement.push(" or lower(coalesce(label, '')) like ");
        statement.push_bind(format!("%{}%", search.to_lowercase()));
        statement.push(" or lower(left(key, 10)) like ");
        statement.push_bind(format!("%{}%", search.to_lowercase()));
        statement.push(")");
    }
}

fn push_client_key_cursor(statement: &mut QueryBuilder<Postgres>, cursor: &ClientApiKeyCursor) {
    let comparison = match cursor.sort.direction {
        ClientApiKeySortDirection::Asc => " > ",
        ClientApiKeySortDirection::Desc => " < ",
    };
    match &cursor.value {
        ClientApiKeyCursorValue::Name(name) => {
            statement.push(" and (lower(name), id)");
            statement.push(comparison);
            statement.push("(");
            statement.push_bind(name.to_lowercase());
            statement.push(", ");
            statement.push_bind(cursor.id.clone());
            statement.push(")");
        }
        ClientApiKeyCursorValue::Enabled(enabled) => {
            statement.push(" and (enabled, id)");
            statement.push(comparison);
            statement.push("(");
            statement.push_bind(*enabled);
            statement.push(", ");
            statement.push_bind(cursor.id.clone());
            statement.push(")");
        }
        ClientApiKeyCursorValue::CreatedAt(created_at) => {
            statement.push(" and (created_at, id)");
            statement.push(comparison);
            statement.push("(");
            statement.push_bind(*created_at);
            statement.push(", ");
            statement.push_bind(cursor.id.clone());
            statement.push(")");
        }
        ClientApiKeyCursorValue::LastUsedAt(Some(last_used_at)) => {
            statement.push(" and (last_used_at is null or (last_used_at, id)");
            statement.push(comparison);
            statement.push("(");
            statement.push_bind(*last_used_at);
            statement.push(", ");
            statement.push_bind(cursor.id.clone());
            statement.push("))");
        }
        ClientApiKeyCursorValue::LastUsedAt(None) => {
            statement.push(" and last_used_at is null and id");
            statement.push(comparison);
            statement.push_bind(cursor.id.clone());
        }
    }
}

fn push_client_key_order(statement: &mut QueryBuilder<Postgres>, sort: ClientApiKeySort) {
    let direction = match sort.direction {
        ClientApiKeySortDirection::Asc => " asc",
        ClientApiKeySortDirection::Desc => " desc",
    };
    match sort.field {
        ClientApiKeySortField::Name => statement.push(" order by lower(name)"),
        ClientApiKeySortField::Enabled => statement.push(" order by enabled"),
        ClientApiKeySortField::CreatedAt => statement.push(" order by created_at"),
        ClientApiKeySortField::LastUsedAt => statement.push(" order by last_used_at"),
    };
    statement.push(direction);
    if sort.field == ClientApiKeySortField::LastUsedAt {
        statement.push(" nulls last");
    }
    statement.push(", id");
    statement.push(direction);
}

fn to_i64(value: u64) -> StoreResult<i64> {
    i64::try_from(value).map_err(|_| invalid("numeric limit exceeds PostgreSQL bigint"))
}

fn to_u64(value: i64) -> StoreResult<u64> {
    u64::try_from(value).map_err(|_| invalid("persisted numeric limit is negative"))
}

fn invalid(message: &str) -> StoreError {
    StoreError::InvalidData {
        entity: ENTITY,
        message: message.to_owned(),
    }
}
