use std::collections::{BTreeMap, BTreeSet};

use gateway_admin::{
    model::{
        MutationContext, Revision,
        plugins::{
            instances::{
                PluginInstance, PluginInstanceMutation, PluginInstanceReplacement,
                PluginInstanceSnapshot, PluginPermissionGrant, PluginVersionConfiguration,
            },
            state::PluginStateCommit,
        },
    },
    ports::store::{AdminStoreError, AdminStoreErrorKind, AdminStoreResult},
};
use secrecy::ExposeSecret as _;
use sqlx::{PgPool, Postgres, Row as _, Transaction};

use super::super::{append_admin_audit_event_in_transaction, bump_config_revision_in_transaction};
use super::artifacts::{conflict, not_found, unavailable};
use crate::{admin_revision, admin_store_error, mutation_audit};

pub(super) async fn management_target_is_current(
    pool: &PgPool,
    target: &gateway_admin::model::plugins::management::PluginManagementTarget,
) -> AdminStoreResult<bool> {
    let (Ok(id), Ok(revision)) = (
        uuid::Uuid::parse_str(&target.instance_id),
        i64::try_from(target.revision),
    ) else {
        return Ok(false);
    };
    sqlx::query_scalar(
        "select exists (select 1 from plugin_instances i \
         join plugin_artifacts a on a.sha256=i.artifact_sha256 \
         where i.id=$1 and i.artifact_sha256=$2 and i.revision=$3 \
         and i.enabled and a.accepted_at is not null)",
    )
    .bind(id)
    .bind(&target.artifact_sha256)
    .bind(revision)
    .fetch_one(pool)
    .await
    .map_err(|_| unavailable())
}

pub(super) async fn load(pool: &PgPool) -> AdminStoreResult<PluginInstanceSnapshot> {
    let mut tx = pool.begin().await.map_err(|_| unavailable())?;
    sqlx::query("set transaction isolation level repeatable read read only")
        .execute(&mut *tx)
        .await
        .map_err(|_| unavailable())?;
    let revision: i64 =
        sqlx::query_scalar("select config_revision from runtime_settings where id=1")
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| unavailable())?;
    let rows = sqlx::query("select i.*, a.metadata_json, a.accepted_at, coalesce(s.secrets_json, '{}'::jsonb) as secrets_json from plugin_instances i join plugin_artifacts a on a.sha256=i.artifact_sha256 left join plugin_instance_secrets s on s.instance_id=i.id order by i.id")
        .fetch_all(&mut *tx).await.map_err(|_| unavailable())?;
    let instances = rows.iter().map(decode).collect::<AdminStoreResult<_>>()?;
    tx.commit().await.map_err(|_| unavailable())?;
    Ok(PluginInstanceSnapshot {
        config_revision: revision_from_i64(revision)?,
        instances,
    })
}

pub(super) async fn load_version_configuration(
    pool: &PgPool,
    id: &str,
    digest: &str,
) -> AdminStoreResult<Option<PluginVersionConfiguration>> {
    let id = uuid::Uuid::parse_str(id).map_err(|_| not_found())?;
    let row = sqlx::query("select configuration_json,secrets_json,bindings_json from plugin_version_configurations where instance_id=$1 and artifact_sha256=$2")
        .bind(id).bind(digest).fetch_optional(pool).await.map_err(|_| unavailable())?;
    row.map(|row| {
        Ok(PluginVersionConfiguration {
            configuration: row
                .try_get("configuration_json")
                .map_err(|_| unavailable())?,
            secrets: row
                .try_get::<sqlx::types::Json<_>, _>("secrets_json")
                .map_err(|_| unavailable())?
                .0,
            bindings: row
                .try_get::<sqlx::types::Json<_>, _>("bindings_json")
                .map_err(|_| unavailable())?
                .0,
        })
    })
    .transpose()
}

