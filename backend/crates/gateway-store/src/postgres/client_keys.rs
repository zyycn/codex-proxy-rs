//! 明文 `client_api_keys` 的 PostgreSQL owner。

use std::{
    collections::BTreeMap,
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gateway_admin::{
    model::{
        MutationContext,
        client_keys::{
            ClientKeyCursor as AdminClientKeyCursor,
            ClientKeyCursorValue as AdminClientKeyCursorValue,
            ClientKeyListQuery as AdminClientKeyListQuery, ClientKeyPage as AdminClientKeyPage,
            ClientKeyRecord as AdminClientKeyRecord, ClientKeySecret as AdminClientKeySecret,
            ClientKeySort as AdminClientKeySort, ClientKeySortField as AdminClientKeySortField,
            DeleteClientKey, NewClientKey, SetClientKeyEnabled,
            SortDirection as AdminSortDirection, UpdateClientKey as AdminUpdateClientKey,
        },
    },
    ports::store::{AdminStoreResult, ClientKeyStore},
};
use gateway_core::routing::ProviderKind;
use gateway_core::{
    engine::execution::ClientApiKeyUsageSink,
    policy::{ClientApiKeyId, PlaintextClientApiKey, RateLimits},
};
use sqlx::{PgPool, Postgres, QueryBuilder, Transaction};

use crate::{
    StoreError, StoreResult, admin_revision, admin_store_error, mutation_audit,
    postgres_unavailable, require_nonempty, store_revision,
};

use super::{ControlPlaneRepository, PgControlPlaneRepository};

const ENTITY: &str = "client API key";
const KEY_LENGTH: usize = 46;
const CLIENT_API_KEY_LAST_USED_FLUSH_DELAY: Duration = Duration::from_secs(1);

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
    ) -> StoreResult<Self> {
        Ok(Self {
            id: ClientApiKeyId::new(id).map_err(|_| invalid("persisted key ID is invalid"))?,
            plaintext_key: PlaintextClientApiKey::new(key)
                .map_err(|_| invalid("persisted plaintext key is invalid"))?,
            provider_kind,
            limits: RateLimits {
                max_concurrency: to_u64(max_concurrency)?,
                requests_per_minute: to_u64(requests_per_minute)?,
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
        if self.page_size == 0 {
            return Err(invalid("page size must be between 1 and 65535"));
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateClientApiKeyDetails {
    pub id: String,
    pub name: String,
    pub label: Option<String>,
    pub provider_kind: String,
    pub max_concurrency: u64,
    pub requests_per_minute: u64,
}

impl UpdateClientApiKeyDetails {
    pub fn validate(&self) -> StoreResult<()> {
        require_nonempty(ENTITY, "id", &self.id)?;
        require_nonempty(ENTITY, "name", &self.name)?;
        require_nonempty(ENTITY, "provider_kind", &self.provider_kind)?;
        to_i64(self.max_concurrency)?;
        to_i64(self.requests_per_minute)?;
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
    async fn get_client_api_key(&self, id: &str) -> StoreResult<Option<ClientApiKeyRecord>>;
    async fn reveal_client_api_key(&self, id: &str) -> StoreResult<Option<ClientApiKeySecret>>;
    async fn insert_client_api_key(&self, key: NewClientApiKey) -> StoreResult<()>;
    async fn update_client_api_key(&self, key: UpdateClientApiKey) -> StoreResult<bool>;
    async fn touch_client_api_keys(
        &self,
        touched_at: &BTreeMap<String, DateTime<Utc>>,
    ) -> StoreResult<u64>;
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
        let row = sqlx::query_as::<_, (String, String, String, bool, i64, i64)>(
            "select id, key, provider_kind, enabled, max_concurrency, requests_per_minute
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
                    requests_per_minute, last_used_at, created_at, updated_at
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
        sqlx::query_as::<_, (String, String, String, bool, i64, i64)>(
            "select id, key, provider_kind, enabled, max_concurrency, requests_per_minute
             from client_api_keys where id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("reveal client API key"))?
        .map(client_secret_from_row)
        .transpose()
    }

    async fn get_client_api_key(&self, id: &str) -> StoreResult<Option<ClientApiKeyRecord>> {
        require_nonempty(ENTITY, "id", id)?;
        sqlx::query_as::<_, ClientRecordRow>(
            "select id, name, label, provider_kind, left(key, 10) as prefix, enabled, max_concurrency,
                    requests_per_minute, last_used_at, created_at, updated_at
             from client_api_keys where id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("get client API key"))?
        .map(client_record_from_row)
        .transpose()
    }

    async fn insert_client_api_key(&self, key: NewClientApiKey) -> StoreResult<()> {
        key.validate()?;
        sqlx::query(
            "insert into client_api_keys (
               id, name, label, provider_kind, key, enabled, max_concurrency, requests_per_minute,
               last_used_at, created_at, updated_at
             ) values ($1, $2, $3, $4, $5, true, $6, $7, null, now(), now())",
        )
        .bind(key.id)
        .bind(key.name)
        .bind(key.label)
        .bind(key.provider_kind)
        .bind(key.key)
        .bind(to_i64(key.max_concurrency)?)
        .bind(to_i64(key.requests_per_minute)?)
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
                 requests_per_minute = $7, updated_at = now()
             where id = $1",
        )
        .bind(key.id)
        .bind(key.name)
        .bind(key.label)
        .bind(key.provider_kind)
        .bind(key.enabled)
        .bind(to_i64(key.max_concurrency)?)
        .bind(to_i64(key.requests_per_minute)?)
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("update client API key"))?;
        Ok(result.rows_affected() == 1)
    }

    async fn touch_client_api_keys(
        &self,
        touched_at: &BTreeMap<String, DateTime<Utc>>,
    ) -> StoreResult<u64> {
        if touched_at.is_empty() {
            return Ok(0);
        }
        let ids = touched_at.keys().cloned().collect::<Vec<_>>();
        let timestamps = touched_at.values().copied().collect::<Vec<_>>();
        let result = sqlx::query(
            "update client_api_keys as keys
             set last_used_at = greatest(coalesce(keys.last_used_at, touched.used_at), touched.used_at)
             from unnest($1::text[], $2::timestamptz[]) as touched(id, used_at)
             where keys.id = touched.id",
        )
        .bind(ids)
        .bind(timestamps)
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("touch client API keys"))?;
        Ok(result.rows_affected())
    }
}

/// 认证成功后按一秒窗口合并写回 API Key 最后使用时间。
///
/// 该 adapter 仅记录稳定 Key ID；认证材料从不进入异步队列或日志。
#[derive(Clone)]
pub struct PgClientApiKeyUsageSink {
    repository: PgClientApiKeyRepository,
    pending: Arc<Mutex<BTreeMap<String, DateTime<Utc>>>>,
    flush_scheduled: Arc<AtomicBool>,
}

impl PgClientApiKeyUsageSink {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            repository: PgClientApiKeyRepository::new(pool),
            pending: Arc::new(Mutex::new(BTreeMap::new())),
            flush_scheduled: Arc::new(AtomicBool::new(false)),
        }
    }

    fn queue(&self, key_id: &ClientApiKeyId) {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key_id.as_str().to_owned(), Utc::now());
        self.schedule_flush();
    }

    fn schedule_flush(&self) {
        if self
            .flush_scheduled
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.flush_scheduled.store(false, Ordering::Release);
            return;
        };
        let sink = self.clone();
        drop(runtime.spawn(async move {
            tokio::time::sleep(CLIENT_API_KEY_LAST_USED_FLUSH_DELAY).await;
            sink.flush_pending().await;
        }));
    }

    async fn flush_pending(&self) {
        let updates = std::mem::take(
            &mut *self
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        if let Err(error) = self.repository.touch_client_api_keys(&updates).await {
            tracing::error!(error = %error, "Failed to flush client API key last-used batch");
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for (key_id, used_at) in updates {
                pending
                    .entry(key_id)
                    .and_modify(|pending_at| *pending_at = (*pending_at).max(used_at))
                    .or_insert(used_at);
            }
        }
        self.flush_scheduled.store(false, Ordering::Release);
        if !self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
        {
            self.schedule_flush();
        }
    }
}

