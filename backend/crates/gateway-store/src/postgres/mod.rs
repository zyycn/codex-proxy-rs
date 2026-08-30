//! PostgreSQL 业务表的 adapters。

use async_trait::async_trait;
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};

use crate::{
    POSTGRES_IDLE_TRANSACTION_TIMEOUT, POSTGRES_LOCK_TIMEOUT, POSTGRES_STATEMENT_TIMEOUT, Revision,
    StoreBackend, StoreError, StorePoolConfig, StoreResult, postgres_unavailable,
};

mod account_groups;
mod admin_security_audit;
mod admission_recovery;
mod backup;
mod client_keys;
mod execution;
mod execution_buffer;
mod observability;
mod ops_events;
mod provider_accounts;
mod retention;
mod runtime_settings;
mod snapshot;
mod usage_facts;

pub use account_groups::*;
pub use admin_security_audit::*;
pub use admission_recovery::*;
pub use backup::*;
pub use client_keys::*;
pub use execution::*;
pub use execution_buffer::*;
pub use observability::*;
pub use ops_events::*;
pub use provider_accounts::*;
pub use retention::*;
pub use runtime_settings::*;
pub use snapshot::*;
pub(crate) use usage_facts::{completed_usage_fact_predicate, push_completed_usage_fact_filter};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

/// 建立 PostgreSQL pool 并只执行冻结的 migration 集。
pub async fn connect_and_migrate(
    database_url: &str,
    pool_config: StorePoolConfig,
) -> StoreResult<PgPool> {
    if database_url.trim().is_empty() {
        return Err(postgres_unavailable("connect PostgreSQL"));
    }
    pool_config.validate()?;
    let connect_options = database_url
        .parse::<PgConnectOptions>()
        .map_err(|_| postgres_unavailable("parse PostgreSQL connection options"))?;
    let migration_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(
            connect_options
                .clone()
                .application_name("codex-proxy-rs:migration"),
        )
        .await
        .map_err(|_| postgres_unavailable("connect PostgreSQL for migrations"))?;
    if let Err(error) = MIGRATOR.run(&migration_pool).await {
        migration_pool.close().await;
        return Err(StoreError::Unavailable {
            backend: StoreBackend::PostgreSql,
            message: format!("apply PostgreSQL migrations: {error}"),
        });
    }
    migration_pool.close().await;

    let statement_timeout = postgres_duration_setting(POSTGRES_STATEMENT_TIMEOUT);
    let lock_timeout = postgres_duration_setting(POSTGRES_LOCK_TIMEOUT);
    let idle_in_transaction_session_timeout =
        postgres_duration_setting(POSTGRES_IDLE_TRANSACTION_TIMEOUT);
    let pool = PgPoolOptions::new()
        .max_connections(pool_config.max_connections)
        .acquire_timeout(std::time::Duration::from_secs(
            pool_config.acquire_timeout_seconds,
        ))
        .after_connect(move |connection, _metadata| {
            let statement_timeout = statement_timeout.clone();
            let lock_timeout = lock_timeout.clone();
            let idle_in_transaction_session_timeout = idle_in_transaction_session_timeout.clone();
            Box::pin(async move {
                sqlx::query(
                    "select set_config('statement_timeout', $1, false),
                            set_config('lock_timeout', $2, false),
                            set_config('idle_in_transaction_session_timeout', $3, false)",
                )
                .bind(statement_timeout)
                .bind(lock_timeout)
                .bind(idle_in_transaction_session_timeout)
                .execute(connection)
                .await?;
                Ok(())
            })
        })
        .connect_with(connect_options.application_name("codex-proxy-rs"))
        .await
        .map_err(|_| postgres_unavailable("connect PostgreSQL"))?;
    Ok(pool)
}

