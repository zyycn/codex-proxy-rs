use async_trait::async_trait;
use gateway_admin::model::plugins::distribution::{SourceCredential, SourceCredentialInfo};
use gateway_admin::{
    model::{
        MutationContext, Revision,
        plugins::{
            InspectedPluginArtifact, InstalledPluginArtifact, PluginArtifactMetadata,
            PluginArtifactMutation, PluginSource,
        },
    },
    ports::{
        plugins::PluginStore,
        store::{AdminStoreError, AdminStoreErrorKind, AdminStoreResult},
    },
};
use sqlx::{PgPool, Row as _};

use super::super::{append_admin_audit_event_in_transaction, bump_config_revision_in_transaction};
use crate::{admin_revision, admin_store_error, mutation_audit};

pub struct PgPluginStore {
    pub(super) pool: PgPool,
}

impl PgPluginStore {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

pub(super) fn unavailable() -> AdminStoreError {
    AdminStoreError::new(
        AdminStoreErrorKind::Unavailable,
        "plugin",
        "plugin artifact store is unavailable",
    )
}
pub(super) fn conflict() -> AdminStoreError {
    AdminStoreError::new(
        AdminStoreErrorKind::Conflict,
        "plugin",
        "plugin artifact already exists with different content or source",
    )
}
pub(super) fn not_found() -> AdminStoreError {
    AdminStoreError::new(
        AdminStoreErrorKind::NotFound,
        "plugin",
        "plugin artifact does not exist",
    )
}

pub(super) fn decode_metadata(
    row: &sqlx::postgres::PgRow,
) -> AdminStoreResult<PluginArtifactMetadata> {
    let mut metadata: serde_json::Value =
        row.try_get("metadata_json").map_err(|_| unavailable())?;
    // 兼容开发期已写入的展示说明；忽略这一旧字段，其他未知字段仍严格拒绝。
    if let Some(metadata) = metadata.as_object_mut() {
        metadata.remove("permissionDescriptions");
    }
    serde_json::from_value(metadata).map_err(|_| unavailable())
}

fn decode(row: &sqlx::postgres::PgRow) -> AdminStoreResult<InstalledPluginArtifact> {
    let source: sqlx::types::Json<PluginSource> =
        row.try_get("source_json").map_err(|_| unavailable())?;
    Ok(InstalledPluginArtifact {
        metadata: decode_metadata(row)?,
        source: source.0,
        installed_at: row.try_get("installed_at").map_err(|_| unavailable())?,
        accepted_at: row.try_get("accepted_at").map_err(|_| unavailable())?,
    })
}

#[async_trait]
impl PluginStore for PgPluginStore {
    async fn management_target_is_current(
        &self,
        target: &gateway_admin::model::plugins::management::PluginManagementTarget,
    ) -> AdminStoreResult<bool> {
        super::instances::management_target_is_current(&self.pool, target).await
    }
    async fn load_instances(
        &self,
    ) -> AdminStoreResult<gateway_admin::model::plugins::instances::PluginInstanceSnapshot> {
        super::instances::load(&self.pool).await
    }
    async fn load_version_configuration(
        &self,
        id: &str,
        digest: &str,
    ) -> AdminStoreResult<
        Option<gateway_admin::model::plugins::instances::PluginVersionConfiguration>,
    > {
        super::instances::load_version_configuration(&self.pool, id, digest).await
    }
    async fn configuration_versions(&self, id: &str) -> AdminStoreResult<Vec<String>> {
        super::instances::configuration_versions(&self.pool, id).await
    }
    async fn save_instance(
        &self,
        instance: gateway_admin::model::plugins::instances::PluginInstance,
        expected: Revision,
        context: &MutationContext,
    ) -> AdminStoreResult<gateway_admin::model::plugins::instances::PluginInstanceMutation> {
        super::instances::save(&self.pool, instance, expected, context).await
    }
    async fn save_instance_with_state(
        &self,
        instance: gateway_admin::model::plugins::instances::PluginInstance,
        expected: Revision,
        state: gateway_admin::model::plugins::state::PluginStateCommit,
        context: &MutationContext,
    ) -> AdminStoreResult<gateway_admin::model::plugins::instances::PluginInstanceMutation> {
        super::instances::save_with_state(&self.pool, instance, expected, &state, &[], context)
            .await
    }
    async fn save_instance_replacing(
        &self,
        instance: gateway_admin::model::plugins::instances::PluginInstance,
        expected: Revision,
        state: gateway_admin::model::plugins::state::PluginStateCommit,
        replacements: &[gateway_admin::model::plugins::instances::PluginInstanceReplacement],
        context: &MutationContext,
    ) -> AdminStoreResult<gateway_admin::model::plugins::instances::PluginInstanceMutation> {
        super::instances::save_with_state(
            &self.pool,
            instance,
            expected,
            &state,
            replacements,
            context,
        )
        .await
    }
    async fn delete_instance(
        &self,
        id: &str,
        expected: Revision,
        context: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        super::instances::delete(&self.pool, id, expected, context).await
    }
    async fn list_update_sources(
        &self,
    ) -> AdminStoreResult<Vec<gateway_admin::model::plugins::distribution::PluginSourceBinding>>
    {
        super::sources::list(&self.pool).await
    }
    async fn change_update_source(
        &self,
        binding: gateway_admin::model::plugins::distribution::PluginSourceBinding,
        context: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        super::sources::change(&self.pool, binding, context).await
    }
    async fn list_source_credentials(&self) -> AdminStoreResult<Vec<SourceCredentialInfo>> {
        super::credentials::list(&self.pool).await
    }
    async fn load_source_credential(&self, id: &str) -> AdminStoreResult<SourceCredential> {
        super::credentials::load(&self.pool, id).await
    }
    async fn save_source_credential(
        &self,
        credential: SourceCredential,
        context: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        super::credentials::save(&self.pool, credential, context).await
    }
    async fn delete_source_credential(
        &self,
        id: &str,
        context: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        super::credentials::delete(&self.pool, id, context).await
    }
    async fn list_artifacts(&self) -> AdminStoreResult<Vec<InstalledPluginArtifact>> {
        let rows = sqlx::query("select metadata_json, source_json, installed_at, accepted_at from plugin_artifacts order by plugin_id, installed_at, sha256")
            .fetch_all(&self.pool).await.map_err(|_| unavailable())?;
        rows.iter().map(decode).collect()
    }

