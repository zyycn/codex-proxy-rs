//! 插件私有状态的 PostgreSQL 读写、代次隔离、配额与迁移事务

use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use gateway_admin::{
    model::{
        Revision,
        plugins::{
            instances::PluginInstance,
            state::{
                ApplyPluginStateMigration, DeletePluginState, PluginStateCommit,
                PluginStateConfiguration, PluginStateMigrationAction, PluginStateMigrationBatch,
                PluginStateMigrationNamespace, PluginStateNamespaceOwner, PluginStateOwner,
                PluginStateOwnerRequest, PluginStateRecord, PluginStateSchema,
                PluginStateTransition, PluginStateWrite, PutPluginState,
            },
        },
    },
    ports::{
        plugins::{
            PluginStateStore, PluginStateStoreError, PluginStateStoreErrorKind,
            PluginStateStoreResult,
        },
        store::{AdminStoreError, AdminStoreErrorKind, AdminStoreResult},
    },
};
use sqlx::{Postgres, Row as _, Transaction};

use super::artifacts::PgPluginStore;

const MIGRATION_BATCH_MAXIMUM: u32 = 100;

struct ActiveGeneration {
    id: uuid::Uuid,
    namespace: String,
    schema_version: u32,
    schema_sha256: String,
    record_count: u64,
    total_bytes: u64,
    maximum_record_bytes: u64,
}

struct WritableGeneration {
    id: uuid::Uuid,
    maximum_records: u64,
    maximum_bytes: u64,
    maximum_value_bytes: u64,
    next_record_version: u64,
    record_count: u64,
    total_bytes: u64,
}

fn state_error(kind: PluginStateStoreErrorKind) -> PluginStateStoreError {
    PluginStateStoreError::new(kind)
}

fn invalid() -> PluginStateStoreError {
    state_error(PluginStateStoreErrorKind::Invalid)
}

fn not_found() -> PluginStateStoreError {
    state_error(PluginStateStoreErrorKind::NotFound)
}

fn conflict() -> PluginStateStoreError {
    state_error(PluginStateStoreErrorKind::Conflict)
}

fn quota() -> PluginStateStoreError {
    state_error(PluginStateStoreErrorKind::Quota)
}

fn denied() -> PluginStateStoreError {
    state_error(PluginStateStoreErrorKind::PermissionDenied)
}

fn unavailable() -> PluginStateStoreError {
    state_error(PluginStateStoreErrorKind::Unavailable)
}

fn admin_conflict() -> AdminStoreError {
    AdminStoreError::new(
        AdminStoreErrorKind::Conflict,
        "plugin state",
        "plugin state configuration conflicts with the active generation",
    )
}

fn admin_invalid() -> AdminStoreError {
    AdminStoreError::new(
        AdminStoreErrorKind::Invalid,
        "plugin state",
        "plugin state configuration is invalid",
    )
}

fn admin_unavailable() -> AdminStoreError {
    AdminStoreError::new(
        AdminStoreErrorKind::Unavailable,
        "plugin state",
        "plugin state store is unavailable",
    )
}

fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.chars().count() <= 256
        && key.len() <= 512
        && !key.chars().any(char::is_control)
}

fn validate_configuration(configuration: &PluginStateConfiguration) -> PluginStateStoreResult<()> {
    if configuration.namespaces.len() > 16 {
        return Err(invalid());
    }
    let mut namespaces = BTreeSet::new();
    for schema in &configuration.namespaces {
        let schema_bytes = serde_json::to_vec(&schema.schema).map_err(|_| invalid())?;
        let migrations = schema
            .migrates_from
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        if !valid_namespace(&schema.namespace)
            || !namespaces.insert(&schema.namespace)
            || schema.schema_version == 0
            || schema.schema_sha256.len() != 64
            || !schema
                .schema_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || !schema.schema.is_object()
            || schema_bytes.len() > 32 * 1024
            || schema.maximum_records == 0
            || schema.maximum_records > 10_000
            || schema.maximum_bytes == 0
            || schema.maximum_bytes > 16 * 1024 * 1024
            || schema.maximum_value_bytes == 0
            || schema.maximum_value_bytes > 256 * 1024
            || u64::from(schema.maximum_value_bytes) > schema.maximum_bytes
            || migrations.len() != schema.migrates_from.len()
            || migrations.contains(&0)
            || migrations.contains(&schema.schema_version)
        {
            return Err(invalid());
        }
    }
    Ok(())
}

fn valid_namespace(namespace: &str) -> bool {
    !namespace.is_empty()
        && namespace.len() <= 64
        && namespace.as_bytes()[0].is_ascii_lowercase()
        && namespace.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_.".contains(&byte)
        })
}

fn target_map(configuration: &PluginStateConfiguration) -> BTreeMap<&str, &PluginStateSchema> {
    configuration
        .namespaces
        .iter()
        .map(|schema| (schema.namespace.as_str(), schema))
        .collect()
}

fn revision_i64(revision: Revision) -> Result<i64, PluginStateStoreError> {
    i64::try_from(revision.get()).map_err(|_| invalid())
}

fn u64_from_i64(value: i64) -> PluginStateStoreResult<u64> {
    u64::try_from(value).map_err(|_| unavailable())
}