impl ClientApiKeyUsageSink for PgClientApiKeyUsageSink {
    fn record_used(&self, key_id: &ClientApiKeyId) {
        self.queue(key_id);
    }
}

/// Admin 用例所需的 Client Key 事务能力。
#[derive(Clone)]
pub struct PgAdminClientKeyStore {
    keys: PgClientApiKeyRepository,
    control_plane: PgControlPlaneRepository,
}

impl PgAdminClientKeyStore {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            keys: PgClientApiKeyRepository::new(pool.clone()),
            control_plane: PgControlPlaneRepository::new(pool),
        }
    }

    async fn revision(&self) -> AdminStoreResult<gateway_admin::model::Revision> {
        self.control_plane
            .load_control_plane()
            .await
            .map_err(|error| admin_store_error(ENTITY, error))
            .and_then(|snapshot| admin_revision(snapshot.settings.config_revision))
    }

    async fn required_record(&self, id: &ClientApiKeyId) -> AdminStoreResult<AdminClientKeyRecord> {
        self.keys
            .get_client_api_key(id.as_str())
            .await
            .map_err(|error| admin_store_error(ENTITY, error))?
            .ok_or_else(|| {
                admin_store_error(
                    ENTITY,
                    StoreError::NotFound {
                        entity: ENTITY,
                        id: id.as_str().to_owned(),
                    },
                )
            })
            .and_then(admin_client_key_record)
    }
}

