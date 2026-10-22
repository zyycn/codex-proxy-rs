//! 控制面设置与 Key 修改的 revision 锁、条件写入和同事务审计

use async_trait::async_trait;
use sqlx::PgPool;

use crate::{Revision, StoreError, StoreResult, postgres_unavailable};

use super::{
    AdminAuditEvent, NewClientApiKey, RuntimeSettings, RuntimeSettingsUpdate,
    UpdateClientApiKeyDetails, append_admin_audit_event_in_transaction,
    bump_config_revision_in_transaction, delete_client_api_key_in_transaction,
    insert_client_api_key_in_transaction, load_runtime_settings_from_pool,
    load_runtime_settings_in_transaction, set_client_api_key_enabled_in_transaction,
    update_admin_api_key_in_transaction, update_client_api_key_in_transaction,
    update_runtime_settings_in_transaction,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlPlaneSnapshot {
    pub settings: RuntimeSettings,
}

#[derive(Debug, Clone)]
pub struct ControlPlaneReplacement {
    pub expected_revision: Revision,
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

    /// 更新 admin_api_key 字段并推进 config revision
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
    pub(super) pool: PgPool,
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
        let mut transaction =
            self.pool.begin().await.map_err(|source| {
                postgres_unavailable("begin control plane replacement", source)
            })?;
        let result: StoreResult<_> = async {
            // 锁住所有配置写入共同使用的行，版本检查与修改不能被其他事务穿插
            let current = sqlx::query_scalar::<_, i64>(
                "select config_revision from runtime_settings where id = 1 for update",
            )
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| postgres_unavailable("lock control plane revision", source))?;
            if u64::try_from(current).ok() != Some(replacement.expected_revision.get()) {
                return Err(StoreError::Conflict {
                    source: None,
                    entity: "runtime settings",
                    id: "1".to_owned(),
                    kind: crate::ConflictKind::StaleRevision,
                });
            }
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
                transaction.commit().await.map_err(|source| {
                    postgres_unavailable("commit control plane replacement", source)
                })?;
                Ok(snapshot)
            }
            Err(error) => {
                let error = match transaction.rollback().await {
                    Ok(()) => error,
                    Err(cleanup) => error.with_cleanup(cleanup),
                };
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
        self.apply_targeted_mutation(
            ControlPlaneMutation::CreateClientApiKey(key, Default::default()),
            audit,
        )
        .await
    }

    async fn update_client_api_key(
        &self,
        key: UpdateClientApiKeyDetails,
        audit: AdminAuditEvent,
    ) -> StoreResult<Revision> {
        self.apply_targeted_mutation(
            ControlPlaneMutation::UpdateClientApiKey(key, Default::default()),
            audit,
        )
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
    CreateClientApiKey(
        NewClientApiKey,
        gateway_admin::model::upstream_quota::QuotaSharingUpdate,
    ),
    UpdateClientApiKey(
        UpdateClientApiKeyDetails,
        gateway_admin::model::upstream_quota::QuotaSharingUpdate,
    ),
    SetClientApiKeyEnabled {
        id: String,
        enabled: bool,
    },
    DeleteClientApiKey(String),
    SetAdminApiKey(Option<String>),
}

impl PgControlPlaneRepository {
    pub(super) async fn create_client_api_key_with_quota_sharing(
        &self,
        key: NewClientApiKey,
        sharing: gateway_admin::model::upstream_quota::QuotaSharingUpdate,
        audit: AdminAuditEvent,
    ) -> StoreResult<Revision> {
        self.apply_targeted_mutation(
            ControlPlaneMutation::CreateClientApiKey(key, sharing),
            audit,
        )
        .await
    }

    pub(super) async fn update_client_api_key_with_quota_sharing(
        &self,
        key: UpdateClientApiKeyDetails,
        sharing: gateway_admin::model::upstream_quota::QuotaSharingUpdate,
        audit: AdminAuditEvent,
    ) -> StoreResult<Revision> {
        self.apply_targeted_mutation(
            ControlPlaneMutation::UpdateClientApiKey(key, sharing),
            audit,
        )
        .await
    }

    async fn apply_targeted_mutation(
        &self,
        mutation: ControlPlaneMutation,
        mut audit: AdminAuditEvent,
    ) -> StoreResult<Revision> {
        let mut transaction = self.pool.begin().await.map_err(|source| {
            postgres_unavailable("begin targeted control plane mutation", source)
        })?;
        let result: StoreResult<_> = async {
            let revision = bump_config_revision_in_transaction(&mut transaction).await?;
            match mutation {
                ControlPlaneMutation::CreateClientApiKey(key, sharing) => {
                    audit.changed_fields.push(if key.group_ids.is_empty() {
                        "routing_scope:all".to_owned()
                    } else {
                        "routing_scope:groups".to_owned()
                    });
                    insert_client_api_key_in_transaction(&mut transaction, &key).await?;
                    set_quota_sharing(&mut transaction, &key.id, sharing, &mut audit).await?;
                }
                ControlPlaneMutation::UpdateClientApiKey(key, sharing) => {
                    let previously_restricted = sqlx::query_scalar::<_, bool>(
                        "select exists(
                           select 1 from client_api_key_groups where client_api_key_id = $1
                         )",
                    )
                    .bind(&key.id)
                    .fetch_one(&mut *transaction)
                    .await
                    .map_err(|source| {
                        postgres_unavailable("load client API key routing scope for audit", source)
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
                    set_quota_sharing(&mut transaction, &key.id, sharing, &mut audit).await?;
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
                transaction.commit().await.map_err(|source| {
                    postgres_unavailable("commit targeted control plane mutation", source)
                })?;
                Ok(revision)
            }
            Err(error) => {
                let error = match transaction.rollback().await {
                    Ok(()) => error,
                    Err(cleanup) => error.with_cleanup(cleanup),
                };
                Err(error)
            }
        }
    }
}

async fn set_quota_sharing(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: &str,
    sharing: gateway_admin::model::upstream_quota::QuotaSharingUpdate,
    audit: &mut AdminAuditEvent,
) -> StoreResult<()> {
    if !sharing.is_empty() {
        sqlx::query(
            "update client_api_keys set
            quota_sharing_enabled=coalesce($2, quota_sharing_enabled),
            quota_sharing_include_names=coalesce($3, quota_sharing_include_names),
            quota_sharing_include_models=coalesce($4, quota_sharing_include_models)
            where id=$1",
        )
        .bind(id)
        .bind(sharing.enabled)
        .bind(sharing.include_names)
        .bind(sharing.include_models)
        .execute(&mut **transaction)
        .await
        .map_err(|source| postgres_unavailable("update client key quota sharing", source))?;
        for (value, field) in [
            (sharing.enabled, "quota_sharing_enabled"),
            (sharing.include_names, "quota_sharing_include_names"),
            (sharing.include_models, "quota_sharing_include_models"),
        ] {
            if value.is_some() {
                audit.changed_fields.push(field.to_owned());
            }
        }
    }
    Ok(())
}

async fn load_control_plane_in_transaction(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> StoreResult<ControlPlaneSnapshot> {
    let settings = load_runtime_settings_in_transaction(transaction).await?;
    Ok(ControlPlaneSnapshot { settings })
}
