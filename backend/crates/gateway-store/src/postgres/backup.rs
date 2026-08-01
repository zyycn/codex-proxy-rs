//! `backup_settings` 与 `backup_records` 的 PostgreSQL owner。
//!
//! 实现 `gateway-admin::ports::backup::BackupRepository`。本模块只执行事务与
//! 条件更新，不决定保留策略或状态机语义。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use secrecy::{ExposeSecret as _, SecretString};
use sqlx::{PgPool, Postgres, Row as _, Transaction};

use gateway_admin::model::backup::{
    BackupRecord, BackupRecordListQuery, BackupRecordPage, BackupRecordSeed, BackupSettings,
    BackupStatus, BackupTriggerKind, UpdateBackupScheduleCommand, UpdateBackupStorageCommand,
};
use gateway_admin::model::{MutationContext, Revision as AdminRevision};
use gateway_admin::ports::backup::{BackupRepository, StatusTransitionUpdate};
use gateway_admin::ports::store::{AdminStoreError, AdminStoreErrorKind, AdminStoreResult};

use crate::{
    StoreError, StoreResult,
    postgres::{
        admin_security_audit::AdminAuditEvent,
        runtime_settings::bump_config_revision_in_transaction,
    },
};

/// `backup_records` 的最大分页大小。
const BACKUP_PAGE_LIMIT: u32 = 200;

/// 备份任务/配置仓储。
#[derive(Clone)]
pub struct PgBackupRepository {
    pool: PgPool,
}