pub(super) async fn configuration_versions(
    pool: &PgPool,
    id: &str,
) -> AdminStoreResult<Vec<String>> {
    let id = uuid::Uuid::parse_str(id).map_err(|_| not_found())?;
    sqlx::query_scalar("select artifact_sha256 from plugin_version_configurations where instance_id=$1 order by artifact_sha256")
        .bind(id).fetch_all(pool).await.map_err(|_| unavailable())
}

fn decode(row: &sqlx::postgres::PgRow) -> AdminStoreResult<PluginInstance> {
    let id: uuid::Uuid = row.try_get("id").map_err(|_| unavailable())?;
    let accepted = row
        .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("accepted_at")
        .map_err(|_| unavailable())?
        .is_some();
    let metadata = super::artifacts::decode_metadata(row)?;
    Ok(PluginInstance {
        id: id.to_string(),
        name: row.try_get("name").map_err(|_| unavailable())?,
        artifact_sha256: row.try_get("artifact_sha256").map_err(|_| unavailable())?,
        enabled: row.try_get("enabled").map_err(|_| unavailable())?,
        trusted_process: accepted,
        configuration: row
            .try_get("configuration_json")
            .map_err(|_| unavailable())?,
        secrets: row
            .try_get::<sqlx::types::Json<_>, _>("secrets_json")
            .map_err(|_| unavailable())?
            .0,
        grants: if accepted {
            metadata
                .requested_permissions
                .into_iter()
                .map(|permission| PluginPermissionGrant { permission })
                .collect()
        } else {
            Vec::new()
        },
        bindings: row
            .try_get::<sqlx::types::Json<_>, _>("bindings_json")
            .map_err(|_| unavailable())?
            .0,
        revision: revision_from_i64(row.try_get("revision").map_err(|_| unavailable())?)?,
    })
}

async fn check_revision(
    tx: &mut Transaction<'_, Postgres>,
    expected: Revision,
) -> AdminStoreResult<()> {
    let revision: i64 =
        sqlx::query_scalar("select config_revision from runtime_settings where id=1 for update")
            .fetch_one(&mut **tx)
            .await
            .map_err(|_| unavailable())?;
    if revision_from_i64(revision)? != expected {
        return Err(conflict());
    }
    Ok(())
}

async fn validate_artifact_acceptance(
    tx: &mut Transaction<'_, Postgres>,
    instance: &PluginInstance,
) -> AdminStoreResult<()> {
    let row = sqlx::query(
        "select metadata_json, accepted_at from plugin_artifacts where sha256=$1 for key share",
    )
    .bind(&instance.artifact_sha256)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| unavailable())?
    .ok_or_else(not_found)?;
    let metadata = super::artifacts::decode_metadata(&row)?;
    let accepted = row
        .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("accepted_at")
        .map_err(|_| unavailable())?
        .is_some();
    let expected_grants = metadata
        .requested_permissions
        .into_iter()
        .map(|permission| PluginPermissionGrant { permission })
        .collect::<Vec<_>>();
    if instance.trusted_process != accepted
        || instance.grants != expected_grants
        || (instance.enabled && !accepted)
    {
        return Err(AdminStoreError::new(
            AdminStoreErrorKind::Invalid,
            "plugin",
            "plugin instance acceptance facts do not match its artifact",
        ));
    }
    Ok(())
}

async fn validate_binding_references(
    tx: &mut Transaction<'_, Postgres>,
    instance: &PluginInstance,
) -> AdminStoreResult<()> {
    let client_key_ids: Vec<String> = instance
        .bindings
        .iter()
        .flat_map(|binding| {
            binding.client_key_ids.iter().chain(
                binding
                    .identity_bindings
                    .iter()
                    .map(|identity| &identity.client_key_id),
            )
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .cloned()
        .collect();
    let account_group_ids: Vec<String> = instance
        .bindings
        .iter()
        .flat_map(|binding| &binding.account_group_ids)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .cloned()
        .collect();
    let valid: bool = sqlx::query_scalar(
        "select not exists (
           select 1 from unnest($1::text[]) as scope(id)
           left join client_api_keys existing_key on existing_key.id = scope.id
           where existing_key.id is null
         ) and not exists (
           select 1 from unnest($2::text[]) as scope(id)
           left join account_groups existing_group on existing_group.id = scope.id
           where existing_group.id is null
         )",
    )
    .bind(client_key_ids)
    .bind(account_group_ids)
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| unavailable())?;
    if !valid {
        return Err(AdminStoreError::new(
            AdminStoreErrorKind::Invalid,
            "plugin",
            "plugin binding references do not exist",
        ));
    }
    Ok(())
}