fn u32_from_i64(value: i64) -> PluginStateStoreResult<u32> {
    u32::try_from(value).map_err(|_| unavailable())
}

async fn active_generations(
    transaction: &mut Transaction<'_, Postgres>,
    instance_id: uuid::Uuid,
    lock: bool,
) -> PluginStateStoreResult<Vec<ActiveGeneration>> {
    let query = if lock {
        "select g.id, g.namespace, g.schema_version, g.schema_sha256, g.record_count, \
                g.total_bytes, coalesce((select max(r.value_bytes) \
                  from plugin_state_records r where r.generation_id=g.id), 0) \
                  as maximum_record_bytes \
         from plugin_state_generations g where g.instance_id=$1 and g.status='active' \
         order by g.namespace for update of g"
    } else {
        "select g.id, g.namespace, g.schema_version, g.schema_sha256, g.record_count, \
                g.total_bytes, coalesce((select max(r.value_bytes) \
                  from plugin_state_records r where r.generation_id=g.id), 0) \
                  as maximum_record_bytes \
         from plugin_state_generations g where g.instance_id=$1 and g.status='active' \
         order by g.namespace"
    };
    sqlx::query(query)
        .bind(instance_id)
        .fetch_all(&mut **transaction)
        .await
        .map_err(|_| unavailable())?
        .into_iter()
        .map(|row| {
            Ok(ActiveGeneration {
                id: row.try_get("id").map_err(|_| unavailable())?,
                namespace: row.try_get("namespace").map_err(|_| unavailable())?,
                schema_version: u32_from_i64(
                    row.try_get("schema_version").map_err(|_| unavailable())?,
                )?,
                schema_sha256: row.try_get("schema_sha256").map_err(|_| unavailable())?,
                record_count: u64_from_i64(
                    row.try_get("record_count").map_err(|_| unavailable())?,
                )?,
                total_bytes: u64_from_i64(row.try_get("total_bytes").map_err(|_| unavailable())?)?,
                maximum_record_bytes: u64_from_i64(
                    row.try_get("maximum_record_bytes")
                        .map_err(|_| unavailable())?,
                )?,
            })
        })
        .collect()
}

#[async_trait]
impl PluginStateStore for PgPluginStore {
    async fn load_owner(
        &self,
        request: PluginStateOwnerRequest,
    ) -> PluginStateStoreResult<Option<PluginStateOwner>> {
        validate_configuration(&request.configuration)?;
        let instance_id = uuid::Uuid::parse_str(&request.instance_id).map_err(|_| invalid())?;
        let revision = revision_i64(request.instance_revision)?;
        let current =
            sqlx::query("select artifact_sha256, revision from plugin_instances where id=$1")
                .bind(instance_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|_| unavailable())?;
        let Some(current) = current else {
            return Ok(None);
        };
        if current
            .try_get::<String, _>("artifact_sha256")
            .map_err(|_| unavailable())?
            != request.artifact_sha256
            || current
                .try_get::<i64, _>("revision")
                .map_err(|_| unavailable())?
                != revision
        {
            return Ok(None);
        }
        let rows = sqlx::query(
            "select id, namespace, artifact_sha256, schema_version, schema_sha256, \
                    instance_revision, fence, maximum_records, maximum_bytes, maximum_value_bytes \
             from plugin_state_generations \
             where instance_id=$1 and status='active' order by namespace",
        )
        .bind(instance_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| unavailable())?;
        let target = target_map(&request.configuration);
        if rows.len() != target.len() {
            return Ok(None);
        }
        let mut namespaces = BTreeMap::new();
        for row in rows {
            let namespace: String = row.try_get("namespace").map_err(|_| unavailable())?;
            let Some(schema) = target.get(namespace.as_str()) else {
                return Ok(None);
            };
            if row
                .try_get::<String, _>("artifact_sha256")
                .map_err(|_| unavailable())?
                != request.artifact_sha256
                || row
                    .try_get::<i64, _>("instance_revision")
                    .map_err(|_| unavailable())?
                    != revision
                || u32_from_i64(row.try_get("schema_version").map_err(|_| unavailable())?)?
                    != schema.schema_version
                || row
                    .try_get::<String, _>("schema_sha256")
                    .map_err(|_| unavailable())?
                    != schema.schema_sha256
                || u64_from_i64(row.try_get("maximum_records").map_err(|_| unavailable())?)?
                    != u64::from(schema.maximum_records)
                || u64_from_i64(row.try_get("maximum_bytes").map_err(|_| unavailable())?)?
                    != schema.maximum_bytes
                || u64_from_i64(
                    row.try_get("maximum_value_bytes")
                        .map_err(|_| unavailable())?,
                )? != u64::from(schema.maximum_value_bytes)
            {
                return Ok(None);
            }
            let generation: uuid::Uuid = row.try_get("id").map_err(|_| unavailable())?;
            let fence: uuid::Uuid = row.try_get("fence").map_err(|_| unavailable())?;
            namespaces.insert(
                namespace,
                PluginStateNamespaceOwner::from_store(
                    generation.to_string(),
                    fence.to_string(),
                    schema.schema_version,
                ),
            );
        }
        Ok(Some(PluginStateOwner::from_store(
            request.instance_id,
            request.artifact_sha256,
            request.instance_revision,
            namespaces,
        )))
    }