impl PgBackupRepository {
    /// 包装连接池。
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl BackupRepository for PgBackupRepository {
    async fn load_settings(&self) -> AdminStoreResult<BackupSettings> {
        load_backup_settings(&self.pool).await
    }

    async fn update_storage_settings(
        &self,
        command: UpdateBackupStorageCommand,
        context: &MutationContext,
    ) -> AdminStoreResult<(BackupSettings, AdminRevision)> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| store_unavailable("begin backup storage update"))?;
        let result = async {
            let current = lock_settings_in_transaction(&mut transaction).await?;
            ensure_storage_identity_stable(&current, &command, &mut transaction).await?;

            let secret = command
                .secret_access_key
                .as_ref()
                .map(|secret| secret.expose_secret().to_owned());
            sqlx::query(
                "update backup_settings
                    set endpoint = $1,
                        region = $2,
                        bucket = $3,
                        access_key_id = $4,
                        secret_access_key = coalesce($5, secret_access_key),
                        prefix = $6,
                        force_path_style = $7,
                        storage_revision = storage_revision + 1,
                        last_verified_at = null,
                        updated_at = now()
                  where id = 1",
            )
            .bind(&command.endpoint)
            .bind(&command.region)
            .bind(&command.bucket)
            .bind(&command.access_key_id)
            .bind(secret.as_deref())
            .bind(&command.prefix)
            .bind(command.force_path_style)
            .execute(&mut *transaction)
            .await
            .map_err(|_| store_unavailable("update backup storage"))?;

            let store_revision = bump_config_revision_in_transaction(&mut transaction)
                .await
                .map_err(map_admin_error)?;
            let revision = admin_revision(store_revision)?;
            let changed = storage_changed_fields(&current, &command);
            let audit = crate::mutation_audit(
                context,
                "backup.s3_config_updated",
                "backup_settings",
                "1",
                changed,
            );
            append_audit_in_transaction(&mut transaction, audit, Some(revision))
                .await
                .map_err(map_admin_error)?;

            let settings = load_settings_in_transaction(&mut transaction).await?;
            Ok((settings, revision))
        }
        .await;
        match result {
            Ok(result) => {
                transaction
                    .commit()
                    .await
                    .map_err(|_| store_unavailable("commit backup storage update"))?;
                Ok(result)
            }
            Err(error) => {
                transaction
                    .rollback()
                    .await
                    .map_err(|_| store_unavailable("rollback backup storage update"))?;
                Err(error)
            }
        }
    }

    async fn update_schedule_settings(
        &self,
        command: UpdateBackupScheduleCommand,
        next_run_at: Option<DateTime<Utc>>,
        context: &MutationContext,
    ) -> AdminStoreResult<BackupSettings> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| store_unavailable("begin backup schedule update"))?;
        let result = async {
            sqlx::query(
                "update backup_settings
                    set schedule_enabled = $1,
                        cron_expression = $2,
                        schedule_timezone = $3,
                        retention_days = $4,
                        retention_count = $5,
                        next_run_at = $6,
                        updated_at = now()
                  where id = 1",
            )
            .bind(command.schedule_enabled)
            .bind(&command.cron_expression)
            .bind(&command.schedule_timezone)
            .bind(i64::from(command.retention_days))
            .bind(i64::from(command.retention_count))
            .bind(next_run_at)
            .execute(&mut *transaction)
            .await
            .map_err(|_| store_unavailable("update backup schedule"))?;

            let audit = crate::mutation_audit(
                context,
                "backup.schedule_updated",
                "backup_settings",
                "1",
                vec![
                    "schedule_enabled".to_owned(),
                    "cron_expression".to_owned(),
                    "schedule_timezone".to_owned(),
                    "retention_days".to_owned(),
                    "retention_count".to_owned(),
                ],
            );
            append_audit_in_transaction(&mut transaction, audit, None)
                .await
                .map_err(map_admin_error)?;
            load_settings_in_transaction(&mut transaction).await
        }
        .await;
        match result {
            Ok(settings) => {
                transaction
                    .commit()
                    .await
                    .map_err(|_| store_unavailable("commit backup schedule update"))?;
                Ok(settings)
            }
            Err(error) => {
                transaction
                    .rollback()
                    .await
                    .map_err(|_| store_unavailable("rollback backup schedule update"))?;
                Err(error)
            }
        }
    }

    async fn record_verification(
        &self,
        storage_revision: u64,
        at: DateTime<Utc>,
    ) -> AdminStoreResult<bool> {
        let result = sqlx::query(
            "update backup_settings
                set last_verified_at = $2, updated_at = now()
              where id = 1 and storage_revision = $1",
        )
        .bind(i64::try_from(storage_revision).map_err(|_| store_invalid("storage revision"))?)
        .bind(at)
        .execute(&self.pool)
        .await
        .map_err(|_| store_unavailable("record backup storage verification"))?;
        Ok(result.rows_affected() == 1)
    }

    async fn insert_backup_record(&self, seed: BackupRecordSeed) -> AdminStoreResult<BackupRecord> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| store_unavailable("begin backup record insert"))?;
        let result = async {
            let _ = lock_settings_in_transaction(&mut transaction).await?;
            insert_queued_in_transaction(&mut transaction, &seed).await
        }
        .await;
        match result {
            Ok(record) => {
                transaction
                    .commit()
                    .await
                    .map_err(|_| store_unavailable("commit backup record insert"))?;
                Ok(record)
            }
            Err(error) => {
                transaction
                    .rollback()
                    .await
                    .map_err(|_| store_unavailable("rollback backup record insert"))?;
                Err(error)
            }
        }
    }

    async fn insert_scheduled_record(&self, seed: BackupRecordSeed) -> AdminStoreResult<bool> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| store_unavailable("begin scheduled insert"))?;
        match insert_queued_in_transaction(&mut transaction, &seed).await {
            Ok(_) => {
                transaction
                    .commit()
                    .await
                    .map_err(|_| store_unavailable("commit scheduled insert"))?;
                Ok(true)
            }
            Err(error) if is_active_or_scheduled_conflict(&error) => {
                transaction
                    .rollback()
                    .await
                    .map_err(|_| store_unavailable("rollback scheduled insert"))?;
                Ok(false)
            }
            Err(error) => {
                transaction
                    .rollback()
                    .await
                    .map_err(|_| store_unavailable("rollback scheduled insert"))?;
                Err(error)
            }
        }
    }

    async fn list_backup_records(
        &self,
        query: BackupRecordListQuery,
    ) -> AdminStoreResult<BackupRecordPage> {
        let limit = u32::from(query.page_size.get()).min(BACKUP_PAGE_LIMIT);
        let offset = u64::from(query.page.saturating_sub(1)) * u64::from(limit);

        let status = query.status.map(BackupStatus::as_str);
        let trigger = query.trigger.map(BackupTriggerKind::as_str);

        let total = sqlx::query_scalar::<_, i64>(
            "select count(*) from backup_records
              where ($1::text is null or status = $1)
                and ($2::text is null or trigger_kind = $2)",
        )
        .bind(status)
        .bind(trigger)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| store_unavailable("count backup records"))?;

        let rows = sqlx::query(
            "select id, trigger_kind, status, scheduled_at, object_key, size_bytes, sha256,
                    attempt_count, error_code, error_message, started_at, completed_at, expires_at,
                    created_at, updated_at
               from backup_records
              where ($1::text is null or status = $1)
                and ($2::text is null or trigger_kind = $2)
              order by created_at desc, id desc
              limit $3 offset $4",
        )
        .bind(status)
        .bind(trigger)
        .bind(i64::from(limit))
        .bind(i64::try_from(offset).map_err(|_| store_invalid("page offset"))?)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| store_unavailable("list backup records"))?;

        let items = rows
            .iter()
            .map(backup_record_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(BackupRecordPage {
            items,
            total: u64::try_from(total).map_err(|_| store_invalid("record count"))?,
            page: query.page,
            page_size: query.page_size,
        })
    }

    async fn load_backup_record(&self, id: &str) -> AdminStoreResult<Option<BackupRecord>> {
        sqlx::query(
            "select id, trigger_kind, status, scheduled_at, object_key, size_bytes, sha256,
                    attempt_count, error_code, error_message, started_at, completed_at, expires_at,
                    created_at, updated_at
               from backup_records where id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| store_unavailable("load backup record"))?
        .map(|row| backup_record_from_row(&row))
        .transpose()
    }

    async fn list_intermediate_records(&self) -> AdminStoreResult<Vec<BackupRecord>> {
        let rows = sqlx::query(
            "select id, trigger_kind, status, scheduled_at, object_key, size_bytes, sha256,
                    attempt_count, error_code, error_message, started_at, completed_at, expires_at,
                    created_at, updated_at
               from backup_records
              where status in ('dumping', 'uploading')
              order by created_at, id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|_| store_unavailable("list intermediate backup records"))?;
        rows.iter()
            .map(backup_record_from_row)
            .collect::<Result<Vec<_>, _>>()
    }

    async fn list_pending_deletions(&self, limit: u32) -> AdminStoreResult<Vec<BackupRecord>> {
        let limit = limit.clamp(1, 100);
        let rows = sqlx::query(
            "select id, trigger_kind, status, scheduled_at, object_key, size_bytes, sha256,
                    attempt_count, error_code, error_message, started_at, completed_at, expires_at,
                    created_at, updated_at
               from backup_records
              where status = 'deleting'
              order by created_at, id
              limit $1",
        )
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|_| store_unavailable("list pending deletions"))?;
        rows.iter()
            .map(backup_record_from_row)
            .collect::<Result<Vec<_>, _>>()
    }

    async fn list_expired_records(&self, limit: u32) -> AdminStoreResult<Vec<BackupRecord>> {
        let limit = limit.clamp(1, 1000);
        let now = Utc::now();
        let rows = sqlx::query(
            "select id, trigger_kind, status, scheduled_at, object_key, size_bytes, sha256,
                    attempt_count, error_code, error_message, started_at, completed_at, expires_at,
                    created_at, updated_at
               from backup_records
              where expires_at is not null
                and expires_at <= $1
                and status in ('completed', 'failed')
              order by created_at, id
              limit $2",
        )
        .bind(now)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|_| store_unavailable("list expired backups"))?;
        rows.iter()
            .map(backup_record_from_row)
            .collect::<Result<Vec<_>, _>>()
    }

    async fn claim_next_queued(
        &self,
        now: DateTime<Utc>,
    ) -> AdminStoreResult<Option<BackupRecord>> {
        sqlx::query(
            "with claimed as (
               select backup_records.id from backup_records
                where backup_records.status = 'queued'
                order by backup_records.created_at, backup_records.id
                limit 1
                for update skip locked
             )
             update backup_records br
                set status = 'dumping',
                    started_at = $1,
                    completed_at = null,
                    attempt_count = br.attempt_count + 1,
                    updated_at = $1
               from claimed
              where br.id = claimed.id
              returning br.id, br.trigger_kind, br.status, br.scheduled_at, br.object_key,
                        br.size_bytes, br.sha256, br.attempt_count, br.error_code,
                        br.error_message, br.started_at, br.completed_at, br.expires_at,
                        br.created_at, br.updated_at",
        )
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| store_unavailable("claim queued backup task"))?
        .map(|row| backup_record_from_row(&row))
        .transpose()
    }

    async fn transition_status(
        &self,
        id: &str,
        from: BackupStatus,
        to: BackupStatus,
        update: StatusTransitionUpdate,
        now: DateTime<Utc>,
    ) -> AdminStoreResult<Option<BackupRecord>> {
        let size_bytes = update
            .size_bytes
            .map(i64::try_from)
            .transpose()
            .map_err(|_| store_invalid("size bytes"))?;
        sqlx::query(
            "update backup_records
                set status = $1,
                    size_bytes = coalesce($2, size_bytes),
                    sha256 = coalesce($3, sha256),
                    error_code = coalesce($4, error_code),
                    error_message = coalesce($5, error_message),
                    completed_at = coalesce(
                        $6,
                        case when $1 in ('completed', 'failed') then $7 else completed_at end
                    ),
                    updated_at = $7
              where id = $8 and status = $9
              returning id, trigger_kind, status, scheduled_at, object_key, size_bytes, sha256,
                        attempt_count, error_code, error_message, started_at, completed_at, expires_at,
                        created_at, updated_at",
        )
        .bind(to.as_str())
        .bind(size_bytes)
        .bind(update.sha256.as_deref())
        .bind(update.error_code.as_deref())
        .bind(update.error_message.as_deref())
        .bind(update.completed_at)
        .bind(now)
        .bind(id)
        .bind(from.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| store_unavailable("transition backup task"))?
        .map(|row| backup_record_from_row(&row))
        .transpose()
    }

    async fn transition_to_deleting(
        &self,
        id: &str,
        now: DateTime<Utc>,
    ) -> AdminStoreResult<Option<BackupRecord>> {
        sqlx::query(
            "update backup_records
                set status = 'deleting', updated_at = $1
              where id = $2 and status in ('completed', 'failed')
              returning id, trigger_kind, status, scheduled_at, object_key, size_bytes, sha256,
                        attempt_count, error_code, error_message, started_at, completed_at, expires_at,
                        created_at, updated_at",
        )
        .bind(now)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| store_unavailable("transition backup to deleting"))?
        .map(|row| backup_record_from_row(&row))
        .transpose()
    }

    async fn delete_record(&self, id: &str) -> AdminStoreResult<()> {
        sqlx::query("delete from backup_records where id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(|_| store_unavailable("delete backup record"))
    }

    async fn advance_schedule_cursor(
        &self,
        next_run_at: DateTime<Utc>,
        expected_cron: &str,
        expected_timezone: &str,
    ) -> AdminStoreResult<bool> {
        let result = sqlx::query(
            "update backup_settings
                set next_run_at = $1
              where id = 1
                and schedule_enabled
                and cron_expression = $2
                and schedule_timezone = $3",
        )
        .bind(next_run_at)
        .bind(expected_cron)
        .bind(expected_timezone)
        .execute(&self.pool)
        .await
        .map_err(|_| store_unavailable("advance backup schedule cursor"))?;
        Ok(result.rows_affected() == 1)
    }

    async fn list_scheduled_completed_desc(
        &self,
        limit: u32,
    ) -> AdminStoreResult<Vec<BackupRecord>> {
        let limit = limit.clamp(1, 10000);
        let rows = sqlx::query(
            "select id, trigger_kind, status, scheduled_at, object_key, size_bytes, sha256,
                    attempt_count, error_code, error_message, started_at, completed_at, expires_at,
                    created_at, updated_at
               from backup_records
              where trigger_kind = 'scheduled' and status = 'completed'
              order by completed_at desc, id desc
              limit $1",
        )
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|_| store_unavailable("list scheduled completed backups"))?;
        rows.iter()
            .map(backup_record_from_row)
            .collect::<Result<Vec<_>, _>>()
    }
}