#[async_trait]
impl ClientKeyStore for PgAdminClientKeyStore {
    async fn list_client_keys(
        &self,
        query: AdminClientKeyListQuery,
    ) -> AdminStoreResult<AdminClientKeyPage> {
        let config_revision = self.revision().await?;
        let page = self
            .keys
            .list_client_api_keys(store_client_key_query(query)?)
            .await
            .map_err(|error| admin_store_error(ENTITY, error))?;
        Ok(AdminClientKeyPage {
            config_revision,
            items: page
                .items
                .into_iter()
                .map(admin_client_key_record)
                .collect::<AdminStoreResult<Vec<_>>>()?,
            total: page.total,
            next_cursor: page.next_cursor.map(admin_client_key_cursor).transpose()?,
        })
    }

    async fn reveal_client_key(
        &self,
        id: &ClientApiKeyId,
    ) -> AdminStoreResult<Option<AdminClientKeySecret>> {
        let Some(secret) = self
            .keys
            .reveal_client_api_key(id.as_str())
            .await
            .map_err(|error| admin_store_error(ENTITY, error))?
        else {
            return Ok(None);
        };
        let record = self.required_record(id).await?;
        Ok(Some(AdminClientKeySecret::new(record, secret.key)))
    }

    async fn create_client_key(
        &self,
        command: NewClientKey,
        context: &MutationContext,
    ) -> AdminStoreResult<(gateway_admin::model::Revision, AdminClientKeyRecord)> {
        let id = command.id;
        let revision = self
            .control_plane
            .create_client_api_key(
                store_revision(command.expected_config_revision)?,
                NewClientApiKey {
                    id: id.as_str().to_owned(),
                    name: command.name,
                    label: command.label,
                    provider_kind: command.provider_kind.as_str().to_owned(),
                    key: command.plaintext,
                    max_concurrency: command.limits.max_concurrency,
                    requests_per_minute: command.limits.requests_per_minute,
                },
                mutation_audit(
                    context,
                    "create",
                    "client_api_key",
                    id.as_str(),
                    [
                        "name",
                        "label",
                        "provider_kind",
                        "key",
                        "enabled",
                        "max_concurrency",
                        "requests_per_minute",
                    ]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                ),
            )
            .await
            .map_err(|error| admin_store_error(ENTITY, error))?;
        Ok((admin_revision(revision)?, self.required_record(&id).await?))
    }