    async fn load_artifact(&self, digest: &str) -> AdminStoreResult<InspectedPluginArtifact> {
        let row =
            sqlx::query("select metadata_json, archive from plugin_artifacts where sha256 = $1")
                .bind(digest)
                .fetch_optional(&self.pool)
                .await
                .map_err(|_| unavailable())?
                .ok_or_else(not_found)?;
        let archive: Vec<u8> = row.try_get("archive").map_err(|_| unavailable())?;
        Ok(InspectedPluginArtifact {
            metadata: decode_metadata(&row)?,
            archive: archive.into(),
        })
    }

    async fn install_artifact(
        &self,
        artifact: InspectedPluginArtifact,
        source: PluginSource,
        context: &MutationContext,
    ) -> AdminStoreResult<PluginArtifactMutation> {
        let mut transaction = self.pool.begin().await.map_err(|_| unavailable())?;
        let current_revision: i64 = sqlx::query_scalar(
            "select config_revision from runtime_settings where id=1 for update",
        )
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| unavailable())?;
        let metadata = artifact.metadata;
        let outbound_proxy = source.outbound_proxy().cloned();
        super::sources::bind(
            &mut transaction,
            &metadata.plugin_id,
            (&source).into(),
            outbound_proxy.as_ref(),
        )
        .await?;
        if let Some(row) = sqlx::query(
            "select metadata_json, source_json, installed_at, accepted_at from plugin_artifacts where sha256=$1",
        )
        .bind(&metadata.sha256)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| unavailable())?
        {
            let artifact = decode(&row)?;
            // 当前允许来源已由 bind 校验。同一包换用已授权的下载入口时保留首次出处，
            // 但不能借重复安装把自定义包提升为内置包，或反向改写内置包的信任身份。
            let same_trust_origin = matches!(&artifact.source, PluginSource::Builtin { .. })
                == matches!(&source, PluginSource::Builtin { .. });
            if artifact.metadata != metadata || !same_trust_origin {
                return Err(conflict());
            }
            transaction.commit().await.map_err(|_| unavailable())?;
            return Ok(PluginArtifactMutation {
                config_revision: Revision::new(
                    u64::try_from(current_revision).map_err(|_| unavailable())?,
                )
                .map_err(|_| unavailable())?,
                artifact,
            });
        }
        let revision = bump_config_revision_in_transaction(&mut transaction)
            .await
            .map_err(|error| admin_store_error("plugin", error))?;
        let mut platforms = metadata.platforms.clone();
        platforms.sort();
        let row = sqlx::query("insert into plugin_artifacts (sha256, plugin_id, version, metadata_json, source_json, outbound_proxy_id, archive) values ($1,$2,$3,$4,$5,$6,$7) returning metadata_json, source_json, installed_at, accepted_at")
            .bind(&metadata.sha256).bind(&metadata.plugin_id).bind(&metadata.version)
            .bind(sqlx::types::Json(&metadata)).bind(sqlx::types::Json(&source))
            .bind(outbound_proxy.map(|proxy| proxy.id)).bind(artifact.archive.as_ref())
            .fetch_one(&mut *transaction).await.map_err(|error| {
                if error.as_database_error().is_some_and(|error| error.is_unique_violation()) { conflict() } else { unavailable() }
            })?;
        for id in source.credential_ids() {
            let id = uuid::Uuid::parse_str(id).map_err(|_| conflict())?;
            sqlx::query("insert into plugin_artifact_credentials(artifact_sha256,credential_id) values ($1,$2)")
                .bind(&metadata.sha256).bind(id).execute(&mut *transaction).await.map_err(|_| conflict())?;
        }
        for platform in platforms {
            sqlx::query("insert into plugin_artifact_platforms (plugin_id, version, platform, artifact_sha256) values ($1,$2,$3,$4)")
                .bind(&metadata.plugin_id).bind(&metadata.version).bind(platform).bind(&metadata.sha256)
                .execute(&mut *transaction).await.map_err(|error| {
                    if error.as_database_error().is_some_and(|error| error.is_unique_violation()) { conflict() } else { unavailable() }
                })?;
        }
        append_admin_audit_event_in_transaction(
            &mut transaction,
            mutation_audit(
                context,
                "install",
                "plugin_artifact",
                &metadata.plugin_id,
                vec!["artifact".into(), "source".into()],
            ),
            revision,
        )
        .await
        .map_err(|error| admin_store_error("plugin", error))?;
        let artifact = decode(&row)?;
        transaction.commit().await.map_err(|_| unavailable())?;
        Ok(PluginArtifactMutation {
            config_revision: admin_revision(revision)?,
            artifact,
        })
    }

    async fn accept_artifact(
        &self,
        digest: &str,
        context: &MutationContext,
    ) -> AdminStoreResult<PluginArtifactMutation> {
        let mut transaction = self.pool.begin().await.map_err(|_| unavailable())?;
        let current: i64 = sqlx::query_scalar(
            "select config_revision from runtime_settings where id=1 for update",
        )
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| unavailable())?;
        let row = sqlx::query("select metadata_json, source_json, installed_at, accepted_at from plugin_artifacts where sha256=$1 for update")
            .bind(digest).fetch_optional(&mut *transaction).await.map_err(|_| unavailable())?
            .ok_or_else(not_found)?;
        let mut artifact = decode(&row)?;
        let config_revision = if artifact.accepted_at.is_some() {
            Revision::new(u64::try_from(current).map_err(|_| unavailable())?)
                .map_err(|_| unavailable())?
        } else {
            let accepted_at: chrono::DateTime<chrono::Utc> = sqlx::query_scalar("update plugin_artifacts set accepted_at=now() where sha256=$1 returning accepted_at")
                .bind(digest).fetch_one(&mut *transaction).await.map_err(|_| unavailable())?;
            artifact.accepted_at = Some(accepted_at);
            let revision = bump_config_revision_in_transaction(&mut transaction)
                .await
                .map_err(|error| admin_store_error("plugin", error))?;
            append_admin_audit_event_in_transaction(
                &mut transaction,
                mutation_audit(
                    context,
                    "accept",
                    "plugin_artifact",
                    digest,
                    vec!["permissions".into()],
                ),
                revision,
            )
            .await
            .map_err(|error| admin_store_error("plugin", error))?;
            admin_revision(revision)?
        };
        transaction.commit().await.map_err(|_| unavailable())?;
        Ok(PluginArtifactMutation {
            config_revision,
            artifact,
        })
    }

    async fn delete_artifact(
        &self,
        digest: &str,
        context: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        let mut transaction = self.pool.begin().await.map_err(|_| unavailable())?;
        let revision = bump_config_revision_in_transaction(&mut transaction)
            .await
            .map_err(|error| admin_store_error("plugin", error))?;
        // 制品删除会级联移除下载引用，先固定本次涉及的凭据，不能清扫无关的未使用凭据。
        let credential_ids: Vec<uuid::Uuid> = sqlx::query_scalar(
            "select credential_id from plugin_artifact_credentials where artifact_sha256=$1",
        )
        .bind(digest)
        .fetch_all(&mut *transaction)
        .await
        .map_err(|_| unavailable())?;
        let id: String = sqlx::query_scalar(
            "delete from plugin_artifacts where sha256 = $1 returning plugin_id",
        )
        .bind(digest)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| {
            if error.as_database_error().is_some_and(|error| {
                error.is_foreign_key_violation() || error.code().as_deref() == Some("23001")
            }) {
                conflict()
            } else {
                unavailable()
            }
        })?
        .ok_or_else(not_found)?;
        super::sources::delete_if_unused(&mut transaction, &id, context, revision).await?;
        super::credentials::delete_unused(&mut transaction, &credential_ids, context, revision)
            .await?;
        append_admin_audit_event_in_transaction(
            &mut transaction,
            mutation_audit(
                context,
                "delete",
                "plugin_artifact",
                &id,
                vec!["artifact".into()],
            ),
            revision,
        )
        .await
        .map_err(|error| admin_store_error("plugin", error))?;
        transaction.commit().await.map_err(|_| unavailable())?;
        admin_revision(revision)
    }
}