/// Store 内部 Revision → Admin Revision 转换。
fn admin_revision(revision: crate::Revision) -> AdminStoreResult<AdminRevision> {
    AdminRevision::new(revision.get()).map_err(|_| {
        AdminStoreError::new(AdminStoreErrorKind::Invalid, "revision", "zero revision")
    })
}

/// 锁定并读取单例配置行。
async fn lock_settings_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
) -> AdminStoreResult<BackupSettings> {
    sqlx::query(
        "select storage_revision, endpoint, region, bucket, access_key_id, secret_access_key,
                prefix, force_path_style, schedule_enabled, cron_expression, schedule_timezone,
                retention_days, retention_count, next_run_at, last_verified_at, updated_at
           from backup_settings where id = 1 for update",
    )
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| store_unavailable("lock backup settings"))?
    .map(|row| backup_settings_from_row(&row))
    .transpose()?
    .ok_or_else(|| store_not_found("backup settings"))
}

async fn load_settings_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
) -> AdminStoreResult<BackupSettings> {
    sqlx::query(
        "select storage_revision, endpoint, region, bucket, access_key_id, secret_access_key,
                prefix, force_path_style, schedule_enabled, cron_expression, schedule_timezone,
                retention_days, retention_count, next_run_at, last_verified_at, updated_at
           from backup_settings where id = 1",
    )
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| store_unavailable("load backup settings in transaction"))?
    .map(|row| backup_settings_from_row(&row))
    .transpose()?
    .ok_or_else(|| store_not_found("backup settings"))
}

