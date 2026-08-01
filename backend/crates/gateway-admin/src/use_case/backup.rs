//! 备份配置、手动创建、下载与删除用例。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gateway_core::routing::snapshot::SnapshotControl;
use secrecy::ExposeSecret as _;
use uuid::Uuid;

use crate::model::{
    AdminError, AdminErrorKind, MutationContext,
    auth::{AdminAuditEvent, AuditActorKind},
    backup::{
        BackupError, BackupRecord, BackupRecordListQuery, BackupRecordPage, BackupSettings,
        BackupStatus, BackupStorageConfig, BackupTriggerKind, ConnectionTestResult,
        DownloadUrlResult, UpdateBackupScheduleCommand, UpdateBackupStorageCommand,
        build_backup_seed, code,
    },
};
use crate::ports::{
    backup::{BackupObjectStorePort, BackupRepository},
    store::AuthStore,
};

use super::map_store_error;

/// 默认下载地址有效期。
const DOWNLOAD_TTL: Duration = Duration::from_secs(5 * 60);

/// API 消费的备份管理服务。
#[async_trait]
pub trait BackupService: Send + Sync {
    async fn load_settings(&self) -> Result<BackupSettings, AdminError>;

    async fn update_storage(
        &self,
        context: &MutationContext,
        command: UpdateBackupStorageCommand,
    ) -> Result<BackupSettings, AdminError>;

    async fn update_schedule(
        &self,
        context: &MutationContext,
        command: UpdateBackupScheduleCommand,
    ) -> Result<BackupSettings, AdminError>;

    async fn test_connection(
        &self,
        context: &MutationContext,
    ) -> Result<ConnectionTestResult, AdminError>;

    async fn create_backup(
        &self,
        context: &MutationContext,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<BackupRecord, AdminError>;

    async fn list_records(
        &self,
        query: BackupRecordListQuery,
    ) -> Result<BackupRecordPage, AdminError>;

    async fn download_url(
        &self,
        context: &MutationContext,
        backup_id: &str,
    ) -> Result<DownloadUrlResult, AdminError>;

    async fn delete_backup(
        &self,
        context: &MutationContext,
        backup_id: &str,
    ) -> Result<BackupRecord, AdminError>;
}

pub(crate) struct DefaultBackupService {
    repository: Arc<dyn BackupRepository>,
    object_store: Arc<dyn BackupObjectStorePort>,
    auth: Arc<dyn AuthStore>,
    snapshot: Arc<dyn SnapshotControl>,
}

impl DefaultBackupService {
    #[must_use]
    pub(crate) fn new(
        repository: Arc<dyn BackupRepository>,
        object_store: Arc<dyn BackupObjectStorePort>,
        auth: Arc<dyn AuthStore>,
        snapshot: Arc<dyn SnapshotControl>,
    ) -> Self {
        Self {
            repository,
            object_store,
            auth,
            snapshot,
        }
    }
}

#[async_trait]
impl BackupService for DefaultBackupService {
    async fn load_settings(&self) -> Result<BackupSettings, AdminError> {
        self.repository
            .load_settings()
            .await
            .map_err(|error| map_store_error(error, "backup settings"))
    }

    async fn update_storage(
        &self,
        context: &MutationContext,
        command: UpdateBackupStorageCommand,
    ) -> Result<BackupSettings, AdminError> {
        validate_storage(&command)?;
        let (settings, revision) = self
            .repository
            .update_storage_settings(command, context)
            .await
            .map_err(|error| map_store_error(error, "backup storage"))?;
        super::publish_committed(self.snapshot.as_ref(), revision).await?;
        Ok(settings)
    }

    async fn update_schedule(
        &self,
        context: &MutationContext,
        command: UpdateBackupScheduleCommand,
    ) -> Result<BackupSettings, AdminError> {
        validate_schedule(&command)?;
        let next_run_at = if command.schedule_enabled {
            Some(
                crate::backup::policy::BackupSchedule::parse(
                    &command.cron_expression,
                    &command.schedule_timezone,
                )
                .map_err(map_backup_error)?
                .next_after(Utc::now())
                .ok_or_else(|| AdminError::bad_request("无法从计划推导下次执行时间"))?,
            )
        } else {
            None
        };
        self.repository
            .update_schedule_settings(command, next_run_at, context)
            .await
            .map_err(|error| map_store_error(error, "backup schedule"))
    }