    async fn get(
        &self,
        owner: &PluginStateOwner,
        namespace: &str,
        key: &str,
    ) -> PluginStateStoreResult<Option<PluginStateRecord>> {
        if !valid_key(key) {
            return Err(invalid());
        }
        let namespace_owner = owner.namespace(namespace).ok_or_else(denied)?;
        let generation =
            uuid::Uuid::parse_str(namespace_owner.generation_id()).map_err(|_| denied())?;
        let fence = uuid::Uuid::parse_str(namespace_owner.fence()).map_err(|_| denied())?;
        let instance_id = uuid::Uuid::parse_str(owner.instance_id()).map_err(|_| denied())?;
        let row = sqlx::query(
            "select g.schema_version, r.state_key, r.value_json, r.record_version \
             from plugin_state_generations g \
             join plugin_instances i on i.id=g.instance_id \
             left join plugin_state_records r on r.generation_id=g.id and r.state_key=$7 \
             where g.id=$1 and g.fence=$2 and g.instance_id=$3 and g.namespace=$4 \
               and g.artifact_sha256=$5 and g.instance_revision=$6 and g.status='active' \
               and i.artifact_sha256=g.artifact_sha256 and i.revision=g.instance_revision",
        )
        .bind(generation)
        .bind(fence)
        .bind(instance_id)
        .bind(namespace)
        .bind(owner.artifact_sha256())
        .bind(revision_i64(owner.instance_revision())?)
        .bind(key)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| unavailable())?
        .ok_or_else(denied)?;
        let Some(state_key) = row
            .try_get::<Option<String>, _>("state_key")
            .map_err(|_| unavailable())?
        else {
            return Ok(None);
        };
        Ok(Some(PluginStateRecord {
            key: state_key,
            value: row.try_get("value_json").map_err(|_| unavailable())?,
            version: u64_from_i64(row.try_get("record_version").map_err(|_| unavailable())?)?,
            schema_version: u32_from_i64(
                row.try_get("schema_version").map_err(|_| unavailable())?,
            )?,
        }))
    }

    async fn put(
        &self,
        owner: &PluginStateOwner,
        command: PutPluginState,
    ) -> PluginStateStoreResult<PluginStateWrite> {
        if !valid_key(&command.key) {
            return Err(invalid());
        }
        let value_bytes = serde_json::to_vec(&command.value)
            .map_err(|_| invalid())?
            .len();
        let value_bytes = u64::try_from(value_bytes).map_err(|_| quota())?;
        let mut transaction = self.pool.begin().await.map_err(|_| unavailable())?;
        let generation = writable_generation(&mut transaction, owner, &command.namespace).await?;
        if value_bytes == 0 || value_bytes > generation.maximum_value_bytes {
            return Err(quota());
        }
        let existing = sqlx::query(
            "select record_version, value_bytes from plugin_state_records \
             where generation_id=$1 and state_key=$2",
        )
        .bind(generation.id)
        .bind(&command.key)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| unavailable())?;
        let previous_bytes = match (command.expected_version, existing) {
            (None, None) => 0,
            (None, Some(_)) | (Some(_), None) => return Err(conflict()),
            (Some(expected), Some(row)) => {
                if u64_from_i64(row.try_get("record_version").map_err(|_| unavailable())?)?
                    != expected
                {
                    return Err(conflict());
                }
                u64_from_i64(row.try_get("value_bytes").map_err(|_| unavailable())?)?
            }
        };
        let record_count = generation
            .record_count
            .checked_add(u64::from(previous_bytes == 0))
            .ok_or_else(quota)?;
        let total_bytes = generation
            .total_bytes
            .checked_sub(previous_bytes)
            .and_then(|bytes| bytes.checked_add(value_bytes))
            .ok_or_else(quota)?;
        if record_count > generation.maximum_records || total_bytes > generation.maximum_bytes {
            return Err(quota());
        }
        let version = generation.next_record_version;
        let next_version = version.checked_add(1).ok_or_else(quota)?;
        sqlx::query(
            "insert into plugin_state_records( \
                 generation_id,state_key,value_json,value_bytes,record_version \
             ) values ($1,$2,$3,$4,$5) \
             on conflict(generation_id,state_key) do update set \
                 value_json=excluded.value_json, value_bytes=excluded.value_bytes, \
                 record_version=excluded.record_version",
        )
        .bind(generation.id)
        .bind(&command.key)
        .bind(&command.value)
        .bind(i64::try_from(value_bytes).map_err(|_| quota())?)
        .bind(i64::try_from(version).map_err(|_| quota())?)
        .execute(&mut *transaction)
        .await
        .map_err(|_| unavailable())?;
        sqlx::query(
            "update plugin_state_generations set next_record_version=$2, record_count=$3, \
                 total_bytes=$4 where id=$1",
        )
        .bind(generation.id)
        .bind(i64::try_from(next_version).map_err(|_| quota())?)
        .bind(i64::try_from(record_count).map_err(|_| quota())?)
        .bind(i64::try_from(total_bytes).map_err(|_| quota())?)
        .execute(&mut *transaction)
        .await
        .map_err(|_| unavailable())?;
        transaction.commit().await.map_err(|_| unavailable())?;
        Ok(PluginStateWrite { version })
    }

    async fn delete(
        &self,
        owner: &PluginStateOwner,
        command: DeletePluginState,
    ) -> PluginStateStoreResult<bool> {
        if !valid_key(&command.key) || command.expected_version == 0 {
            return Err(invalid());
        }
        let mut transaction = self.pool.begin().await.map_err(|_| unavailable())?;
        let generation = writable_generation(&mut transaction, owner, &command.namespace).await?;
        let row = sqlx::query(
            "select record_version, value_bytes from plugin_state_records \
             where generation_id=$1 and state_key=$2",
        )
        .bind(generation.id)
        .bind(&command.key)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| unavailable())?;
        let Some(row) = row else {
            transaction.commit().await.map_err(|_| unavailable())?;
            return Ok(false);
        };
        if u64_from_i64(row.try_get("record_version").map_err(|_| unavailable())?)?
            != command.expected_version
        {
            return Err(conflict());
        }
        let value_bytes = u64_from_i64(row.try_get("value_bytes").map_err(|_| unavailable())?)?;
        sqlx::query("delete from plugin_state_records where generation_id=$1 and state_key=$2")
            .bind(generation.id)
            .bind(&command.key)
            .execute(&mut *transaction)
            .await
            .map_err(|_| unavailable())?;
        sqlx::query(
            "update plugin_state_generations set record_count=$2,total_bytes=$3 where id=$1",
        )
        .bind(generation.id)
        .bind(i64::try_from(generation.record_count.saturating_sub(1)).map_err(|_| unavailable())?)
        .bind(
            i64::try_from(generation.total_bytes.saturating_sub(value_bytes))
                .map_err(|_| unavailable())?,
        )
        .execute(&mut *transaction)
        .await
        .map_err(|_| unavailable())?;
        transaction.commit().await.map_err(|_| unavailable())?;
        Ok(true)
    }

    async fn transition_required(
        &self,
        instance_id: &str,
        target: &PluginStateConfiguration,
    ) -> PluginStateStoreResult<bool> {
        validate_configuration(target)?;
        let instance_id = uuid::Uuid::parse_str(instance_id).map_err(|_| invalid())?;
        let mut transaction = self.pool.begin().await.map_err(|_| unavailable())?;
        let active = active_generations(&mut transaction, instance_id, false).await?;
        transaction.commit().await.map_err(|_| unavailable())?;
        let target = target_map(target);
        let mut required = false;
        for generation in active {
            let Some(schema) = target.get(generation.namespace.as_str()) else {
                if generation.record_count != 0 {
                    return Err(conflict());
                }
                continue;
            };
            if schema.schema_version == generation.schema_version
                && schema.schema_sha256 == generation.schema_sha256
            {
                if generation.record_count > u64::from(schema.maximum_records)
                    || generation.total_bytes > schema.maximum_bytes
                    || generation.maximum_record_bytes > u64::from(schema.maximum_value_bytes)
                {
                    return Err(conflict());
                }
                continue;
            }
            if !schema.migrates_from.contains(&generation.schema_version) {
                return Err(conflict());
            }
            required = true;
        }
        Ok(required)
    }

    async fn begin_transition(
        &self,
        instance_id: &str,
        expected_instance_revision: Revision,
        artifact_sha256: &str,
        target: PluginStateConfiguration,
    ) -> PluginStateStoreResult<PluginStateTransition> {
        validate_configuration(&target)?;
        let instance_uuid = uuid::Uuid::parse_str(instance_id).map_err(|_| invalid())?;
        let mut transaction = self.pool.begin().await.map_err(|_| unavailable())?;
        let current =
            sqlx::query("select enabled, revision from plugin_instances where id=$1 for update")
                .bind(instance_uuid)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|_| unavailable())?
                .ok_or_else(not_found)?;
        if current
            .try_get::<bool, _>("enabled")
            .map_err(|_| unavailable())?
            || current
                .try_get::<i64, _>("revision")
                .map_err(|_| unavailable())?
                != revision_i64(expected_instance_revision)?
        {
            return Err(conflict());
        }
        let staging_exists: bool = sqlx::query_scalar(
            "select exists(select 1 from plugin_state_generations \
             where instance_id=$1 and status='staging')",
        )
        .bind(instance_uuid)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| unavailable())?;
        if staging_exists {
            return Err(conflict());
        }
        let active = active_generations(&mut transaction, instance_uuid, true).await?;
        let targets = target_map(&target);
        let transition_id = uuid::Uuid::new_v4();
        let mut namespaces = Vec::new();
        for generation in &active {
            let Some(schema) = targets.get(generation.namespace.as_str()) else {
                if generation.record_count != 0 {
                    return Err(conflict());
                }
                continue;
            };
            if schema.schema_version == generation.schema_version
                && schema.schema_sha256 == generation.schema_sha256
            {
                if generation.record_count > u64::from(schema.maximum_records)
                    || generation.total_bytes > schema.maximum_bytes
                    || generation.maximum_record_bytes > u64::from(schema.maximum_value_bytes)
                {
                    return Err(conflict());
                }
                continue;
            }
            if !schema.migrates_from.contains(&generation.schema_version) {
                return Err(conflict());
            }
            let generation_id = uuid::Uuid::new_v4();
            sqlx::query(
                "insert into plugin_state_generations( \
                   id,transition_id,instance_id,namespace,artifact_sha256,schema_version, \
                   schema_sha256,maximum_records,maximum_bytes,maximum_value_bytes, \
                   instance_revision,fence,status,source_generation_id \
                 ) values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,'staging',$13)",
            )
            .bind(generation_id)
            .bind(transition_id)
            .bind(instance_uuid)
            .bind(&schema.namespace)
            .bind(artifact_sha256)
            .bind(i64::from(schema.schema_version))
            .bind(&schema.schema_sha256)
            .bind(i64::from(schema.maximum_records))
            .bind(i64::try_from(schema.maximum_bytes).map_err(|_| invalid())?)
            .bind(i64::from(schema.maximum_value_bytes))
            .bind(revision_i64(expected_instance_revision)?)
            .bind(uuid::Uuid::new_v4())
            .bind(generation.id)
            .execute(&mut *transaction)
            .await
            .map_err(|_| unavailable())?;
            namespaces.push(PluginStateMigrationNamespace {
                namespace: schema.namespace.clone(),
                from_schema_version: generation.schema_version,
                to_schema_version: schema.schema_version,
            });
        }
        if namespaces.is_empty() {
            return Err(conflict());
        }
        transaction.commit().await.map_err(|_| unavailable())?;
        Ok(PluginStateTransition {
            id: transition_id.to_string(),
            instance_id: instance_id.to_owned(),
            artifact_sha256: artifact_sha256.to_owned(),
            namespaces,
        })
    }

    async fn migration_batch(
        &self,
        transition_id: &str,
        namespace: &str,
        maximum_records: u32,
    ) -> PluginStateStoreResult<PluginStateMigrationBatch> {
        if maximum_records == 0 || maximum_records > MIGRATION_BATCH_MAXIMUM {
            return Err(invalid());
        }
        let transition_id = uuid::Uuid::parse_str(transition_id).map_err(|_| invalid())?;
        let staging = sqlx::query(
            "select source_generation_id,migration_cursor,migration_complete \
             from plugin_state_generations \
             where transition_id=$1 and namespace=$2 and status='staging'",
        )
        .bind(transition_id)
        .bind(namespace)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| unavailable())?
        .ok_or_else(not_found)?;
        if staging
            .try_get::<bool, _>("migration_complete")
            .map_err(|_| unavailable())?
        {
            return Err(conflict());
        }
        let source: uuid::Uuid = staging
            .try_get("source_generation_id")
            .map_err(|_| unavailable())?;
        let cursor: Option<String> = staging
            .try_get("migration_cursor")
            .map_err(|_| unavailable())?;
        let rows = sqlx::query(
            "select r.state_key,r.value_json,r.record_version,g.schema_version \
             from plugin_state_records r \
             join plugin_state_generations g on g.id=r.generation_id \
             where r.generation_id=$1 and ($2::text is null or r.state_key>$2) \
             order by r.state_key limit $3",
        )
        .bind(source)
        .bind(cursor.as_deref())
        .bind(i64::from(maximum_records))
        .fetch_all(&self.pool)
        .await
        .map_err(|_| unavailable())?;
        let records = rows
            .into_iter()
            .map(|row| {
                Ok(PluginStateRecord {
                    key: row.try_get("state_key").map_err(|_| unavailable())?,
                    value: row.try_get("value_json").map_err(|_| unavailable())?,
                    version: u64_from_i64(
                        row.try_get("record_version").map_err(|_| unavailable())?,
                    )?,
                    schema_version: u32_from_i64(
                        row.try_get("schema_version").map_err(|_| unavailable())?,
                    )?,
                })
            })
            .collect::<PluginStateStoreResult<_>>()?;
        Ok(PluginStateMigrationBatch { cursor, records })
    }

    async fn apply_migration_batch(
        &self,
        command: ApplyPluginStateMigration,
    ) -> PluginStateStoreResult<()> {
        if command.expected_keys.len() > usize::try_from(MIGRATION_BATCH_MAXIMUM).unwrap()
            || command.changes.len() != command.expected_keys.len()
            || command.expected_keys.iter().any(|key| !valid_key(key))
            || command
                .expected_keys
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
            || command
                .changes
                .iter()
                .zip(&command.expected_keys)
                .any(|(change, expected)| change.key != *expected)
        {
            return Err(invalid());
        }
        let transition_id = uuid::Uuid::parse_str(&command.transition_id).map_err(|_| invalid())?;
        let mut transaction = self.pool.begin().await.map_err(|_| unavailable())?;
        let row = sqlx::query(
            "select id,source_generation_id,migration_cursor,migration_complete, \
                    maximum_records,maximum_bytes,maximum_value_bytes,next_record_version, \
                    record_count,total_bytes \
             from plugin_state_generations \
             where transition_id=$1 and namespace=$2 and status='staging' for update",
        )
        .bind(transition_id)
        .bind(&command.namespace)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| unavailable())?
        .ok_or_else(not_found)?;
        let cursor: Option<String> = row.try_get("migration_cursor").map_err(|_| unavailable())?;
        if cursor != command.cursor
            || row
                .try_get::<bool, _>("migration_complete")
                .map_err(|_| unavailable())?
        {
            return Err(conflict());
        }
        let generation = WritableGeneration {
            id: row.try_get("id").map_err(|_| unavailable())?,
            maximum_records: u64_from_i64(
                row.try_get("maximum_records").map_err(|_| unavailable())?,
            )?,
            maximum_bytes: u64_from_i64(row.try_get("maximum_bytes").map_err(|_| unavailable())?)?,
            maximum_value_bytes: u64_from_i64(
                row.try_get("maximum_value_bytes")
                    .map_err(|_| unavailable())?,
            )?,
            next_record_version: u64_from_i64(
                row.try_get("next_record_version")
                    .map_err(|_| unavailable())?,
            )?,
            record_count: u64_from_i64(row.try_get("record_count").map_err(|_| unavailable())?)?,
            total_bytes: u64_from_i64(row.try_get("total_bytes").map_err(|_| unavailable())?)?,
        };
        let source: uuid::Uuid = row
            .try_get("source_generation_id")
            .map_err(|_| unavailable())?;
        let limit =
            i64::try_from(command.expected_keys.len().saturating_add(1)).map_err(|_| invalid())?;
        let source_rows = sqlx::query(
            "select state_key,value_json from plugin_state_records \
             where generation_id=$1 and ($2::text is null or state_key>$2) \
             order by state_key limit $3",
        )
        .bind(source)
        .bind(command.cursor.as_deref())
        .bind(limit)
        .fetch_all(&mut *transaction)
        .await
        .map_err(|_| unavailable())?;
        if source_rows.len() < command.expected_keys.len()
            || source_rows
                .iter()
                .take(command.expected_keys.len())
                .zip(&command.expected_keys)
                .any(|(row, expected)| {
                    row.try_get::<String, _>("state_key").ok().as_ref() != Some(expected)
                })
            || (command.expected_keys.is_empty() && !source_rows.is_empty())
        {
            return Err(conflict());
        }
        let mut next_version = generation.next_record_version;
        let mut record_count = generation.record_count;
        let mut total_bytes = generation.total_bytes;
        for ((change, source), expected_key) in command
            .changes
            .iter()
            .zip(source_rows.iter())
            .zip(&command.expected_keys)
        {
            let value = match &change.action {
                PluginStateMigrationAction::Keep => {
                    Some(source.try_get("value_json").map_err(|_| unavailable())?)
                }
                PluginStateMigrationAction::Replace(value) => Some(value.clone()),
                PluginStateMigrationAction::Delete => None,
            };
            let Some(value) = value else {
                continue;
            };
            let bytes = u64::try_from(serde_json::to_vec(&value).map_err(|_| invalid())?.len())
                .map_err(|_| quota())?;
            if bytes == 0 || bytes > generation.maximum_value_bytes {
                return Err(quota());
            }
            record_count = record_count.checked_add(1).ok_or_else(quota)?;
            total_bytes = total_bytes.checked_add(bytes).ok_or_else(quota)?;
            if record_count > generation.maximum_records || total_bytes > generation.maximum_bytes {
                return Err(quota());
            }
            let version = next_version;
            next_version = next_version.checked_add(1).ok_or_else(quota)?;
            sqlx::query(
                "insert into plugin_state_records( \
                   generation_id,state_key,value_json,value_bytes,record_version \
                 ) values ($1,$2,$3,$4,$5)",
            )
            .bind(generation.id)
            .bind(expected_key)
            .bind(value)
            .bind(i64::try_from(bytes).map_err(|_| quota())?)
            .bind(i64::try_from(version).map_err(|_| quota())?)
            .execute(&mut *transaction)
            .await
            .map_err(|error| {
                if error
                    .as_database_error()
                    .is_some_and(|database| database.is_unique_violation())
                {
                    conflict()
                } else {
                    unavailable()
                }
            })?;
        }
        let complete = command.expected_keys.is_empty();
        let next_cursor = command.expected_keys.last().or(command.cursor.as_ref());
        sqlx::query(
            "update plugin_state_generations set migration_cursor=$2,migration_complete=$3, \
                 next_record_version=$4,record_count=$5,total_bytes=$6 where id=$1",
        )
        .bind(generation.id)
        .bind(next_cursor)
        .bind(complete)
        .bind(i64::try_from(next_version).map_err(|_| quota())?)
        .bind(i64::try_from(record_count).map_err(|_| quota())?)
        .bind(i64::try_from(total_bytes).map_err(|_| quota())?)
        .execute(&mut *transaction)
        .await
        .map_err(|_| unavailable())?;
        transaction.commit().await.map_err(|_| unavailable())?;
        Ok(())
    }

    async fn abort_transition(&self, transition_id: &str) -> PluginStateStoreResult<()> {
        let transition_id = uuid::Uuid::parse_str(transition_id).map_err(|_| invalid())?;
        sqlx::query(
            "delete from plugin_state_generations where transition_id=$1 and status='staging'",
        )
        .bind(transition_id)
        .execute(&self.pool)
        .await
        .map_err(|_| unavailable())?;
        Ok(())
    }
}