async fn load_backup_settings(pool: &PgPool) -> AdminStoreResult<BackupSettings> {
    sqlx::query(
        "select storage_revision, endpoint, region, bucket, access_key_id, secret_access_key,
                prefix, force_path_style, schedule_enabled, cron_expression, schedule_timezone,
                retention_days, retention_count, next_run_at, last_verified_at, updated_at
           from backup_settings where id = 1",
    )
    .fetch_optional(pool)
    .await
    .map_err(|_| store_unavailable("load backup settings"))?
    .map(|row| backup_settings_from_row(&row))
    .transpose()?
    .ok_or_else(|| store_not_found("backup settings"))
}

/// 插入 queued 记录；返回记录或唯一约束冲突。
async fn insert_queued_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    seed: &BackupRecordSeed,
) -> AdminStoreResult<BackupRecord> {
    let row = sqlx::query(
        "insert into backup_records (
           id, trigger_kind, status, scheduled_at, object_key, expires_at, created_at, updated_at
         ) values ($1, $2, 'queued', $3, $4, $5, now(), now())
         returning id, trigger_kind, status, scheduled_at, object_key, size_bytes, sha256,
                   attempt_count, error_code, error_message, started_at, completed_at, expires_at,
                   created_at, updated_at",
    )
    .bind(&seed.id)
    .bind(seed.trigger_kind.as_str())
    .bind(seed.scheduled_at)
    .bind(&seed.object_key)
    .bind(seed.expires_at)
    .fetch_one(&mut **transaction)
    .await
    .map_err(map_insert_error)?;
    backup_record_from_row(&row)
}