    async fn test_connection(
        &self,
        context: &MutationContext,
    ) -> Result<ConnectionTestResult, AdminError> {
        let settings = self
            .repository
            .load_settings()
            .await
            .map_err(|error| map_store_error(error, "backup settings"))?;
        let Some(config) = BackupStorageConfig::from_settings(&settings) else {
            return Ok(ConnectionTestResult {
                ok: false,
                stage: "putObject",
                code: Some(code::INVALID_CONFIG),
                message: "备份存储配置不完整，无法执行连接测试".to_owned(),
            });
        };
        let result = self
            .object_store
            .test_connection(&config)
            .await
            .map_err(map_backup_error)?;
        if result.ok {
            let _ = self
                .repository
                .record_verification(config.storage_revision, Utc::now())
                .await
                .map_err(|error| map_store_error(error, "backup settings"))?;
        }
        self.append_audit(
            context,
            "backup.s3_connection_tested",
            "backup_settings",
            vec![],
        )
        .await?;
        Ok(result)
    }

    async fn create_backup(
        &self,
        context: &MutationContext,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<BackupRecord, AdminError> {
        let settings = self
            .repository
            .load_settings()
            .await
            .map_err(|error| map_store_error(error, "backup settings"))?;
        if !settings.storage_configured() {
            return Err(AdminError::bad_request(
                "备份存储配置不完整，请先保存存储配置",
            ));
        }
        if !settings.storage_verified() {
            return Err(AdminError::bad_request(
                "备份存储尚未通过连接测试，请先测试存储配置",
            ));
        }
        let prefix = settings.prefix.as_deref().unwrap_or_default();
        let seed = build_backup_seed(
            BackupTriggerKind::Manual,
            None,
            prefix,
            Utc::now(),
            expires_at,
        )
        .map_err(map_backup_error)?;
        let record = self
            .repository
            .insert_backup_record(seed)
            .await
            .map_err(|error| map_store_error(error, "backup record"))?;
        self.append_audit(context, "backup.created", "backup", vec![record.id.clone()])
            .await?;
        Ok(record)
    }

    async fn list_records(
        &self,
        query: BackupRecordListQuery,
    ) -> Result<BackupRecordPage, AdminError> {
        self.repository
            .list_backup_records(query)
            .await
            .map_err(|error| map_store_error(error, "backup records"))
    }

    async fn download_url(
        &self,
        context: &MutationContext,
        backup_id: &str,
    ) -> Result<DownloadUrlResult, AdminError> {
        let record = self.require_record(backup_id).await?;
        if record.status != BackupStatus::Completed {
            return Err(AdminError::conflict(
                "只有 completed 的备份才能创建下载地址",
            ));
        }
        let settings = self
            .repository
            .load_settings()
            .await
            .map_err(|error| map_store_error(error, "backup settings"))?;
        let Some(config) = BackupStorageConfig::from_settings(&settings) else {
            return Err(AdminError::bad_request("备份存储配置不完整"));
        };
        let file_name = crate::model::backup::build_download_file_name(backup_id);
        let url = self
            .object_store
            .presigned_download(&config, &record.object_key, &file_name, DOWNLOAD_TTL)
            .await
            .map_err(map_backup_error)?;
        self.append_audit(
            context,
            "backup.download_url_created",
            "backup",
            vec![backup_id.to_owned()],
        )
        .await?;
        Ok(DownloadUrlResult {
            url,
            file_name,
            expires_in: DOWNLOAD_TTL,
        })
    }

    async fn delete_backup(
        &self,
        context: &MutationContext,
        backup_id: &str,
    ) -> Result<BackupRecord, AdminError> {
        let record = self.require_record(backup_id).await?;
        if !record.status.can_be_deleted() {
            return Err(AdminError::conflict(
                "只有 completed 或 failed 的备份才能删除",
            ));
        }
        let now = Utc::now();
        let deleting = self
            .repository
            .transition_to_deleting(backup_id, now)
            .await
            .map_err(|error| map_store_error(error, "backup record"))?
            .ok_or_else(|| AdminError::conflict("备份记录已进入删除流程"))?;
        self.append_audit(
            context,
            "backup.delete_requested",
            "backup",
            vec![backup_id.to_owned()],
        )
        .await?;
        Ok(deleting)
    }
}

impl DefaultBackupService {
    async fn require_record(&self, backup_id: &str) -> Result<BackupRecord, AdminError> {
        self.repository
            .load_backup_record(backup_id)
            .await
            .map_err(|error| map_store_error(error, "backup record"))?
            .ok_or_else(|| AdminError::not_found("备份记录不存在"))
    }