pub(super) async fn save(
    pool: &PgPool,
    instance: PluginInstance,
    expected: Revision,
    context: &MutationContext,
) -> AdminStoreResult<PluginInstanceMutation> {
    save_inner(pool, instance, expected, None, &[], context).await
}

pub(super) async fn save_with_state(
    pool: &PgPool,
    instance: PluginInstance,
    expected: Revision,
    state: &PluginStateCommit,
    replacements: &[PluginInstanceReplacement],
    context: &MutationContext,
) -> AdminStoreResult<PluginInstanceMutation> {
    save_inner(pool, instance, expected, Some(state), replacements, context).await
}

async fn save_inner(
    pool: &PgPool,
    mut instance: PluginInstance,
    expected: Revision,
    state: Option<&PluginStateCommit>,
    replacements: &[PluginInstanceReplacement],
    context: &MutationContext,
) -> AdminStoreResult<PluginInstanceMutation> {
    let id = uuid::Uuid::parse_str(&instance.id).map_err(|_| conflict())?;
    let mut tx = pool.begin().await.map_err(|_| unavailable())?;
    check_revision(&mut tx, expected).await?;
    validate_artifact_acceptance(&mut tx, &instance).await?;
    // 停用是损坏配置的恢复路径，必须原样保留绑定；再次启用时才要求引用仍存在。
    if instance.enabled {
        validate_binding_references(&mut tx, &instance).await?;
    }
    let revision = bump_config_revision_in_transaction(&mut tx)
        .await
        .map_err(|e| admin_store_error("plugin", e))?;
    sqlx::query("insert into plugin_instances (id,artifact_sha256,name,enabled,configuration_json,bindings_json,revision) values ($1,$2,$3,$4,$5,$6,$7) on conflict (id) do update set artifact_sha256=excluded.artifact_sha256,name=excluded.name,enabled=excluded.enabled,configuration_json=excluded.configuration_json,bindings_json=excluded.bindings_json,revision=excluded.revision")
        .bind(id).bind(&instance.artifact_sha256).bind(&instance.name).bind(instance.enabled).bind(&instance.configuration)
        .bind(sqlx::types::Json(&instance.bindings)).bind(i64::try_from(revision.get()).map_err(|_| unavailable())?)
        .execute(&mut *tx).await.map_err(|error| if error.as_database_error().is_some_and(|e| e.is_foreign_key_violation()) { conflict() } else {unavailable()})?;
    let secrets: BTreeMap<_, _> = instance
        .secrets
        .iter()
        .map(|(key, value)| (key.as_str(), value.expose_secret()))
        .collect();
    sqlx::query("insert into plugin_instance_secrets(instance_id,secrets_json) values ($1,$2) on conflict (instance_id) do update set secrets_json=excluded.secrets_json")
        .bind(id).bind(sqlx::types::Json(secrets)).execute(&mut *tx).await.map_err(|_| unavailable())?;
    // 与当前配置及状态一同提交，准备失败或事务冲突不能污染恢复点。
    // 停用草稿可能缺少必填参数，不能覆盖此版本最近的启用配置。
    if instance.enabled {
        sqlx::query("insert into plugin_version_configurations (instance_id,artifact_sha256,configuration_json,secrets_json,bindings_json) select i.id,i.artifact_sha256,i.configuration_json,s.secrets_json,i.bindings_json from plugin_instances i join plugin_instance_secrets s on s.instance_id=i.id where i.id=$1 on conflict (instance_id,artifact_sha256) do update set configuration_json=excluded.configuration_json,secrets_json=excluded.secrets_json,bindings_json=excluded.bindings_json")
            .bind(id).execute(&mut *tx).await.map_err(|_| unavailable())?;
    }
    let committed_revision = admin_revision(revision)?;
    // 旧配置与新配置共享事务，任一版本检查或状态提交失败都不留下半次切换。
    for replacement in replacements {
        let previous_id = uuid::Uuid::parse_str(&replacement.id).map_err(|_| conflict())?;
        if previous_id == id || !instance.enabled {
            return Err(conflict());
        }
        let artifact_sha256: String = sqlx::query_scalar(
            "update plugin_instances set enabled=false,revision=$3 \
             where id=$1 and revision=$2 and enabled=true \
             and artifact_sha256 in (select sha256 from plugin_artifacts where \
                 metadata_json->>'pluginId'=(select metadata_json->>'pluginId' from plugin_artifacts where sha256=$4)) \
             returning artifact_sha256",
        )
        .bind(previous_id)
        .bind(i64::try_from(replacement.expected_revision).map_err(|_| conflict())?)
        .bind(i64::try_from(revision.get()).map_err(|_| unavailable())?)
        .bind(&instance.artifact_sha256)
        .fetch_optional(&mut *tx).await.map_err(|_| unavailable())?
        .ok_or_else(conflict)?;
        super::state::rebind_existing_configuration(
            &mut tx,
            &replacement.id,
            &artifact_sha256,
            committed_revision,
        )
        .await?;
        append_admin_audit_event_in_transaction(
            &mut tx,
            mutation_audit(
                context,
                "configure",
                "plugin_instance",
                &replacement.id,
                vec!["enabled".into()],
            ),
            revision,
        )
        .await
        .map_err(|e| admin_store_error("plugin", e))?;
    }
    if let Some(state) = state {
        super::state::commit_configuration(&mut tx, &instance, committed_revision, state).await?;
    } else {
        super::state::rebind_existing_configuration(
            &mut tx,
            &instance.id,
            &instance.artifact_sha256,
            committed_revision,
        )
        .await?;
    }
    append_admin_audit_event_in_transaction(
        &mut tx,
        mutation_audit(
            context,
            "configure",
            "plugin_instance",
            &instance.id,
            vec![
                "artifact".into(),
                "configuration".into(),
                "bindings".into(),
                "enabled".into(),
            ],
        ),
        revision,
    )
    .await
    .map_err(|e| admin_store_error("plugin", e))?;
    tx.commit().await.map_err(|_| unavailable())?;
    instance.revision = committed_revision;
    Ok(PluginInstanceMutation {
        config_revision: instance.revision,
        instance,
    })
}