/// 把唯一约束冲突映射为活跃任务/计划冲突。
fn map_insert_error(error: sqlx::Error) -> AdminStoreError {
    match &error {
        sqlx::Error::Database(database_error)
            if database_error.constraint() == Some("backup_records_active_uq") =>
        {
            AdminStoreError::new(
                AdminStoreErrorKind::Conflict,
                "backup record",
                "an active backup task already exists",
            )
        }
        sqlx::Error::Database(database_error)
            if database_error.constraint() == Some("backup_records_scheduled_uq") =>
        {
            AdminStoreError::new(
                AdminStoreErrorKind::Conflict,
                "backup record",
                "a scheduled backup already exists at this time",
            )
        }
        _ => AdminStoreError::new(
            AdminStoreErrorKind::Unavailable,
            "backup record",
            "backup record insert failed",
        ),
    }
}

fn is_active_or_scheduled_conflict(error: &AdminStoreError) -> bool {
    error.kind() == AdminStoreErrorKind::Conflict
}

/// 已有记录时锁定存储身份：endpoint/region/bucket/path-style 不允许变化。
async fn ensure_storage_identity_stable(
    current: &BackupSettings,
    command: &UpdateBackupStorageCommand,
    transaction: &mut Transaction<'_, Postgres>,
) -> AdminStoreResult<()> {
    let identity_changed = current.endpoint.as_deref() != Some(command.endpoint.as_str())
        || current.region.as_deref() != Some(command.region.as_str())
        || current.bucket.as_deref() != Some(command.bucket.as_str())
        || current.force_path_style != command.force_path_style;
    if !identity_changed {
        return Ok(());
    }
    let records_exist =
        sqlx::query_scalar::<_, bool>("select exists(select 1 from backup_records)")
            .fetch_one(&mut **transaction)
            .await
            .map_err(|_| store_unavailable("check backup records existence"))?;
    if records_exist {
        return Err(AdminStoreError::new(
            AdminStoreErrorKind::Conflict,
            "backup storage",
            "storage identity cannot change while backup records exist",
        ));
    }
    Ok(())
}