fn postgres_duration_setting(duration: std::time::Duration) -> String {
    format!("{}ms", duration.as_millis())
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
        replacement: ControlPlaneReplacement,
    ) -> StoreResult<ControlPlaneSnapshot>;

    /// 更新 admin_api_key 字段并推进 config revision。
    async fn replace_admin_api_key(
        &self,
        admin_api_key: Option<String>,
        audit: AdminAuditEvent,
    ) -> StoreResult<Revision>;

    async fn create_client_api_key(
        &self,
        key: NewClientApiKey,
        audit: AdminAuditEvent,
    ) -> StoreResult<Revision>;

    async fn update_client_api_key(
        &self,
        key: UpdateClientApiKeyDetails,
        audit: AdminAuditEvent,
    ) -> StoreResult<Revision>;

    async fn set_client_api_key_enabled(
        &self,
        id: &str,
        enabled: bool,
        audit: AdminAuditEvent,
    ) -> StoreResult<Revision>;

    async fn delete_client_api_key(
        &self,
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
        let settings = load_runtime_settings_from_pool(&self.pool).await?;
        Ok(ControlPlaneSnapshot { settings })
    }

    async fn replace_control_plane(
        &self,
        replacement: ControlPlaneReplacement,
    ) -> StoreResult<ControlPlaneSnapshot> {
        replacement.settings.validate()?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| postgres_unavailable("begin control plane replacement"))?;
        let result = async {
            let revision =
                update_runtime_settings_in_transaction(&mut transaction, &replacement.settings)
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

    async fn replace_admin_api_key(
        &self,
        admin_api_key: Option<String>,
        audit: AdminAuditEvent,
    ) -> StoreResult<Revision> {
        self.apply_targeted_mutation(ControlPlaneMutation::SetAdminApiKey(admin_api_key), audit)
            .await
    }

    async fn create_client_api_key(
        &self,
        key: NewClientApiKey,
        audit: AdminAuditEvent,
    ) -> StoreResult<Revision> {
        self.apply_targeted_mutation(ControlPlaneMutation::CreateClientApiKey(key), audit)
            .await
    }

    async fn update_client_api_key(
        &self,
        key: UpdateClientApiKeyDetails,
        audit: AdminAuditEvent,
    ) -> StoreResult<Revision> {
        self.apply_targeted_mutation(ControlPlaneMutation::UpdateClientApiKey(key), audit)
            .await
    }

    async fn set_client_api_key_enabled(
        &self,
        id: &str,
        enabled: bool,
        audit: AdminAuditEvent,
    ) -> StoreResult<Revision> {
        self.apply_targeted_mutation(
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
        id: &str,
        audit: AdminAuditEvent,
    ) -> StoreResult<Revision> {
        self.apply_targeted_mutation(
            ControlPlaneMutation::DeleteClientApiKey(id.to_owned()),
            audit,
        )
        .await
    }
}

enum ControlPlaneMutation {
    CreateClientApiKey(NewClientApiKey),
    UpdateClientApiKey(UpdateClientApiKeyDetails),
    SetClientApiKeyEnabled { id: String, enabled: bool },
    DeleteClientApiKey(String),
    SetAdminApiKey(Option<String>),
}

impl PgControlPlaneRepository {
    async fn apply_targeted_mutation(
        &self,
        mutation: ControlPlaneMutation,
        mut audit: AdminAuditEvent,
    ) -> StoreResult<Revision> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| postgres_unavailable("begin targeted control plane mutation"))?;
        let result = async {
            let revision = bump_config_revision_in_transaction(&mut transaction).await?;
            match mutation {
                ControlPlaneMutation::CreateClientApiKey(key) => {
                    audit.changed_fields.push(if key.group_ids.is_empty() {
                        "routing_scope:all".to_owned()
                    } else {
                        "routing_scope:groups".to_owned()
                    });
                    insert_client_api_key_in_transaction(&mut transaction, &key).await?;
                }
                ControlPlaneMutation::UpdateClientApiKey(key) => {
                    let previously_restricted = sqlx::query_scalar::<_, bool>(
                        "select exists(
                           select 1 from client_api_key_groups where client_api_key_id = $1
                         )",
                    )
                    .bind(&key.id)
                    .fetch_one(&mut *transaction)
                    .await
                    .map_err(|_| {
                        postgres_unavailable("load client API key routing scope for audit")
                    })?;
                    if previously_restricted && key.group_ids.is_empty() {
                        audit
                            .changed_fields
                            .push("routing_scope:groups->all".to_owned());
                    } else if !previously_restricted && !key.group_ids.is_empty() {
                        audit
                            .changed_fields
                            .push("routing_scope:all->groups".to_owned());
                    }
                    update_client_api_key_in_transaction(&mut transaction, &key).await?;
                }
                ControlPlaneMutation::SetClientApiKeyEnabled { id, enabled } => {
                    set_client_api_key_enabled_in_transaction(&mut transaction, &id, enabled)
                        .await?;
                }
                ControlPlaneMutation::DeleteClientApiKey(id) => {
                    delete_client_api_key_in_transaction(&mut transaction, &id).await?;
                }
                ControlPlaneMutation::SetAdminApiKey(key) => {
                    update_admin_api_key_in_transaction(&mut transaction, key).await?;
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