    async fn update_client_key(
        &self,
        command: AdminUpdateClientKey,
        context: &MutationContext,
    ) -> AdminStoreResult<(gateway_admin::model::Revision, AdminClientKeyRecord)> {
        let id = command.id;
        let revision = self
            .control_plane
            .update_client_api_key(
                store_revision(command.expected_config_revision)?,
                UpdateClientApiKeyDetails {
                    id: id.as_str().to_owned(),
                    name: command.name,
                    label: command.label,
                    provider_kind: command.provider_kind.as_str().to_owned(),
                    max_concurrency: command.limits.max_concurrency,
                    requests_per_minute: command.limits.requests_per_minute,
                },
                mutation_audit(
                    context,
                    "update",
                    "client_api_key",
                    id.as_str(),
                    [
                        "name",
                        "label",
                        "provider_kind",
                        "max_concurrency",
                        "requests_per_minute",
                    ]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                ),
            )
            .await
            .map_err(|error| admin_store_error(ENTITY, error))?;
        Ok((admin_revision(revision)?, self.required_record(&id).await?))
    }

    async fn set_client_key_enabled(
        &self,
        command: SetClientKeyEnabled,
        context: &MutationContext,
    ) -> AdminStoreResult<(gateway_admin::model::Revision, AdminClientKeyRecord)> {
        let id = command.id;
        let revision = self
            .control_plane
            .set_client_api_key_enabled(
                store_revision(command.expected_config_revision)?,
                id.as_str(),
                command.enabled,
                mutation_audit(
                    context,
                    if command.enabled { "enable" } else { "disable" },
                    "client_api_key",
                    id.as_str(),
                    vec!["enabled".to_owned()],
                ),
            )
            .await
            .map_err(|error| admin_store_error(ENTITY, error))?;
        Ok((admin_revision(revision)?, self.required_record(&id).await?))
    }

    async fn delete_client_key(
        &self,
        command: DeleteClientKey,
        context: &MutationContext,
    ) -> AdminStoreResult<gateway_admin::model::Revision> {
        self.control_plane
            .delete_client_api_key(
                store_revision(command.expected_config_revision)?,
                command.id.as_str(),
                mutation_audit(
                    context,
                    "delete",
                    "client_api_key",
                    command.id.as_str(),
                    Vec::new(),
                ),
            )
            .await
            .map_err(|error| admin_store_error(ENTITY, error))
            .and_then(admin_revision)
    }
}

fn store_client_key_query(
    query: AdminClientKeyListQuery,
) -> AdminStoreResult<ClientApiKeyListQuery> {
    let sort = store_client_key_sort(query.sort);
    Ok(ClientApiKeyListQuery {
        cursor: query
            .cursor
            .map(|cursor| store_client_key_cursor(cursor, sort))
            .transpose()?,
        page_size: query.page_size.get(),
        search: query.search,
        sort,
    })
}

fn store_client_key_sort(sort: AdminClientKeySort) -> ClientApiKeySort {
    ClientApiKeySort {
        field: match sort.field {
            AdminClientKeySortField::Name => ClientApiKeySortField::Name,
            AdminClientKeySortField::Enabled => ClientApiKeySortField::Enabled,
            AdminClientKeySortField::CreatedAt => ClientApiKeySortField::CreatedAt,
            AdminClientKeySortField::LastUsedAt => ClientApiKeySortField::LastUsedAt,
        },
        direction: match sort.direction {
            AdminSortDirection::Asc => ClientApiKeySortDirection::Asc,
            AdminSortDirection::Desc => ClientApiKeySortDirection::Desc,
        },
    }
}

fn store_client_key_cursor(
    cursor: AdminClientKeyCursor,
    sort: ClientApiKeySort,
) -> AdminStoreResult<ClientApiKeyCursor> {
    let value = match cursor.value {
        AdminClientKeyCursorValue::Name(value) => ClientApiKeyCursorValue::Name(value),
        AdminClientKeyCursorValue::Enabled(value) => ClientApiKeyCursorValue::Enabled(value),
        AdminClientKeyCursorValue::CreatedAt(value) => ClientApiKeyCursorValue::CreatedAt(value),
        AdminClientKeyCursorValue::LastUsedAt(value) => ClientApiKeyCursorValue::LastUsedAt(value),
    };
    ClientApiKeyCursor::new(sort, value, cursor.id.as_str().to_owned())
        .map_err(|error| admin_store_error(ENTITY, error))
}

