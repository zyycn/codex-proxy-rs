//! 明文 `client_api_keys` 的 PostgreSQL owner。

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use gateway_admin::{
    model::{
        MutationContext,
        account_groups::AccountGroupRef as AdminAccountGroupRef,
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
use gateway_core::{
    engine::execution::ClientApiKeyUsageSink,
    lifecycle::CancellationToken,
    policy::{ClientApiKeyId, PlaintextClientApiKey, RateLimits},
    routing::{AccountGroupId, ProviderKind},
    task::{DaemonTask, WorkerTaskError},
};
use serde::Deserialize;
use sqlx::{PgPool, Postgres, QueryBuilder, Transaction};
use tokio::sync::Notify;

use crate::{
    StoreError, StoreResult, admin_revision, admin_store_error, mutation_audit,
    postgres_unavailable, require_nonempty,
};

use super::{ControlPlaneRepository, PgControlPlaneRepository};

const ENTITY: &str = "client API key";
const KEY_LENGTH: usize = 46;
const CLIENT_API_KEY_LAST_USED_FLUSH_DELAY: Duration = Duration::from_secs(1);
const CLIENT_API_KEY_LAST_USED_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientApiKeySnapshot {
    pub id: ClientApiKeyId,
    pub plaintext_key: PlaintextClientApiKey,
    pub group_ids: Vec<AccountGroupId>,
    pub limits: RateLimits,
}

impl ClientApiKeySnapshot {
    pub(crate) fn from_persisted(
        id: String,
        key: String,
        group_ids: Vec<String>,
        max_concurrency: i64,
        requests_per_minute: i64,
    ) -> StoreResult<Self> {
        Ok(Self {
            id: ClientApiKeyId::new(id).map_err(|_| invalid("persisted key ID is invalid"))?,
            plaintext_key: PlaintextClientApiKey::new(key)
                .map_err(|_| invalid("persisted plaintext key is invalid"))?,
            group_ids: group_ids
                .into_iter()
                .map(|id| {
                    AccountGroupId::new(id).map_err(|_| invalid("persisted group ID is invalid"))
                })
                .collect::<StoreResult<Vec<_>>>()?,
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
    pub groups: Vec<ClientApiKeyGroupRecord>,
    pub provider_kinds: Vec<String>,
    pub prefix: String,
    pub enabled: bool,
    pub max_concurrency: u64,
    pub requests_per_minute: u64,
    pub last_used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ClientApiKeyGroupRecord {
    pub id: String,
    pub name: String,
    pub color: String,
    pub enabled: bool,
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
    pub group_ids: Vec<String>,
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
        validate_group_ids(&self.group_ids)?;
        validate_key(&self.key)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateClientApiKeyDetails {
    pub id: String,
    pub name: String,
    pub label: Option<String>,
    pub group_ids: Vec<String>,
    pub max_concurrency: u64,
    pub requests_per_minute: u64,
}

impl UpdateClientApiKeyDetails {
    pub fn validate(&self) -> StoreResult<()> {
        require_nonempty(ENTITY, "id", &self.id)?;
        require_nonempty(ENTITY, "name", &self.name)?;
        validate_group_ids(&self.group_ids)?;
        to_i64(self.max_concurrency)?;
        to_i64(self.requests_per_minute)?;
        Ok(())
    }
}

#[async_trait]
pub trait ClientApiKeyRepository: Send + Sync {
    async fn list_client_api_keys(
        &self,
        query: ClientApiKeyListQuery,
    ) -> StoreResult<ClientApiKeyPage>;
    async fn get_client_api_key(&self, id: &str) -> StoreResult<Option<ClientApiKeyRecord>>;
    async fn reveal_client_api_key(&self, id: &str) -> StoreResult<Option<ClientApiKeySecret>>;
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
    async fn list_client_api_keys(
        &self,
        query: ClientApiKeyListQuery,
    ) -> StoreResult<ClientApiKeyPage> {
        query.validate()?;
        let total = count_client_api_keys(&self.pool, query.search.as_deref()).await?;
        let mut statement = QueryBuilder::<Postgres>::new(
            "select k.id, k.name, k.label, left(k.key, 10) as prefix, k.enabled,
                    k.max_concurrency, k.requests_per_minute, k.last_used_at, k.created_at,
                    k.updated_at, '[]'::jsonb as groups, '{}'::text[] as provider_kinds
             from client_api_keys k
             where true",
        );
        push_client_key_search(&mut statement, query.search.as_deref());
        if let Some(cursor) = &query.cursor {
            push_client_key_cursor(&mut statement, cursor);
        }
        push_client_key_order(&mut statement, query.sort);
        statement.push(" limit ");
        statement.push_bind(i64::from(query.page_size) + 1);
        let rows = statement
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(|_| postgres_unavailable("list client API keys"))?;
        let mut items = rows
            .iter()
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
        load_client_key_memberships(&self.pool, &mut items).await?;
        Ok(ClientApiKeyPage {
            items,
            total,
            next_cursor,
        })
    }

    async fn reveal_client_api_key(&self, id: &str) -> StoreResult<Option<ClientApiKeySecret>> {
        require_nonempty(ENTITY, "id", id)?;
        sqlx::query_as::<_, (String, String, bool, i64, i64)>(
            "select id, key, enabled, max_concurrency, requests_per_minute
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
        sqlx::query(
            "select k.id, k.name, k.label, left(k.key, 10) as prefix, k.enabled,
                    k.max_concurrency, k.requests_per_minute, k.last_used_at, k.created_at,
                    k.updated_at, coalesce(groups.groups, '[]'::jsonb) as groups,
                    case
                      when groups.binding_count = 0 then coalesce(
                        (select array_agg(distinct a.provider_kind order by a.provider_kind)
                         from provider_accounts a),
                        '{}'
                      )
                      else coalesce(groups.provider_kinds, '{}')
                    end as provider_kinds
             from client_api_keys k
             left join lateral (
               select
                 (select count(*)::bigint
                  from client_api_key_groups kg
                  where kg.client_api_key_id = k.id) as binding_count,
                 (select jsonb_agg(jsonb_build_object(
                           'id', g.id, 'name', g.name, 'color', g.color, 'enabled', g.enabled
                         ) order by g.id)
                  from client_api_key_groups kg
                  join account_groups g on g.id = kg.account_group_id
                  where kg.client_api_key_id = k.id) as groups,
                 (select array_agg(distinct a.provider_kind order by a.provider_kind)
                  from client_api_key_groups kg
                  join account_groups g on g.id = kg.account_group_id and g.enabled
                  join account_group_accounts gm on gm.account_group_id = g.id
                  join provider_accounts a on a.id = gm.provider_account_id
                  where kg.client_api_key_id = k.id) as provider_kinds
             ) groups on true
             where k.id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("get client API key"))?
        .as_ref()
        .map(client_record_from_row)
        .transpose()
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
    state: Arc<ClientApiKeyUsageBuffer>,
}

struct ClientApiKeyUsageBuffer {
    pending: Mutex<BTreeMap<String, DateTime<Utc>>>,
    flush_requested: Notify,
}

pub struct PgClientApiKeyUsageWriter {
    repository: PgClientApiKeyRepository,
    state: Arc<ClientApiKeyUsageBuffer>,
    flush_delay: Duration,
}

impl PgClientApiKeyUsageSink {
    #[must_use]
    pub fn new(pool: PgPool) -> (Self, PgClientApiKeyUsageWriter) {
        Self::with_flush_delay(pool, CLIENT_API_KEY_LAST_USED_FLUSH_DELAY)
    }

    #[must_use]
    pub fn with_flush_delay(
        pool: PgPool,
        flush_delay: Duration,
    ) -> (Self, PgClientApiKeyUsageWriter) {
        let state = Arc::new(ClientApiKeyUsageBuffer {
            pending: Mutex::new(BTreeMap::new()),
            flush_requested: Notify::new(),
        });
        (
            Self {
                state: Arc::clone(&state),
            },
            PgClientApiKeyUsageWriter {
                repository: PgClientApiKeyRepository::new(pool),
                state,
                flush_delay: flush_delay.max(Duration::from_millis(1)),
            },
        )
    }

    fn queue(&self, key_id: &ClientApiKeyId) {
        let used_at = Utc::now();
        let mut pending = lock_unpoisoned(&self.state.pending);
        pending
            .entry(key_id.as_str().to_owned())
            .and_modify(|pending_at| *pending_at = (*pending_at).max(used_at))
            .or_insert(used_at);
        drop(pending);
        self.state.flush_requested.notify_one();
    }
}

impl PgClientApiKeyUsageWriter {
    async fn flush_pending(&self) -> StoreResult<u64> {
        let updates = std::mem::take(&mut *lock_unpoisoned(&self.state.pending));
        if updates.is_empty() {
            return Ok(0);
        }
        match self.repository.touch_client_api_keys(&updates).await {
            Ok(updated) => Ok(updated),
            Err(error) => {
                let mut pending = lock_unpoisoned(&self.state.pending);
                for (key_id, used_at) in updates {
                    pending
                        .entry(key_id)
                        .and_modify(|pending_at| *pending_at = (*pending_at).max(used_at))
                        .or_insert(used_at);
                }
                drop(pending);
                self.state.flush_requested.notify_one();
                Err(error)
            }
        }
    }

    async fn flush_on_shutdown(&self) {
        match tokio::time::timeout(
            CLIENT_API_KEY_LAST_USED_SHUTDOWN_TIMEOUT,
            self.flush_pending(),
        )
        .await
        {
            Ok(Ok(updated)) if updated > 0 => {
                tracing::info!(updated, "Client API Key last-used 已在关闭前写回");
            }
            Ok(Ok(_)) => {}
            Ok(Err(_)) => {
                tracing::warn!("Client API Key last-used 关闭写回失败");
            }
            Err(_) => {
                tracing::warn!(
                    pending = lock_unpoisoned(&self.state.pending).len(),
                    "Client API Key last-used 关闭写回超时"
                );
            }
        }
    }
}

impl ClientApiKeyUsageSink for PgClientApiKeyUsageSink {
    fn record_used(&self, key_id: &ClientApiKeyId) {
        self.queue(key_id);
    }
}

impl DaemonTask for PgClientApiKeyUsageWriter {
    fn run(&self, cancellation: CancellationToken) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            loop {
                tokio::select! {
                    () = cancellation.cancelled() => {
                        self.flush_on_shutdown().await;
                        return Ok(());
                    }
                    () = self.state.flush_requested.notified() => {}
                }
                tokio::select! {
                    () = cancellation.cancelled() => {
                        self.flush_on_shutdown().await;
                        return Ok(());
                    }
                    () = tokio::time::sleep(self.flush_delay) => {}
                }
                if self.flush_pending().await.is_err() {
                    tracing::warn!("Client API Key last-used 批量写回失败");
                    return Err(WorkerTaskError::safe(
                        "client API key last-used flush failed",
                    ));
                }
            }
        })
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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
                NewClientApiKey {
                    id: id.as_str().to_owned(),
                    name: command.name,
                    label: command.label,
                    group_ids: command
                        .group_ids
                        .iter()
                        .map(|id| id.as_str().to_owned())
                        .collect(),
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
                        "group_ids",
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
                UpdateClientApiKeyDetails {
                    id: id.as_str().to_owned(),
                    name: command.name,
                    label: command.label,
                    group_ids: command
                        .group_ids
                        .iter()
                        .map(|id| id.as_str().to_owned())
                        .collect(),
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
                        "group_ids",
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
        groups: record
            .groups
            .into_iter()
            .map(|group| {
                Ok(AdminAccountGroupRef {
                    id: AccountGroupId::new(group.id)
                        .map_err(|_| admin_store_error(ENTITY, invalid("invalid group id")))?,
                    name: group.name,
                    color: gateway_admin::model::account_groups::AccountGroupColor::parse(
                        &group.color,
                    )
                    .ok_or_else(|| admin_store_error(ENTITY, invalid("invalid group color")))?,
                    enabled: group.enabled,
                })
            })
            .collect::<AdminStoreResult<Vec<_>>>()?,
        provider_kinds: record
            .provider_kinds
            .into_iter()
            .map(|provider| {
                ProviderKind::new(provider)
                    .map_err(|_| admin_store_error(ENTITY, invalid("invalid provider kind")))
            })
            .collect::<AdminStoreResult<Vec<_>>>()?,
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
           id, name, label, key, enabled, max_concurrency, requests_per_minute,
           last_used_at, created_at, updated_at
         ) values ($1, $2, $3, $4, true, $5, $6, null, now(), now())",
    )
    .bind(&key.id)
    .bind(&key.name)
    .bind(&key.label)
    .bind(&key.key)
    .bind(to_i64(key.max_concurrency)?)
    .bind(to_i64(key.requests_per_minute)?)
    .execute(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("insert client API key in transaction"))?;
    replace_client_api_key_groups_in_transaction(transaction, &key.id, &key.group_ids).await?;
    Ok(())
}

pub(crate) async fn update_client_api_key_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    key: &UpdateClientApiKeyDetails,
) -> StoreResult<()> {
    key.validate()?;
    let result = sqlx::query(
        "update client_api_keys
         set name = $2, label = $3, max_concurrency = $4,
             requests_per_minute = $5, updated_at = now()
         where id = $1",
    )
    .bind(&key.id)
    .bind(&key.name)
    .bind(&key.label)
    .bind(to_i64(key.max_concurrency)?)
    .bind(to_i64(key.requests_per_minute)?)
    .execute(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("update client API key in transaction"))?;
    require_changed(result.rows_affected(), &key.id)?;
    replace_client_api_key_groups_in_transaction(transaction, &key.id, &key.group_ids).await
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

async fn replace_client_api_key_groups_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    key_id: &str,
    group_ids: &[String],
) -> StoreResult<()> {
    validate_group_ids(group_ids)?;
    if !group_ids.is_empty() {
        let count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint from account_groups where id = any($1::text[])",
        )
        .bind(group_ids)
        .fetch_one(&mut **transaction)
        .await
        .map_err(|_| postgres_unavailable("validate client API key groups"))?;
        if usize::try_from(count).ok() != Some(group_ids.len()) {
            return Err(StoreError::NotFound {
                entity: "account group",
                id: "client API key group selection".to_owned(),
            });
        }
    }
    sqlx::query("delete from client_api_key_groups where client_api_key_id = $1")
        .bind(key_id)
        .execute(&mut **transaction)
        .await
        .map_err(|_| postgres_unavailable("delete client API key groups"))?;
    if !group_ids.is_empty() {
        sqlx::query(
            "insert into client_api_key_groups
             (client_api_key_id, account_group_id, created_at)
             select $1, group_id, now() from unnest($2::text[]) group_id",
        )
        .bind(key_id)
        .bind(group_ids)
        .execute(&mut **transaction)
        .await
        .map_err(|_| postgres_unavailable("insert client API key groups"))?;
    }
    Ok(())
}

fn validate_group_ids(group_ids: &[String]) -> StoreResult<()> {
    if group_ids.len() > 1000
        || group_ids.iter().collect::<BTreeSet<_>>().len() != group_ids.len()
        || group_ids
            .iter()
            .any(|id| AccountGroupId::new(id.clone()).is_err())
    {
        return Err(invalid("group IDs are invalid or duplicated"));
    }
    Ok(())
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
    row: (String, String, bool, i64, i64),
) -> StoreResult<ClientApiKeySecret> {
    Ok(ClientApiKeySecret {
        id: row.0,
        key: row.1,
        enabled: row.2,
        max_concurrency: to_u64(row.3)?,
        requests_per_minute: to_u64(row.4)?,
    })
}

fn client_record_from_row(row: &sqlx::postgres::PgRow) -> StoreResult<ClientApiKeyRecord> {
    use sqlx::Row as _;
    let groups: serde_json::Value = row
        .try_get("groups")
        .map_err(|_| invalid("invalid groups"))?;
    Ok(ClientApiKeyRecord {
        id: row.try_get("id").map_err(|_| invalid("invalid id"))?,
        name: row.try_get("name").map_err(|_| invalid("invalid name"))?,
        label: row.try_get("label").map_err(|_| invalid("invalid label"))?,
        groups: serde_json::from_value(groups).map_err(|_| invalid("invalid groups"))?,
        provider_kinds: row
            .try_get("provider_kinds")
            .map_err(|_| invalid("invalid provider kinds"))?,
        prefix: row
            .try_get("prefix")
            .map_err(|_| invalid("invalid prefix"))?,
        enabled: row
            .try_get("enabled")
            .map_err(|_| invalid("invalid enabled"))?,
        max_concurrency: to_u64(
            row.try_get("max_concurrency")
                .map_err(|_| invalid("invalid max concurrency"))?,
        )?,
        requests_per_minute: to_u64(
            row.try_get("requests_per_minute")
                .map_err(|_| invalid("invalid requests per minute"))?,
        )?,
        last_used_at: row
            .try_get("last_used_at")
            .map_err(|_| invalid("invalid last used at"))?,
        created_at: row
            .try_get("created_at")
            .map_err(|_| invalid("invalid created at"))?,
        updated_at: row
            .try_get("updated_at")
            .map_err(|_| invalid("invalid updated at"))?,
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
        let prefix = literal_prefix_pattern(search);
        statement.push(" and (lower(name) like ");
        statement.push_bind(prefix.clone());
        statement.push(" escape '\\'");
        statement.push(" or lower(coalesce(label, '')) like ");
        statement.push_bind(prefix.clone());
        statement.push(" escape '\\'");
        statement.push(" or lower(left(key, 10)) like ");
        statement.push_bind(prefix);
        statement.push(" escape '\\'");
        statement.push(")");
    }
}

async fn load_client_key_memberships(
    pool: &PgPool,
    records: &mut [ClientApiKeyRecord],
) -> StoreResult<()> {
    if records.is_empty() {
        return Ok(());
    }
    let ids = records
        .iter()
        .map(|record| record.id.clone())
        .collect::<Vec<_>>();
    let rows = sqlx::query(
        "with requested_keys(key_id) as (
           select unnest($1::text[])
         ),
         global_providers as (
           select coalesce(array_agg(distinct provider_kind order by provider_kind), '{}')
                    as provider_kinds
             from provider_accounts
         )
         select requested_keys.key_id,
                groups.id as group_id, groups.name as group_name, groups.color as group_color,
                groups.enabled as group_enabled,
                global_providers.provider_kinds as global_provider_kinds,
                coalesce(array_agg(distinct accounts.provider_kind order by accounts.provider_kind)
                  filter (where accounts.provider_kind is not null), '{}')
                  as group_provider_kinds
           from requested_keys
           cross join global_providers
           left join client_api_key_groups bindings
             on bindings.client_api_key_id = requested_keys.key_id
           left join account_groups groups on groups.id = bindings.account_group_id
           left join account_group_accounts memberships
             on memberships.account_group_id = groups.id and groups.enabled
           left join provider_accounts accounts
             on accounts.id = memberships.provider_account_id
          group by requested_keys.key_id, global_providers.provider_kinds,
                   groups.id, groups.name, groups.color, groups.enabled
          order by requested_keys.key_id, groups.id",
    )
    .bind(ids)
    .fetch_all(pool)
    .await
    .map_err(|_| postgres_unavailable("load client API key memberships"))?;
    let mut groups = BTreeMap::<String, Vec<ClientApiKeyGroupRecord>>::new();
    let mut providers = BTreeMap::<String, BTreeSet<String>>::new();
    for row in rows {
        use sqlx::Row as _;
        let key_id: String = row
            .try_get("key_id")
            .map_err(|_| invalid("invalid client API key membership"))?;
        let group_id: Option<String> = row
            .try_get("group_id")
            .map_err(|_| invalid("invalid client API key group"))?;
        if let Some(group_id) = group_id {
            groups
                .entry(key_id.clone())
                .or_default()
                .push(ClientApiKeyGroupRecord {
                    id: group_id,
                    name: row
                        .try_get("group_name")
                        .map_err(|_| invalid("invalid client API key group name"))?,
                    color: row
                        .try_get("group_color")
                        .map_err(|_| invalid("invalid client API key group color"))?,
                    enabled: row
                        .try_get("group_enabled")
                        .map_err(|_| invalid("invalid client API key group state"))?,
                });
            providers.entry(key_id).or_default().extend(
                row.try_get::<Vec<String>, _>("group_provider_kinds")
                    .map_err(|_| invalid("invalid client API key provider kinds"))?,
            );
        } else {
            providers.entry(key_id).or_default().extend(
                row.try_get::<Vec<String>, _>("global_provider_kinds")
                    .map_err(|_| invalid("invalid global provider kinds"))?,
            );
        }
    }
    for record in records {
        record.groups = groups.remove(&record.id).unwrap_or_default();
        record.provider_kinds = providers
            .remove(&record.id)
            .unwrap_or_default()
            .into_iter()
            .collect();
    }
    Ok(())
}

fn literal_prefix_pattern(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len().saturating_add(1));
    for character in value.to_lowercase().chars() {
        if matches!(character, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped.push('%');
    escaped
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
