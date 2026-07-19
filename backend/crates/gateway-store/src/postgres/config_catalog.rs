//! `provider_instances` 的 PostgreSQL owner。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction};

use crate::{StoreError, StoreResult, postgres_unavailable, require_nonempty};

const ENTITY: &str = "provider instance";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderInstanceRecord {
    pub id: String,
    pub provider_kind: String,
    pub name: String,
    pub base_url: String,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewProviderInstance {
    pub id: String,
    pub provider_kind: String,
    pub name: String,
    pub base_url: String,
}

impl NewProviderInstance {
    pub fn validate(&self) -> StoreResult<()> {
        validate_instance_fields(&self.id, &self.provider_kind, &self.name, &self.base_url)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateProviderInstance {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateProviderInstanceDetails {
    pub id: String,
    pub name: String,
    pub base_url: String,
}

impl UpdateProviderInstanceDetails {
    pub fn validate(&self) -> StoreResult<()> {
        require_nonempty(ENTITY, "id", &self.id)?;
        require_nonempty(ENTITY, "name", &self.name)?;
        require_nonempty(ENTITY, "base_url", &self.base_url)
    }
}

#[async_trait]
pub trait ConfigCatalogRepository: Send + Sync {
    async fn get_provider_instance(&self, id: &str) -> StoreResult<Option<ProviderInstanceRecord>>;
    async fn list_provider_instances(
        &self,
        include_disabled: bool,
    ) -> StoreResult<Vec<ProviderInstanceRecord>>;
    async fn insert_provider_instance(&self, instance: NewProviderInstance) -> StoreResult<()>;
    async fn update_provider_instance(&self, instance: UpdateProviderInstance)
    -> StoreResult<bool>;
    async fn delete_provider_instance(&self, id: &str) -> StoreResult<bool>;
}

#[derive(Clone)]
pub struct PgConfigCatalogRepository {
    pool: PgPool,
}

impl PgConfigCatalogRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ConfigCatalogRepository for PgConfigCatalogRepository {
    async fn get_provider_instance(&self, id: &str) -> StoreResult<Option<ProviderInstanceRecord>> {
        require_nonempty(ENTITY, "id", id)?;
        let row = sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                String,
                bool,
                DateTime<Utc>,
                DateTime<Utc>,
            ),
        >(
            "select id, provider_kind, name, base_url, enabled, created_at, updated_at
             from provider_instances where id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("get provider instance"))?;
        Ok(row.map(provider_instance_from_row))
    }

    async fn list_provider_instances(
        &self,
        include_disabled: bool,
    ) -> StoreResult<Vec<ProviderInstanceRecord>> {
        let rows = sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                String,
                bool,
                DateTime<Utc>,
                DateTime<Utc>,
            ),
        >(
            "select id, provider_kind, name, base_url, enabled, created_at, updated_at
             from provider_instances
             where $1 or enabled
             order by provider_kind, name, id",
        )
        .bind(include_disabled)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("list provider instances"))?;
        Ok(rows.into_iter().map(provider_instance_from_row).collect())
    }

    async fn insert_provider_instance(&self, instance: NewProviderInstance) -> StoreResult<()> {
        instance.validate()?;
        sqlx::query(
            "insert into provider_instances
             (id, provider_kind, name, base_url, enabled, created_at, updated_at)
             values ($1, $2, $3, $4, true, now(), now())",
        )
        .bind(instance.id)
        .bind(instance.provider_kind)
        .bind(instance.name)
        .bind(instance.base_url)
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("insert provider instance"))?;
        Ok(())
    }

    async fn update_provider_instance(
        &self,
        instance: UpdateProviderInstance,
    ) -> StoreResult<bool> {
        require_nonempty(ENTITY, "id", &instance.id)?;
        require_nonempty(ENTITY, "name", &instance.name)?;
        require_nonempty(ENTITY, "base_url", &instance.base_url)?;
        let result = sqlx::query(
            "update provider_instances
             set name = $2, base_url = $3, enabled = $4, updated_at = now()
             where id = $1",
        )
        .bind(instance.id)
        .bind(instance.name)
        .bind(instance.base_url)
        .bind(instance.enabled)
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("update provider instance"))?;
        Ok(result.rows_affected() == 1)
    }

    async fn delete_provider_instance(&self, id: &str) -> StoreResult<bool> {
        require_nonempty(ENTITY, "id", id)?;
        let result = sqlx::query("delete from provider_instances where id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|_| postgres_unavailable("delete provider instance"))?;
        Ok(result.rows_affected() == 1)
    }
}

pub(crate) async fn insert_provider_instance_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    instance: &NewProviderInstance,
) -> StoreResult<()> {
    instance.validate()?;
    sqlx::query(
        "insert into provider_instances
         (id, provider_kind, name, base_url, enabled, created_at, updated_at)
         values ($1, $2, $3, $4, true, now(), now())",
    )
    .bind(&instance.id)
    .bind(&instance.provider_kind)
    .bind(&instance.name)
    .bind(&instance.base_url)
    .execute(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("insert provider instance in transaction"))?;
    Ok(())
}

pub(crate) async fn update_provider_instance_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    instance: &UpdateProviderInstanceDetails,
) -> StoreResult<()> {
    instance.validate()?;
    let result = sqlx::query(
        "update provider_instances
         set name = $2, base_url = $3, updated_at = now()
         where id = $1",
    )
    .bind(&instance.id)
    .bind(&instance.name)
    .bind(&instance.base_url)
    .execute(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("update provider instance in transaction"))?;
    require_changed(result.rows_affected(), &instance.id)
}

pub(crate) async fn set_provider_instance_enabled_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    id: &str,
    enabled: bool,
) -> StoreResult<()> {
    require_nonempty(ENTITY, "id", id)?;
    let result =
        sqlx::query("update provider_instances set enabled = $2, updated_at = now() where id = $1")
            .bind(id)
            .bind(enabled)
            .execute(&mut **transaction)
            .await
            .map_err(|_| postgres_unavailable("set provider instance state in transaction"))?;
    require_changed(result.rows_affected(), id)
}

pub(crate) async fn delete_provider_instance_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    id: &str,
) -> StoreResult<()> {
    require_nonempty(ENTITY, "id", id)?;
    let result = sqlx::query("delete from provider_instances where id = $1")
        .bind(id)
        .execute(&mut **transaction)
        .await
        .map_err(|_| postgres_unavailable("delete provider instance in transaction"))?;
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

type ProviderInstanceRow = (
    String,
    String,
    String,
    String,
    bool,
    DateTime<Utc>,
    DateTime<Utc>,
);

fn provider_instance_from_row(row: ProviderInstanceRow) -> ProviderInstanceRecord {
    ProviderInstanceRecord {
        id: row.0,
        provider_kind: row.1,
        name: row.2,
        base_url: row.3,
        enabled: row.4,
        created_at: row.5,
        updated_at: row.6,
    }
}

fn validate_instance_fields(id: &str, kind: &str, name: &str, base_url: &str) -> StoreResult<()> {
    require_nonempty(ENTITY, "id", id)?;
    require_nonempty(ENTITY, "provider_kind", kind)?;
    require_nonempty(ENTITY, "name", name)?;
    require_nonempty(ENTITY, "base_url", base_url)?;
    let valid_kind = kind.bytes().enumerate().all(|(index, byte)| {
        byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || (index > 0 && matches!(byte, b'-' | b'_'))
    });
    if !valid_kind {
        return Err(StoreError::InvalidData {
            entity: ENTITY,
            message: "provider_kind must be a stable lowercase slug".to_owned(),
        });
    }
    Ok(())
}
