//! `provider_instances` 的 PostgreSQL owner。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gateway_admin::{
    model::{
        MutationContext,
        catalog::{
            CreateProviderInstance as AdminCreateProviderInstance,
            DeleteProviderInstance as AdminDeleteProviderInstance,
            ProviderInstance as AdminProviderInstance, ProviderInstanceCatalog,
            ProviderInstanceDetail, ProviderInstanceMutation, SetProviderInstanceEnabled,
            UpdateProviderInstance as AdminUpdateProviderInstance,
        },
    },
    ports::store::{AdminStoreResult, CatalogStore},
};
use gateway_core::provider_ports::{
    ProviderInstanceCatalogPort, ProviderInstanceConfig, ProviderStoreError, ProviderStoreErrorKind,
};
use gateway_core::routing::{ProviderInstanceId, ProviderKind};
use sqlx::{PgPool, Postgres, Transaction};

use crate::{
    StoreError, StoreResult, admin_revision, admin_store_error, mutation_audit,
    postgres_unavailable, require_nonempty, store_revision,
};

use super::{ControlPlaneRepository, PgControlPlaneRepository};

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

impl ProviderInstanceCatalogPort for PgConfigCatalogRepository {
    fn list_instances<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        include_disabled: bool,
    ) -> futures::future::BoxFuture<'a, Result<Vec<ProviderInstanceConfig>, ProviderStoreError>>
    {
        Box::pin(async move {
            self.list_provider_instances(include_disabled)
                .await
                .map_err(|_| {
                    ProviderStoreError::new(
                        ProviderStoreErrorKind::Unavailable,
                        "list Provider instances",
                    )
                })?
                .into_iter()
                .filter(|instance| instance.provider_kind == provider_kind.as_str())
                .map(|instance| {
                    let id = ProviderInstanceId::new(instance.id).map_err(|_| {
                        ProviderStoreError::new(
                            ProviderStoreErrorKind::InvalidData,
                            "decode Provider instance ID",
                        )
                    })?;
                    let kind = ProviderKind::new(instance.provider_kind).map_err(|_| {
                        ProviderStoreError::new(
                            ProviderStoreErrorKind::InvalidData,
                            "decode Provider kind",
                        )
                    })?;
                    Ok(ProviderInstanceConfig::new(
                        id,
                        kind,
                        instance.base_url,
                        instance.enabled,
                    ))
                })
                .collect()
        })
    }
}

/// Admin 用例所需的 Provider instance 事务能力。
#[derive(Clone)]
pub struct PgAdminCatalogStore {
    catalog: PgConfigCatalogRepository,
    control_plane: PgControlPlaneRepository,
}

impl PgAdminCatalogStore {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            catalog: PgConfigCatalogRepository::new(pool.clone()),
            control_plane: PgControlPlaneRepository::new(pool),
        }
    }

    async fn revision(&self) -> AdminStoreResult<gateway_admin::model::Revision> {
        self.control_plane
            .load_control_plane()
            .await
            .map_err(|error| admin_store_error("provider instance", error))
            .and_then(|snapshot| admin_revision(snapshot.settings.config_revision))
    }

    async fn load_mutation(
        &self,
        revision: crate::Revision,
        id: &ProviderInstanceId,
    ) -> AdminStoreResult<ProviderInstanceMutation> {
        let instance = self
            .catalog
            .get_provider_instance(id.as_str())
            .await
            .map_err(|error| admin_store_error("provider instance", error))?
            .map(admin_provider_instance)
            .transpose()?;
        Ok(ProviderInstanceMutation {
            config_revision: admin_revision(revision)?,
            instance,
        })
    }
}

#[async_trait]
impl CatalogStore for PgAdminCatalogStore {
    async fn list_provider_instances(
        &self,
        include_disabled: bool,
    ) -> AdminStoreResult<ProviderInstanceCatalog> {
        let config_revision = self.revision().await?;
        let items = self
            .catalog
            .list_provider_instances(include_disabled)
            .await
            .map_err(|error| admin_store_error("provider instance", error))?
            .into_iter()
            .map(admin_provider_instance)
            .collect::<AdminStoreResult<Vec<_>>>()?;
        Ok(ProviderInstanceCatalog {
            config_revision,
            items,
        })
    }