pub(super) async fn delete(
    pool: &PgPool,
    id: &str,
    expected: Revision,
    context: &MutationContext,
) -> AdminStoreResult<Revision> {
    let key = uuid::Uuid::parse_str(id).map_err(|_| not_found())?;
    let mut tx = pool.begin().await.map_err(|_| unavailable())?;
    check_revision(&mut tx, expected).await?;
    let revision = bump_config_revision_in_transaction(&mut tx)
        .await
        .map_err(|e| admin_store_error("plugin", e))?;
    let enabled: bool = sqlx::query_scalar("select enabled from plugin_instances where id=$1")
        .bind(key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| unavailable())?
        .ok_or_else(not_found)?;
    if enabled {
        return Err(conflict());
    }
    sqlx::query("delete from plugin_instances where id=$1")
        .bind(key)
        .execute(&mut *tx)
        .await
        .map_err(|_| conflict())?;
    append_admin_audit_event_in_transaction(
        &mut tx,
        mutation_audit(
            context,
            "delete",
            "plugin_instance",
            id,
            vec!["instance".into()],
        ),
        revision,
    )
    .await
    .map_err(|e| admin_store_error("plugin", e))?;
    tx.commit().await.map_err(|_| unavailable())?;
    admin_revision(revision)
}

fn revision_from_i64(value: i64) -> AdminStoreResult<Revision> {
    Revision::new(u64::try_from(value).map_err(|_| unavailable())?).map_err(|_| unavailable())
}
