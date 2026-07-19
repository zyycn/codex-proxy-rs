//! 八张终态业务表的 PostgreSQL adapters。

use async_trait::async_trait;
use sqlx::{PgPool, postgres::PgPoolOptions};

use crate::{Revision, StoreBackend, StoreError, StoreResult, postgres_unavailable};

mod admin_security_audit;
mod admission_recovery;
mod client_keys;
mod config_catalog;
mod execution;
mod history;
mod observability;
mod ops_events;
mod provider_accounts;
mod retention;
mod runtime_settings;
mod snapshot;

pub use admin_security_audit::*;
pub use admission_recovery::*;
pub use client_keys::*;
pub use config_catalog::*;
pub use execution::*;
pub use history::*;
pub use observability::*;
pub use ops_events::*;
pub use provider_accounts::*;
pub use retention::*;
pub use runtime_settings::*;
pub use snapshot::*;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

/// 建立 PostgreSQL pool 并只执行冻结的 `0001_initial.sql` migration 集。
pub async fn connect_and_migrate(database_url: &str) -> StoreResult<PgPool> {
    if database_url.trim().is_empty() {
        return Err(postgres_unavailable("connect PostgreSQL"));
    }
    let pool = PgPoolOptions::new()
        .connect(database_url)
        .await
        .map_err(|_| postgres_unavailable("connect PostgreSQL"))?;
    if let Err(error) = MIGRATOR.run(&pool).await {
        pool.close().await;
        return Err(StoreError::Unavailable {
            backend: StoreBackend::PostgreSql,
            message: format!("apply PostgreSQL migrations: {error}"),
        });
    }
    Ok(pool)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlPlaneSnapshot {
    pub settings: RuntimeSettings,
}

#[derive(Debug, Clone)]
pub struct ControlPlaneReplacement {
    pub settings: RuntimeSettingsUpdate,
    pub audit: AdminAuditEvent,
}

#[async_trait]
pub trait ControlPlaneRepository: Send + Sync {
    async fn load_control_plane(&self) -> StoreResult<ControlPlaneSnapshot>;

    async fn replace_control_plane(
        &self,
        expected_revision: Revision,
        replacement: ControlPlaneReplacement,
    ) -> StoreResult<ControlPlaneSnapshot>;

    async fn create_provider_instance(
        &self,
        expected_revision: Revision,
        instance: NewProviderInstance,
        audit: AdminAuditEvent,
    ) -> StoreResult<Revision>;

    async fn update_provider_instance(
        &self,
        expected_revision: Revision,
        instance: UpdateProviderInstanceDetails,
        audit: AdminAuditEvent,
    ) -> StoreResult<Revision>;

    async fn set_provider_instance_enabled(
        &self,
        expected_revision: Revision,
        id: &str,
        enabled: bool,
        audit: AdminAuditEvent,
    ) -> StoreResult<Revision>;

    async fn delete_provider_instance(
        &self,
        expected_revision: Revision,
        id: &str,
        audit: AdminAuditEvent,
    ) -> StoreResult<Revision>;

    async fn create_client_api_key(
        &self,
        expected_revision: Revision,
        key: NewClientApiKey,
        audit: AdminAuditEvent,
    ) -> StoreResult<Revision>;

    async fn update_client_api_key(
        &self,
        expected_revision: Revision,
        key: UpdateClientApiKeyDetails,
        audit: AdminAuditEvent,
    ) -> StoreResult<Revision>;

    async fn set_client_api_key_enabled(
        &self,
        expected_revision: Revision,
        id: &str,
        enabled: bool,
        audit: AdminAuditEvent,
    ) -> StoreResult<Revision>;

    async fn delete_client_api_key(
        &self,
        expected_revision: Revision,
        id: &str,
        audit: AdminAuditEvent,
    ) -> StoreResult<Revision>;
}

#[derive(Clone)]
pub struct PgControlPlaneRepository {
    pool: PgPool,
}

impl PgControlPlaneRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ControlPlaneRepository for PgControlPlaneRepository {
    async fn load_control_plane(&self) -> StoreResult<ControlPlaneSnapshot> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| postgres_unavailable("begin control plane snapshot"))?;
        sqlx::query("set transaction isolation level repeatable read read only")
            .execute(&mut *transaction)
            .await
            .map_err(|_| postgres_unavailable("configure control plane snapshot"))?;
        let snapshot = load_control_plane_in_transaction(&mut transaction).await?;
        transaction
            .commit()
            .await
            .map_err(|_| postgres_unavailable("commit control plane snapshot"))?;
        Ok(snapshot)
    }

    async fn replace_control_plane(
        &self,
        expected_revision: Revision,
        replacement: ControlPlaneReplacement,
    ) -> StoreResult<ControlPlaneSnapshot> {
        replacement.settings.validate()?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| postgres_unavailable("begin control plane replacement"))?;
        let result = async {
            let revision = update_runtime_settings_in_transaction(
                &mut transaction,
                expected_revision,
                &replacement.settings,
            )
            .await?;
            append_admin_audit_event_in_transaction(&mut transaction, replacement.audit, revision)
                .await?;
            load_control_plane_in_transaction(&mut transaction).await
        }
        .await;
        match result {
            Ok(snapshot) => {
                transaction
                    .commit()
                    .await
                    .map_err(|_| postgres_unavailable("commit control plane replacement"))?;
                Ok(snapshot)
            }
            Err(error) => {
                transaction
                    .rollback()
                    .await
                    .map_err(|_| postgres_unavailable("rollback control plane replacement"))?;
                Err(error)
            }
        }
    }

    async fn create_provider_instance(
        &self,
        expected_revision: Revision,
        instance: NewProviderInstance,
        audit: AdminAuditEvent,
    ) -> StoreResult<Revision> {
        self.apply_targeted_mutation(
            expected_revision,
            ControlPlaneMutation::CreateProviderInstance(instance),
            audit,
        )
        .await
    }

    async fn update_provider_instance(
        &self,
        expected_revision: Revision,
        instance: UpdateProviderInstanceDetails,
        audit: AdminAuditEvent,
    ) -> StoreResult<Revision> {
        self.apply_targeted_mutation(
            expected_revision,
            ControlPlaneMutation::UpdateProviderInstance(instance),
            audit,
        )
        .await
    }

    async fn set_provider_instance_enabled(
        &self,
        expected_revision: Revision,
        id: &str,
        enabled: bool,
        audit: AdminAuditEvent,
    ) -> StoreResult<Revision> {
        self.apply_targeted_mutation(
            expected_revision,
            ControlPlaneMutation::SetProviderInstanceEnabled {
                id: id.to_owned(),
                enabled,
            },
            audit,
        )
        .await
    }

    async fn delete_provider_instance(
        &self,
        expected_revision: Revision,
        id: &str,
        audit: AdminAuditEvent,
    ) -> StoreResult<Revision> {
        self.apply_targeted_mutation(
            expected_revision,
            ControlPlaneMutation::DeleteProviderInstance(id.to_owned()),
            audit,
        )
        .await
    }

    async fn create_client_api_key(
        &self,
        expected_revision: Revision,
        key: NewClientApiKey,
        audit: AdminAuditEvent,
    ) -> StoreResult<Revision> {
        self.apply_targeted_mutation(
            expected_revision,
            ControlPlaneMutation::CreateClientApiKey(key),
            audit,
        )
        .await
    }

    async fn update_client_api_key(
        &self,
        expected_revision: Revision,
        key: UpdateClientApiKeyDetails,
        audit: AdminAuditEvent,
    ) -> StoreResult<Revision> {
        self.apply_targeted_mutation(
            expected_revision,
            ControlPlaneMutation::UpdateClientApiKey(key),
            audit,
        )
        .await
    }

    async fn set_client_api_key_enabled(
        &self,
        expected_revision: Revision,
        id: &str,
        enabled: bool,
        audit: AdminAuditEvent,
    ) -> StoreResult<Revision> {
        self.apply_targeted_mutation(
            expected_revision,
            ControlPlaneMutation::SetClientApiKeyEnabled {
                id: id.to_owned(),
                enabled,
            },
            audit,
        )
        .await
    }

    async fn delete_client_api_key(
        &self,
        expected_revision: Revision,
        id: &str,
        audit: AdminAuditEvent,
    ) -> StoreResult<Revision> {
        self.apply_targeted_mutation(
            expected_revision,
            ControlPlaneMutation::DeleteClientApiKey(id.to_owned()),
            audit,
        )
        .await
    }
}