async fn writable_generation(
    transaction: &mut Transaction<'_, Postgres>,
    owner: &PluginStateOwner,
    namespace: &str,
) -> PluginStateStoreResult<WritableGeneration> {
    let namespace_owner = owner.namespace(namespace).ok_or_else(denied)?;
    let generation =
        uuid::Uuid::parse_str(namespace_owner.generation_id()).map_err(|_| denied())?;
    let fence = uuid::Uuid::parse_str(namespace_owner.fence()).map_err(|_| denied())?;
    let instance = uuid::Uuid::parse_str(owner.instance_id()).map_err(|_| denied())?;
    let row = sqlx::query(
        "select g.id,g.maximum_records,g.maximum_bytes,g.maximum_value_bytes, \
                g.next_record_version,g.record_count,g.total_bytes \
         from plugin_state_generations g join plugin_instances i on i.id=g.instance_id \
         where g.id=$1 and g.fence=$2 and g.instance_id=$3 and g.namespace=$4 \
           and g.artifact_sha256=$5 and g.instance_revision=$6 and g.status='active' \
           and i.artifact_sha256=g.artifact_sha256 and i.revision=g.instance_revision \
         for update of g",
    )
    .bind(generation)
    .bind(fence)
    .bind(instance)
    .bind(namespace)
    .bind(owner.artifact_sha256())
    .bind(revision_i64(owner.instance_revision())?)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| unavailable())?
    .ok_or_else(denied)?;
    Ok(WritableGeneration {
        id: row.try_get("id").map_err(|_| unavailable())?,
        maximum_records: u64_from_i64(row.try_get("maximum_records").map_err(|_| unavailable())?)?,
        maximum_bytes: u64_from_i64(row.try_get("maximum_bytes").map_err(|_| unavailable())?)?,
        maximum_value_bytes: u64_from_i64(
            row.try_get("maximum_value_bytes")
                .map_err(|_| unavailable())?,
        )?,
        next_record_version: u64_from_i64(
            row.try_get("next_record_version")
                .map_err(|_| unavailable())?,
        )?,
        record_count: u64_from_i64(row.try_get("record_count").map_err(|_| unavailable())?)?,
        total_bytes: u64_from_i64(row.try_get("total_bytes").map_err(|_| unavailable())?)?,
    })
}