    async fn load_provider_instance(
        &self,
        id: &ProviderInstanceId,
    ) -> AdminStoreResult<Option<ProviderInstanceDetail>> {
        let (revision, instance) = futures::try_join!(self.revision(), async {
            self.catalog
                .get_provider_instance(id.as_str())
                .await
                .map_err(|error| admin_store_error("provider instance", error))?
                .map(admin_provider_instance)
                .transpose()
        },)?;
        Ok(instance.map(|item| ProviderInstanceDetail {
            config_revision: revision,
            item,
        }))
    }

    async fn create_provider_instance(
        &self,
        command: AdminCreateProviderInstance,
        context: &MutationContext,
    ) -> AdminStoreResult<ProviderInstanceMutation> {
        let id = command.id;
        let revision = self
            .control_plane
            .create_provider_instance(
                store_revision(command.expected_config_revision)?,
                NewProviderInstance {
                    id: id.as_str().to_owned(),
                    provider_kind: command.provider_kind.as_str().to_owned(),
                    name: command.name,
                    base_url: command.base_url,
                },
                mutation_audit(
                    context,
                    "create",
                    "provider_instance",
                    id.as_str(),
                    vec![
                        "provider_kind".to_owned(),
                        "name".to_owned(),
                        "base_url".to_owned(),
                        "enabled".to_owned(),
                    ],
                ),
            )
            .await
            .map_err(|error| admin_store_error("provider instance", error))?;
        self.load_mutation(revision, &id).await
    }

    async fn update_provider_instance(
        &self,
        command: AdminUpdateProviderInstance,
        context: &MutationContext,
    ) -> AdminStoreResult<ProviderInstanceMutation> {
        let id = command.id;
        let revision = self
            .control_plane
            .update_provider_instance(
                store_revision(command.expected_config_revision)?,
                UpdateProviderInstanceDetails {
                    id: id.as_str().to_owned(),
                    name: command.name,
                    base_url: command.base_url,
                },
                mutation_audit(
                    context,
                    "update",
                    "provider_instance",
                    id.as_str(),
                    vec!["name".to_owned(), "base_url".to_owned()],
                ),
            )
            .await
            .map_err(|error| admin_store_error("provider instance", error))?;
        self.load_mutation(revision, &id).await
    }

    async fn set_provider_instance_enabled(
        &self,
        command: SetProviderInstanceEnabled,
        context: &MutationContext,
    ) -> AdminStoreResult<ProviderInstanceMutation> {
        let id = command.id;
        let revision = self
            .control_plane
            .set_provider_instance_enabled(
                store_revision(command.expected_config_revision)?,
                id.as_str(),
                command.enabled,
                mutation_audit(
                    context,
                    if command.enabled { "enable" } else { "disable" },
                    "provider_instance",
                    id.as_str(),
                    vec!["enabled".to_owned()],
                ),
            )
            .await
            .map_err(|error| admin_store_error("provider instance", error))?;
        self.load_mutation(revision, &id).await
    }

    async fn delete_provider_instance(
        &self,
        command: AdminDeleteProviderInstance,
        context: &MutationContext,
    ) -> AdminStoreResult<gateway_admin::model::Revision> {
        self.control_plane
            .delete_provider_instance(
                store_revision(command.expected_config_revision)?,
                command.id.as_str(),
                mutation_audit(
                    context,
                    "delete",
                    "provider_instance",
                    command.id.as_str(),
                    Vec::new(),
                ),
            )
            .await
            .map_err(|error| admin_store_error("provider instance", error))
            .and_then(admin_revision)
    }
}

fn admin_provider_instance(
    record: ProviderInstanceRecord,
) -> AdminStoreResult<AdminProviderInstance> {
    Ok(AdminProviderInstance {
        id: ProviderInstanceId::new(record.id).map_err(|_| {
            admin_store_error(
                "provider instance",
                StoreError::InvalidData {
                    entity: ENTITY,
                    message: "persisted instance ID is invalid".to_owned(),
                },
            )
        })?,
        provider_kind: ProviderKind::new(record.provider_kind).map_err(|_| {
            admin_store_error(
                "provider instance",
                StoreError::InvalidData {
                    entity: ENTITY,
                    message: "persisted provider kind is invalid".to_owned(),
                },
            )
        })?,
        name: record.name,
        base_url: record.base_url,
        enabled: record.enabled,
        created_at: record.created_at,
        updated_at: record.updated_at,
    })
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