fn admin_client_key_cursor(cursor: ClientApiKeyCursor) -> AdminStoreResult<AdminClientKeyCursor> {
    let sort = AdminClientKeySort {
        field: match cursor.sort.field {
            ClientApiKeySortField::Name => AdminClientKeySortField::Name,
            ClientApiKeySortField::Enabled => AdminClientKeySortField::Enabled,
            ClientApiKeySortField::CreatedAt => AdminClientKeySortField::CreatedAt,
            ClientApiKeySortField::LastUsedAt => AdminClientKeySortField::LastUsedAt,
        },
        direction: match cursor.sort.direction {
            ClientApiKeySortDirection::Asc => AdminSortDirection::Asc,
            ClientApiKeySortDirection::Desc => AdminSortDirection::Desc,
        },
    };
    let value = match cursor.value {
        ClientApiKeyCursorValue::Name(value) => AdminClientKeyCursorValue::Name(value),
        ClientApiKeyCursorValue::Enabled(value) => AdminClientKeyCursorValue::Enabled(value),
        ClientApiKeyCursorValue::CreatedAt(value) => AdminClientKeyCursorValue::CreatedAt(value),
        ClientApiKeyCursorValue::LastUsedAt(value) => AdminClientKeyCursorValue::LastUsedAt(value),
    };
    Ok(AdminClientKeyCursor {
        sort,
        value,
        id: ClientApiKeyId::new(cursor.id)
            .map_err(|_| admin_store_error(ENTITY, invalid("invalid client key id")))?,
    })
}

fn admin_client_key_record(record: ClientApiKeyRecord) -> AdminStoreResult<AdminClientKeyRecord> {
    Ok(AdminClientKeyRecord {
        id: ClientApiKeyId::new(record.id)
            .map_err(|_| admin_store_error(ENTITY, invalid("invalid client key id")))?,
        name: record.name,
        label: record.label,
        provider_kind: ProviderKind::new(record.provider_kind)
            .map_err(|_| admin_store_error(ENTITY, invalid("invalid provider kind")))?,
        prefix: record.prefix,
        enabled: record.enabled,
        limits: RateLimits {
            max_concurrency: record.max_concurrency,
            requests_per_minute: record.requests_per_minute,
        },
        last_used_at: record.last_used_at,
        created_at: record.created_at,
        updated_at: record.updated_at,
    })
}

pub(crate) async fn insert_client_api_key_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    key: &NewClientApiKey,
) -> StoreResult<()> {
    key.validate()?;
    sqlx::query(
        "insert into client_api_keys (
           id, name, label, provider_kind, key, enabled, max_concurrency, requests_per_minute,
           last_used_at, created_at, updated_at
         ) values ($1, $2, $3, $4, $5, true, $6, $7, null, now(), now())",
    )
    .bind(&key.id)
    .bind(&key.name)
    .bind(&key.label)
    .bind(&key.provider_kind)
    .bind(&key.key)
    .bind(to_i64(key.max_concurrency)?)
    .bind(to_i64(key.requests_per_minute)?)
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
             requests_per_minute = $6, updated_at = now()
         where id = $1",
    )
    .bind(&key.id)
    .bind(&key.name)
    .bind(&key.label)
    .bind(&key.provider_kind)
    .bind(to_i64(key.max_concurrency)?)
    .bind(to_i64(key.requests_per_minute)?)
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
    row: (String, String, String, bool, i64, i64),
) -> StoreResult<ClientApiKeySecret> {
    Ok(ClientApiKeySecret {
        id: row.0,
        key: row.1,
        provider_kind: row.2,
        enabled: row.3,
        max_concurrency: to_u64(row.4)?,
        requests_per_minute: to_u64(row.5)?,
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
        last_used_at: row.8,
        created_at: row.9,
        updated_at: row.10,
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