/// 实例与状态代次在同一事务绑定；只有这里会晋升 staging generation
pub(super) async fn commit_configuration(
    transaction: &mut Transaction<'_, Postgres>,
    instance: &PluginInstance,
    revision: Revision,
    commit: &PluginStateCommit,
) -> AdminStoreResult<()> {
    validate_configuration(&commit.configuration).map_err(|_| admin_invalid())?;
    let instance_id = uuid::Uuid::parse_str(&instance.id).map_err(|_| admin_invalid())?;
    let active = active_generations(transaction, instance_id, true)
        .await
        .map_err(|_| admin_unavailable())?;
    let targets = target_map(&commit.configuration);
    let transition = commit
        .transition_id
        .as_deref()
        .map(uuid::Uuid::parse_str)
        .transpose()
        .map_err(|_| admin_invalid())?;
    let revision = i64::try_from(revision.get()).map_err(|_| admin_invalid())?;
    let mut handled = BTreeSet::new();
    for generation in active {
        let Some(schema) = targets.get(generation.namespace.as_str()) else {
            if generation.record_count != 0 {
                return Err(admin_conflict());
            }
            sqlx::query("delete from plugin_state_generations where id=$1")
                .bind(generation.id)
                .execute(&mut **transaction)
                .await
                .map_err(|_| admin_unavailable())?;
            continue;
        };
        handled.insert(generation.namespace.clone());
        if schema.schema_version == generation.schema_version
            && schema.schema_sha256 == generation.schema_sha256
        {
            if generation.record_count > u64::from(schema.maximum_records)
                || generation.total_bytes > schema.maximum_bytes
                || generation.maximum_record_bytes > u64::from(schema.maximum_value_bytes)
            {
                return Err(admin_conflict());
            }
            sqlx::query(
                "update plugin_state_generations set artifact_sha256=$2, \
                   maximum_records=$3,maximum_bytes=$4,maximum_value_bytes=$5, \
                   instance_revision=$6,fence=$7 where id=$1",
            )
            .bind(generation.id)
            .bind(&instance.artifact_sha256)
            .bind(i64::from(schema.maximum_records))
            .bind(i64::try_from(schema.maximum_bytes).map_err(|_| admin_invalid())?)
            .bind(i64::from(schema.maximum_value_bytes))
            .bind(revision)
            .bind(uuid::Uuid::new_v4())
            .execute(&mut **transaction)
            .await
            .map_err(|_| admin_unavailable())?;
            continue;
        }
        let transition = transition.ok_or_else(admin_conflict)?;
        let staging = sqlx::query(
            "select id,record_count,total_bytes,migration_complete,source_generation_id, \
                    schema_version,schema_sha256,artifact_sha256 \
             from plugin_state_generations \
             where transition_id=$1 and instance_id=$2 and namespace=$3 and status='staging' \
             for update",
        )
        .bind(transition)
        .bind(instance_id)
        .bind(&generation.namespace)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(|_| admin_unavailable())?
        .ok_or_else(admin_conflict)?;
        if !staging
            .try_get::<bool, _>("migration_complete")
            .map_err(|_| admin_unavailable())?
            || staging
                .try_get::<uuid::Uuid, _>("source_generation_id")
                .map_err(|_| admin_unavailable())?
                != generation.id
            || u32::try_from(
                staging
                    .try_get::<i64, _>("schema_version")
                    .map_err(|_| admin_unavailable())?,
            )
            .map_err(|_| admin_unavailable())?
                != schema.schema_version
            || staging
                .try_get::<String, _>("schema_sha256")
                .map_err(|_| admin_unavailable())?
                != schema.schema_sha256
            || staging
                .try_get::<String, _>("artifact_sha256")
                .map_err(|_| admin_unavailable())?
                != instance.artifact_sha256
            || u64::try_from(
                staging
                    .try_get::<i64, _>("record_count")
                    .map_err(|_| admin_unavailable())?,
            )
            .map_err(|_| admin_unavailable())?
                > u64::from(schema.maximum_records)
            || u64::try_from(
                staging
                    .try_get::<i64, _>("total_bytes")
                    .map_err(|_| admin_unavailable())?,
            )
            .map_err(|_| admin_unavailable())?
                > schema.maximum_bytes
        {
            return Err(admin_conflict());
        }
        let staging_id: uuid::Uuid = staging.try_get("id").map_err(|_| admin_unavailable())?;
        // 先解除迁移来源，再替换活动代次；任一步失败都会连同配置和旧记录一起回滚
        sqlx::query("update plugin_state_generations set source_generation_id=null where id=$1")
            .bind(staging_id)
            .execute(&mut **transaction)
            .await
            .map_err(|_| admin_unavailable())?;
        sqlx::query("delete from plugin_state_generations where id=$1")
            .bind(generation.id)
            .execute(&mut **transaction)
            .await
            .map_err(|_| admin_unavailable())?;
        sqlx::query(
            "update plugin_state_generations set status='active',instance_revision=$2, \
             fence=$3,promoted_at=now(),transition_id=null,migration_cursor=null where id=$1",
        )
        .bind(staging_id)
        .bind(revision)
        .bind(uuid::Uuid::new_v4())
        .execute(&mut **transaction)
        .await
        .map_err(|_| admin_unavailable())?;
    }
    for schema in &commit.configuration.namespaces {
        if handled.contains(&schema.namespace) {
            continue;
        }
        let id = uuid::Uuid::new_v4();
        sqlx::query(
            "insert into plugin_state_generations( \
               id,instance_id,namespace,artifact_sha256,schema_version, \
               schema_sha256,maximum_records,maximum_bytes,maximum_value_bytes, \
               instance_revision,fence,status,migration_complete,promoted_at \
             ) values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,'active',true,now())",
        )
        .bind(id)
        .bind(instance_id)
        .bind(&schema.namespace)
        .bind(&instance.artifact_sha256)
        .bind(i64::from(schema.schema_version))
        .bind(&schema.schema_sha256)
        .bind(i64::from(schema.maximum_records))
        .bind(i64::try_from(schema.maximum_bytes).map_err(|_| admin_invalid())?)
        .bind(i64::from(schema.maximum_value_bytes))
        .bind(revision)
        .bind(uuid::Uuid::new_v4())
        .execute(&mut **transaction)
        .await
        .map_err(|error| {
            if error
                .as_database_error()
                .is_some_and(|database| database.is_unique_violation())
            {
                admin_conflict()
            } else {
                admin_unavailable()
            }
        })?;
    }
    if let Some(transition) = transition {
        let incomplete: bool = sqlx::query_scalar(
            "select exists(select 1 from plugin_state_generations \
             where transition_id=$1 and status='staging')",
        )
        .bind(transition)
        .fetch_one(&mut **transaction)
        .await
        .map_err(|_| admin_unavailable())?;
        if incomplete {
            return Err(admin_conflict());
        }
    }
    Ok(())
}