    async fn append_audit(
        &self,
        context: &MutationContext,
        action: &str,
        entity_ref: &str,
        changed_fields: Vec<String>,
    ) -> Result<(), AdminError> {
        let actor_kind = match context.actor {
            crate::model::MutationActor::AdminSession { .. } => AuditActorKind::AdminSession,
            crate::model::MutationActor::AdminApiKey => AuditActorKind::AdminApiKey,
            crate::model::MutationActor::System => AuditActorKind::System,
        };
        let actor_ref = match &context.actor {
            crate::model::MutationActor::AdminSession { admin_user_id } => admin_user_id.clone(),
            crate::model::MutationActor::AdminApiKey => "admin_api_key".to_owned(),
            crate::model::MutationActor::System => "system".to_owned(),
        };
        let actor_admin_user_id = match &context.actor {
            crate::model::MutationActor::AdminSession { admin_user_id } => {
                Some(admin_user_id.clone())
            }
            _ => None,
        };
        let event = AdminAuditEvent {
            id: format!("audit_{}", Uuid::now_v7().simple()),
            actor_kind,
            actor_admin_user_id,
            actor_ref,
            request_id: Some(context.request_id.clone()),
            action: action.to_owned(),
            entity_kind: "backup".to_owned(),
            entity_ref: entity_ref.to_owned(),
            config_revision: None,
            changed_fields,
            occurred_at: Utc::now(),
        };
        self.auth
            .append_audit_event(event)
            .await
            .map_err(|error| map_store_error(error, "backup audit"))
    }
}

/// 校验存储配置；空串视为非法，Secret 缺省表示保留旧值。
fn validate_storage(command: &UpdateBackupStorageCommand) -> Result<(), AdminError> {
    validate_endpoint(&command.endpoint)?;
    validate_nonempty("region", &command.region)?;
    validate_nonempty("bucket", &command.bucket)?;
    validate_nonempty("accessKeyId", &command.access_key_id)?;
    if let Some(secret) = command.secret_access_key.as_ref() {
        let value = secret.expose_secret();
        if value.is_empty() {
            return Err(AdminError::bad_request(
                "secretAccessKey 不能为空字符串；缺省表示保留旧值",
            ));
        }
        validate_length("secretAccessKey", value, 512)?;
    }
    crate::model::backup::normalize_prefix(&command.prefix).map_err(map_backup_error)?;
    Ok(())
}

fn validate_endpoint(value: &str) -> Result<(), AdminError> {
    if value.is_empty() {
        return Err(AdminError::bad_request("endpoint 不能为空"));
    }
    if !value.starts_with("https://") {
        return Err(AdminError::bad_request(
            "endpoint 必须使用 HTTPS；仅测试或显式开发配置允许 HTTP",
        ));
    }
    if value.contains('@') || value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(AdminError::bad_request("endpoint 格式非法"));
    }
    validate_length("endpoint", value, 2048)
}

fn validate_nonempty(field: &str, value: &str) -> Result<(), AdminError> {
    if value.is_empty() {
        return Err(AdminError::bad_request(format!("{field} 不能为空")));
    }
    validate_length(field, value, 255)
}

fn validate_length(field: &str, value: &str, max: usize) -> Result<(), AdminError> {
    if value.len() > max || value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(AdminError::bad_request(format!(
            "{field} 超出长度或包含非法字符"
        )));
    }
    Ok(())
}

/// 校验调度配置并解析 Cron 与时区。
fn validate_schedule(command: &UpdateBackupScheduleCommand) -> Result<(), AdminError> {
    if command.cron_expression.trim().is_empty() {
        return Err(AdminError::bad_request("cronExpression 不能为空"));
    }
    crate::backup::policy::BackupSchedule::parse(
        &command.cron_expression,
        &command.schedule_timezone,
    )
    .map_err(map_backup_error)?;
    Ok(())
}

/// 备份基础设施错误到既有 `AdminErrorKind` 的唯一边界映射。
fn map_backup_error(error: BackupError) -> AdminError {
    match error.code() {
        code::INVALID_CONFIG | code::INVALID_CRON | code::INVALID_TIMEZONE => {
            AdminError::bad_request(error.message().to_owned())
        }
        code::RECORD_NOT_FOUND => AdminError::not_found(error.message().to_owned()),
        code::ACTIVE_TASK_CONFLICT | code::STATE_CONFLICT | code::STORAGE_IDENTITY_LOCKED => {
            AdminError::conflict(error.message().to_owned())
        }
        code::STORE_UNAVAILABLE => {
            AdminError::new(AdminErrorKind::Unavailable, error.message().to_owned())
        }
        _ => AdminError::bad_gateway(error.message().to_owned()),
    }
}