fn storage_changed_fields(
    current: &BackupSettings,
    command: &UpdateBackupStorageCommand,
) -> Vec<String> {
    let mut fields = Vec::new();
    if current.endpoint.as_deref() != Some(command.endpoint.as_str()) {
        fields.push("endpoint".to_owned());
    }
    if current.region.as_deref() != Some(command.region.as_str()) {
        fields.push("region".to_owned());
    }
    if current.bucket.as_deref() != Some(command.bucket.as_str()) {
        fields.push("bucket".to_owned());
    }
    if current.access_key_id.as_deref() != Some(command.access_key_id.as_str()) {
        fields.push("access_key_id".to_owned());
    }
    if command.secret_access_key.is_some() {
        fields.push("secret_access_key".to_owned());
    }
    if current.prefix.as_deref() != Some(command.prefix.as_str()) {
        fields.push("prefix".to_owned());
    }
    if current.force_path_style != command.force_path_style {
        fields.push("force_path_style".to_owned());
    }
    fields
}

/// 在同事务追加审计事件；`revision` 为空时不写 config_revision。
async fn append_audit_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    mut event: AdminAuditEvent,
    revision: Option<AdminRevision>,
) -> StoreResult<()> {
    event.config_revision = revision
        .map(|revision| {
            i64::try_from(revision.get()).map_err(|_| StoreError::InvalidData {
                entity: "backup",
                message: "revision overflow".to_owned(),
            })
        })
        .transpose()?;
    event.validate()?;
    sqlx::query(
        "insert into admin_audit_events (
           id, actor_kind, actor_admin_user_id, actor_ref, admin_request_id,
           action, entity_kind, entity_ref, config_revision, changed_fields, created_at
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(event.id)
    .bind(event.actor_kind.as_str())
    .bind(event.actor_admin_user_id)
    .bind(event.actor_ref)
    .bind(event.admin_request_id)
    .bind(event.action)
    .bind(event.entity_kind)
    .bind(event.entity_ref)
    .bind(event.config_revision)
    .bind(event.changed_fields)
    .bind(event.created_at)
    .execute(&mut **transaction)
    .await
    .map_err(|_| crate::postgres_unavailable("append backup audit event"))?;
    Ok(())
}

fn backup_record_from_row(row: &sqlx::postgres::PgRow) -> AdminStoreResult<BackupRecord> {
    let size_bytes = decode(row.try_get::<Option<i64>, _>("size_bytes"))?
        .map(u64::try_from)
        .transpose()
        .map_err(|_| store_invalid("size_bytes"))?;
    Ok(BackupRecord {
        id: decode(row.try_get("id"))?,
        trigger_kind: BackupTriggerKind::parse(decode(row.try_get::<&str, _>("trigger_kind"))?)
            .ok_or_else(|| store_invalid("trigger_kind"))?,
        status: BackupStatus::parse(decode(row.try_get::<&str, _>("status"))?)
            .ok_or_else(|| store_invalid("status"))?,
        scheduled_at: decode(row.try_get("scheduled_at"))?,
        object_key: decode(row.try_get("object_key"))?,
        size_bytes,
        sha256: decode(row.try_get("sha256"))?,
        attempt_count: u32::try_from(decode(row.try_get::<i32, _>("attempt_count"))?)
            .map_err(|_| store_invalid("attempt_count"))?,
        error_code: decode(row.try_get("error_code"))?,
        error_message: decode(row.try_get("error_message"))?,
        started_at: decode(row.try_get("started_at"))?,
        completed_at: decode(row.try_get("completed_at"))?,
        expires_at: decode(row.try_get("expires_at"))?,
        created_at: decode(row.try_get("created_at"))?,
        updated_at: decode(row.try_get("updated_at"))?,
    })
}

fn backup_settings_from_row(row: &sqlx::postgres::PgRow) -> AdminStoreResult<BackupSettings> {
    let storage_revision = u64::try_from(decode(row.try_get::<i64, _>("storage_revision"))?)
        .map_err(|_| store_invalid("storage_revision"))?;
    let secret = decode(row.try_get::<Option<String>, _>("secret_access_key"))?;
    Ok(BackupSettings {
        storage_revision,
        endpoint: decode(row.try_get("endpoint"))?,
        region: decode(row.try_get("region"))?,
        bucket: decode(row.try_get("bucket"))?,
        access_key_id: decode(row.try_get("access_key_id"))?,
        secret_access_key: secret.map(SecretString::from),
        prefix: decode(row.try_get("prefix"))?,
        force_path_style: decode(row.try_get("force_path_style"))?,
        schedule_enabled: decode(row.try_get("schedule_enabled"))?,
        cron_expression: decode(row.try_get("cron_expression"))?,
        schedule_timezone: decode(row.try_get("schedule_timezone"))?,
        retention_days: u32::try_from(decode(row.try_get::<i64, _>("retention_days"))?)
            .map_err(|_| store_invalid("retention_days"))?,
        retention_count: u32::try_from(decode(row.try_get::<i64, _>("retention_count"))?)
            .map_err(|_| store_invalid("retention_count"))?,
        next_run_at: decode(row.try_get("next_run_at"))?,
        last_verified_at: decode(row.try_get("last_verified_at"))?,
        updated_at: decode(row.try_get("updated_at"))?,
    })
}

/// sqlx 解码错误统一映射为备份 InvalidData。
fn decode<T>(result: Result<T, sqlx::Error>) -> AdminStoreResult<T> {
    result.map_err(|_| {
        AdminStoreError::new(
            AdminStoreErrorKind::Invalid,
            "backup",
            "backup record decode failed",
        )
    })
}

/// StoreError → AdminStoreError 的显式映射。
fn map_admin_error(error: StoreError) -> AdminStoreError {
    match error {
        StoreError::Unavailable { .. } => AdminStoreError::new(
            AdminStoreErrorKind::Unavailable,
            "backup",
            "backup store unavailable",
        ),
        StoreError::NotFound { entity, .. } => {
            AdminStoreError::new(AdminStoreErrorKind::NotFound, entity, "not found")
        }
        StoreError::Conflict { entity, .. } => {
            AdminStoreError::new(AdminStoreErrorKind::Conflict, entity, "conflict")
        }
        StoreError::InvalidData { entity, message } => {
            AdminStoreError::new(AdminStoreErrorKind::Invalid, entity, message)
        }
    }
}

fn store_unavailable(operation: &'static str) -> AdminStoreError {
    AdminStoreError::new(
        AdminStoreErrorKind::Unavailable,
        "backup",
        format!("{operation}: store unavailable"),
    )
}

fn store_not_found(entity: &'static str) -> AdminStoreError {
    AdminStoreError::new(AdminStoreErrorKind::NotFound, entity, "not found")
}

fn store_invalid(message: &'static str) -> AdminStoreError {
    AdminStoreError::new(AdminStoreErrorKind::Invalid, "backup", message)
}