/// 紧急停用不依赖损坏包的 schema 解析；只允许原制品续绑并立即轮换 fence
pub(super) async fn rebind_existing_configuration(
    transaction: &mut Transaction<'_, Postgres>,
    instance_id: &str,
    artifact_sha256: &str,
    revision: Revision,
) -> AdminStoreResult<()> {
    let instance_id = uuid::Uuid::parse_str(instance_id).map_err(|_| admin_invalid())?;
    let mismatched: bool = sqlx::query_scalar(
        "select exists(select 1 from plugin_state_generations \
         where instance_id=$1 and status='active' and artifact_sha256<>$2)",
    )
    .bind(instance_id)
    .bind(artifact_sha256)
    .fetch_one(&mut **transaction)
    .await
    .map_err(|_| admin_unavailable())?;
    if mismatched {
        return Err(admin_conflict());
    }
    sqlx::query(
        "update plugin_state_generations set instance_revision=$2,fence=$3 \
         where instance_id=$1 and status='active'",
    )
    .bind(instance_id)
    .bind(i64::try_from(revision.get()).map_err(|_| admin_invalid())?)
    .bind(uuid::Uuid::new_v4())
    .execute(&mut **transaction)
    .await
    .map_err(|_| admin_unavailable())?;
    Ok(())
}