enum ControlPlaneMutation {
    CreateProviderInstance(NewProviderInstance),
    UpdateProviderInstance(UpdateProviderInstanceDetails),
    SetProviderInstanceEnabled { id: String, enabled: bool },
    DeleteProviderInstance(String),
    CreateClientApiKey(NewClientApiKey),
    UpdateClientApiKey(UpdateClientApiKeyDetails),
    SetClientApiKeyEnabled { id: String, enabled: bool },
    DeleteClientApiKey(String),
}

impl PgControlPlaneRepository {
    async fn apply_targeted_mutation(
        &self,
        expected_revision: Revision,
        mutation: ControlPlaneMutation,
        audit: AdminAuditEvent,
    ) -> StoreResult<Revision> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| postgres_unavailable("begin targeted control plane mutation"))?;
        let result = async {
            let revision =
                bump_config_revision_in_transaction(&mut transaction, expected_revision).await?;
            match mutation {
                ControlPlaneMutation::CreateProviderInstance(instance) => {
                    insert_provider_instance_in_transaction(&mut transaction, &instance).await?;
                }
                ControlPlaneMutation::UpdateProviderInstance(instance) => {
                    update_provider_instance_in_transaction(&mut transaction, &instance).await?;
                }
                ControlPlaneMutation::SetProviderInstanceEnabled { id, enabled } => {
                    set_provider_instance_enabled_in_transaction(&mut transaction, &id, enabled)
                        .await?;
                }
                ControlPlaneMutation::DeleteProviderInstance(id) => {
                    delete_provider_instance_in_transaction(&mut transaction, &id).await?;
                }
                ControlPlaneMutation::CreateClientApiKey(key) => {
                    insert_client_api_key_in_transaction(&mut transaction, &key).await?;
                }
                ControlPlaneMutation::UpdateClientApiKey(key) => {
                    update_client_api_key_in_transaction(&mut transaction, &key).await?;
                }
                ControlPlaneMutation::SetClientApiKeyEnabled { id, enabled } => {
                    set_client_api_key_enabled_in_transaction(&mut transaction, &id, enabled)
                        .await?;
                }
                ControlPlaneMutation::DeleteClientApiKey(id) => {
                    delete_client_api_key_in_transaction(&mut transaction, &id).await?;
                }
            }
            append_admin_audit_event_in_transaction(&mut transaction, audit, revision).await?;
            Ok(revision)
        }
        .await;
        match result {
            Ok(revision) => {
                transaction
                    .commit()
                    .await
                    .map_err(|_| postgres_unavailable("commit targeted control plane mutation"))?;
                Ok(revision)
            }
            Err(error) => {
                transaction.rollback().await.map_err(|_| {
                    postgres_unavailable("rollback targeted control plane mutation")
                })?;
                Err(error)
            }
        }
    }
}

async fn load_control_plane_in_transaction(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> StoreResult<ControlPlaneSnapshot> {
    let settings = load_runtime_settings_in_transaction(transaction).await?;
    Ok(ControlPlaneSnapshot { settings })
}
