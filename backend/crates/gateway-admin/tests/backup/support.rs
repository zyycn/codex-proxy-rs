//! 备份测试共享 fixture：内存 fake 端口与辅助函数。
//!
//! 用内存 fake 端口驱动编排层，不依赖 MinIO/真实 S3。
//! s3.rs / pg_dump.rs 是 SDK 薄适配器，协议正确性交给 aws-sdk / pg_dump 保证。
//! 供 backup::task 与 use_case::backup 的测试复用。

use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gateway_admin::{
    model::{
        MutationActor, MutationContext, Revision,
        auth::AdminAuditEvent,
        backup::{
            BackupError, BackupObjectMetadata, BackupRecord, BackupRecordListQuery,
            BackupRecordPage, BackupRecordSeed, BackupSettings, BackupStatus, BackupStorageConfig,
            BackupTriggerKind, ConnectionTestResult, UpdateBackupScheduleCommand,
            UpdateBackupStorageCommand,
        },
    },
    ports::{
        backup::{
            BackupObjectStorePort, BackupRepository, DatabaseDumpPort, DumpArtifact, DumpRequest,
            StagedArtifact, StatusTransitionUpdate, UploadObjectRequest,
        },
        store::{AdminStoreError, AdminStoreErrorKind, AdminStoreResult, AuthStore},
    },
};
use secrecy::SecretString;
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;

/// 假归档内容；dump 与上传校验共用同一字节与校验值。
pub(crate) const DUMP_CONTENT: &[u8] = b"custom-format-archive-bytes-0123456789";

/// 计算一段内容的 SHA-256 十六进制。
pub(crate) fn sha256_hex(content: &[u8]) -> String {
    hex::encode(Sha256::digest(content))
}

/// 返回 32 位十六进制的合法备份记录 id。
pub(crate) fn backup_id(suffix: &str) -> String {
    format!("backup_{}", sha256_hex(suffix.as_bytes()))
}

/// 已配置且已通过连接测试的设置快照。
pub(crate) fn configured_settings() -> BackupSettings {
    let now = Utc::now();
    BackupSettings {
        storage_revision: 1,
        endpoint: Some("https://s3.example.com".to_owned()),
        region: Some("auto".to_owned()),
        bucket: Some("backup-bucket".to_owned()),
        access_key_id: Some("ak".to_owned()),
        secret_access_key: Some(SecretString::from("sk-value")),
        prefix: Some("codex".to_owned()),
        force_path_style: false,
        schedule_enabled: false,
        cron_expression: None,
        schedule_timezone: None,
        retention_days: 0,
        retention_count: 0,
        next_run_at: None,
        last_verified_at: Some(now),
        updated_at: now,
    }
}

/// 从 seed 构造 queued 记录。
pub(crate) fn queued_record(seed: &BackupRecordSeed) -> BackupRecord {
    let now = Utc::now();
    BackupRecord {
        id: seed.id.clone(),
        trigger_kind: seed.trigger_kind,
        status: BackupStatus::Queued,
        scheduled_at: seed.scheduled_at,
        object_key: seed.object_key.clone(),
        size_bytes: None,
        sha256: None,
        attempt_count: 0,
        error_code: None,
        error_message: None,
        started_at: None,
        completed_at: None,
        expires_at: seed.expires_at,
        created_at: now,
        updated_at: now,
    }
}

/// 内存版备份仓储。
pub(crate) struct FakeBackupRepository {
    settings: Mutex<BackupSettings>,
    records: Mutex<Vec<BackupRecord>>,
}

impl FakeBackupRepository {
    pub(crate) fn new(settings: BackupSettings) -> Self {
        Self {
            settings: Mutex::new(settings),
            records: Mutex::new(Vec::new()),
        }
    }

    /// 测试辅助：读取当前全部记录。
    pub(crate) fn all_records(&self) -> Vec<BackupRecord> {
        self.records.lock().expect("records").clone()
    }

    /// 测试辅助：强制把一条记录标记为指定时间完成的 completed。
    pub(crate) fn set_completed(&self, id: &str, completed_at: DateTime<Utc>) {
        let mut records = self.records.lock().expect("records");
        if let Some(record) = records.iter_mut().find(|record| record.id == id) {
            record.status = BackupStatus::Completed;
            record.completed_at = Some(completed_at);
            record.updated_at = Utc::now();
        }
    }

    /// 测试辅助：强制把一条记录迁移到指定状态。
    pub(crate) fn force_status(&self, id: &str, status: BackupStatus) {
        let mut records = self.records.lock().expect("records");
        if let Some(record) = records.iter_mut().find(|record| record.id == id) {
            record.status = status;
            if matches!(status, BackupStatus::Completed | BackupStatus::Failed) {
                record.completed_at = Some(Utc::now());
            }
            record.updated_at = Utc::now();
        }
    }
}

#[async_trait]
impl BackupRepository for FakeBackupRepository {
    async fn load_settings(&self) -> AdminStoreResult<BackupSettings> {
        Ok(self.settings.lock().expect("settings").clone())
    }

    async fn update_storage_settings(
        &self,
        command: UpdateBackupStorageCommand,
        _context: &MutationContext,
    ) -> AdminStoreResult<(BackupSettings, Revision)> {
        let mut settings = self.settings.lock().expect("settings");
        settings.storage_revision += 1;
        settings.endpoint = Some(command.endpoint);
        settings.region = Some(command.region);
        settings.bucket = Some(command.bucket);
        settings.access_key_id = Some(command.access_key_id);
        if let Some(secret) = command.secret_access_key {
            settings.secret_access_key = Some(secret);
        }
        settings.prefix = Some(command.prefix);
        settings.force_path_style = command.force_path_style;
        settings.last_verified_at = None;
        settings.updated_at = Utc::now();
        let revision = Revision::new(settings.storage_revision).map_err(|_| {
            AdminStoreError::new(AdminStoreErrorKind::Invalid, "backup", "zero revision")
        })?;
        Ok((settings.clone(), revision))
    }

    async fn update_schedule_settings(
        &self,
        command: UpdateBackupScheduleCommand,
        next_run_at: Option<DateTime<Utc>>,
        _context: &MutationContext,
    ) -> AdminStoreResult<BackupSettings> {
        let mut settings = self.settings.lock().expect("settings");
        settings.schedule_enabled = command.schedule_enabled;
        settings.cron_expression = Some(command.cron_expression);
        settings.schedule_timezone = Some(command.schedule_timezone);
        settings.retention_days = command.retention_days;
        settings.retention_count = command.retention_count;
        settings.next_run_at = next_run_at;
        settings.updated_at = Utc::now();
        Ok(settings.clone())
    }

    async fn record_verification(
        &self,
        storage_revision: u64,
        at: DateTime<Utc>,
    ) -> AdminStoreResult<bool> {
        let mut settings = self.settings.lock().expect("settings");
        if settings.storage_revision != storage_revision {
            return Ok(false);
        }
        settings.last_verified_at = Some(at);
        Ok(true)
    }

    async fn insert_backup_record(&self, seed: BackupRecordSeed) -> AdminStoreResult<BackupRecord> {
        let mut records = self.records.lock().expect("records");
        if records.iter().any(|record| record.status.is_active()) {
            return Err(AdminStoreError::new(
                AdminStoreErrorKind::Conflict,
                "backup record",
                "an active backup task already exists",
            ));
        }
        let record = queued_record(&seed);
        records.push(record.clone());
        Ok(record)
    }

    async fn insert_scheduled_record(&self, seed: BackupRecordSeed) -> AdminStoreResult<bool> {
        let mut records = self.records.lock().expect("records");
        if records.iter().any(|record| record.status.is_active())
            || records.iter().any(|record| {
                record.trigger_kind == BackupTriggerKind::Scheduled
                    && record.scheduled_at == seed.scheduled_at
            })
        {
            return Ok(false);
        }
        records.push(queued_record(&seed));
        Ok(true)
    }

    async fn list_backup_records(
        &self,
        query: BackupRecordListQuery,
    ) -> AdminStoreResult<BackupRecordPage> {
        let records = self.records.lock().expect("records");
        let mut items = records
            .iter()
            .filter(|record| {
                query.status.is_none_or(|status| record.status == status)
                    && query
                        .trigger
                        .is_none_or(|trigger| record.trigger_kind == trigger)
            })
            .cloned()
            .collect::<Vec<_>>();
        items.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        let total = items.len() as u64;
        let offset =
            (u64::from(query.page.saturating_sub(1)) * u64::from(query.page_size.get())) as usize;
        let limit = usize::from(query.page_size.get());
        items = items.into_iter().skip(offset).take(limit).collect();
        Ok(BackupRecordPage {
            items,
            total,
            page: query.page,
            page_size: query.page_size,
        })
    }

    async fn load_backup_record(&self, id: &str) -> AdminStoreResult<Option<BackupRecord>> {
        Ok(self
            .records
            .lock()
            .expect("records")
            .iter()
            .find(|record| record.id == id)
            .cloned())
    }

    async fn list_intermediate_records(&self) -> AdminStoreResult<Vec<BackupRecord>> {
        Ok(self
            .records
            .lock()
            .expect("records")
            .iter()
            .filter(|record| {
                matches!(
                    record.status,
                    BackupStatus::Dumping | BackupStatus::Uploading
                )
            })
            .cloned()
            .collect())
    }

    async fn list_pending_deletions(&self, _limit: u32) -> AdminStoreResult<Vec<BackupRecord>> {
        Ok(self
            .records
            .lock()
            .expect("records")
            .iter()
            .filter(|record| record.status == BackupStatus::Deleting)
            .cloned()
            .collect())
    }

    async fn list_expired_records(&self, _limit: u32) -> AdminStoreResult<Vec<BackupRecord>> {
        let now = Utc::now();
        Ok(self
            .records
            .lock()
            .expect("records")
            .iter()
            .filter(|record| {
                record
                    .expires_at
                    .is_some_and(|expires_at| expires_at <= now)
                    && matches!(
                        record.status,
                        BackupStatus::Completed | BackupStatus::Failed
                    )
            })
            .cloned()
            .collect())
    }

    async fn claim_next_queued(
        &self,
        now: DateTime<Utc>,
    ) -> AdminStoreResult<Option<BackupRecord>> {
        let mut records = self.records.lock().expect("records");
        let Some(record) = records
            .iter_mut()
            .find(|record| record.status == BackupStatus::Queued)
        else {
            return Ok(None);
        };
        record.status = BackupStatus::Dumping;
        record.started_at = Some(now);
        record.attempt_count += 1;
        record.updated_at = now;
        Ok(Some(record.clone()))
    }

    async fn transition_status(
        &self,
        id: &str,
        from: BackupStatus,
        to: BackupStatus,
        update: StatusTransitionUpdate,
        now: DateTime<Utc>,
    ) -> AdminStoreResult<Option<BackupRecord>> {
        let mut records = self.records.lock().expect("records");
        let Some(record) = records
            .iter_mut()
            .find(|record| record.id == id && record.status == from)
        else {
            return Ok(None);
        };
        record.status = to;
        if let Some(size) = update.size_bytes {
            record.size_bytes = Some(size);
        }
        if let Some(sha256) = update.sha256 {
            record.sha256 = Some(sha256);
        }
        if let Some(code) = update.error_code {
            record.error_code = Some(code);
        }
        if let Some(message) = update.error_message {
            record.error_message = Some(message);
        }
        record.completed_at = update.completed_at.or_else(|| {
            matches!(to, BackupStatus::Completed | BackupStatus::Failed).then_some(now)
        });
        record.updated_at = now;
        Ok(Some(record.clone()))
    }

    async fn transition_to_deleting(
        &self,
        id: &str,
        now: DateTime<Utc>,
    ) -> AdminStoreResult<Option<BackupRecord>> {
        let mut records = self.records.lock().expect("records");
        let Some(record) = records
            .iter_mut()
            .find(|record| record.id == id && record.status.can_be_deleted())
        else {
            return Ok(None);
        };
        record.status = BackupStatus::Deleting;
        record.updated_at = now;
        Ok(Some(record.clone()))
    }

    async fn delete_record(&self, id: &str) -> AdminStoreResult<()> {
        let mut records = self.records.lock().expect("records");
        records.retain(|record| record.id != id);
        Ok(())
    }

    async fn advance_schedule_cursor(
        &self,
        next_run_at: DateTime<Utc>,
        _expected_cron: &str,
        _expected_timezone: &str,
    ) -> AdminStoreResult<bool> {
        let mut settings = self.settings.lock().expect("settings");
        settings.next_run_at = Some(next_run_at);
        Ok(true)
    }

    async fn list_scheduled_completed_desc(
        &self,
        _limit: u32,
    ) -> AdminStoreResult<Vec<BackupRecord>> {
        let mut records = self
            .records
            .lock()
            .expect("records")
            .iter()
            .filter(|record| {
                record.trigger_kind == BackupTriggerKind::Scheduled
                    && record.status == BackupStatus::Completed
            })
            .cloned()
            .collect::<Vec<_>>();
        records.sort_by(|a, b| {
            b.completed_at
                .cmp(&a.completed_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        Ok(records)
    }
}

/// 假 `pg_dump` 端口：把固定内容写入临时文件作为归档；可注入失败。
pub(crate) struct FakeDumpPort {
    dir: TempDir,
    fail_dump: bool,
}

impl FakeDumpPort {
    pub(crate) fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("temp dir"),
            fail_dump: false,
        }
    }

    /// 让下一次 `dump` 返回 `backup.pg_dump_failed`。
    pub(crate) fn fail_next_dump(mut self) -> Self {
        self.fail_dump = true;
        self
    }

    fn artifact(&self, backup_id: &str) -> DumpArtifact {
        let path = self.dir.path().join(format!("{backup_id}.dump"));
        std::fs::write(&path, DUMP_CONTENT).expect("write artifact");
        DumpArtifact {
            path,
            size_bytes: DUMP_CONTENT.len() as u64,
            sha256: sha256_hex(DUMP_CONTENT),
        }
    }
}

#[async_trait]
impl DatabaseDumpPort for FakeDumpPort {
    async fn dump(
        &self,
        request: DumpRequest,
    ) -> Result<DumpArtifact, gateway_admin::model::backup::BackupError> {
        if self.fail_dump {
            return Err(gateway_admin::model::backup::BackupError::new(
                gateway_admin::model::backup::code::PG_DUMP_FAILED,
                "pg_dump failed".to_owned(),
            ));
        }
        if request.cancellation.is_cancelled() {
            return Err(gateway_admin::model::backup::BackupError::new(
                gateway_admin::model::backup::code::CANCELLED,
                "cancelled".to_owned(),
            ));
        }
        Ok(self.artifact(&request.backup_id))
    }

    async fn inspect_staging(
        &self,
        backup_id: &str,
    ) -> Result<Option<StagedArtifact>, gateway_admin::model::backup::BackupError> {
        let path = self.dir.path().join(format!("{backup_id}.dump"));
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(StagedArtifact {
            path,
            size_bytes: DUMP_CONTENT.len() as u64,
            sha256: sha256_hex(DUMP_CONTENT),
        }))
    }

    async fn cleanup_staging(
        &self,
        _backup_id: &str,
    ) -> Result<(), gateway_admin::model::backup::BackupError> {
        Ok(())
    }
}

/// 内存版对象存储：把上传内容按 key 保存并支持 Head/Delete/预签名。
pub(crate) struct FakeObjectStore {
    pub(crate) objects: Mutex<std::collections::HashMap<String, Vec<u8>>>,
    upload_error: Option<BackupError>,
}

impl FakeObjectStore {
    pub(crate) fn new() -> Self {
        Self {
            objects: Mutex::new(std::collections::HashMap::new()),
            upload_error: None,
        }
    }

    /// 让上传返回指定的脱敏 S3 错误。
    pub(crate) fn fail_upload(mut self, code: &'static str, message: impl Into<String>) -> Self {
        self.upload_error = Some(BackupError::new(code, message.into()));
        self
    }

    pub(crate) fn object(&self, key: &str) -> Option<Vec<u8>> {
        self.objects.lock().expect("objects").get(key).cloned()
    }
}

#[async_trait]
impl BackupObjectStorePort for FakeObjectStore {
    async fn test_connection(
        &self,
        _config: &BackupStorageConfig,
    ) -> Result<ConnectionTestResult, gateway_admin::model::backup::BackupError> {
        Ok(ConnectionTestResult {
            ok: true,
            stage: "deleteObject",
            code: None,
            message: "连接测试成功".to_owned(),
        })
    }

    async fn upload_file(
        &self,
        _config: &BackupStorageConfig,
        request: UploadObjectRequest,
    ) -> Result<(), gateway_admin::model::backup::BackupError> {
        if let Some(error) = &self.upload_error {
            return Err(error.clone());
        }
        let content = std::fs::read(&request.source).map_err(|_| {
            gateway_admin::model::backup::BackupError::new(
                gateway_admin::model::backup::code::S3_UPLOAD_FAILED,
                "read source".to_owned(),
            )
        })?;
        self.objects
            .lock()
            .expect("objects")
            .insert(request.object_key, content);
        Ok(())
    }

    async fn head_object(
        &self,
        _config: &BackupStorageConfig,
        object_key: &str,
    ) -> Result<Option<BackupObjectMetadata>, gateway_admin::model::backup::BackupError> {
        let Some(content) = self.object(object_key) else {
            return Ok(None);
        };
        Ok(Some(BackupObjectMetadata::new(
            "backup-test".to_owned(),
            sha256_hex(&content),
            Utc::now(),
            content.len() as u64,
        )))
    }

    async fn delete_object(
        &self,
        _config: &BackupStorageConfig,
        object_key: &str,
    ) -> Result<(), gateway_admin::model::backup::BackupError> {
        self.objects.lock().expect("objects").remove(object_key);
        Ok(())
    }

    async fn presigned_download(
        &self,
        _config: &BackupStorageConfig,
        object_key: &str,
        _file_name: &str,
        _ttl: std::time::Duration,
    ) -> Result<String, gateway_admin::model::backup::BackupError> {
        Ok(format!("https://presigned.example.com/{object_key}"))
    }
}

/// 记录审计事件的假 AuthStore。
pub(crate) struct FakeAuthStore {
    audit: Mutex<Vec<AdminAuditEvent>>,
}

impl FakeAuthStore {
    pub(crate) fn new() -> Self {
        Self {
            audit: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn audit_actions(&self) -> Vec<String> {
        self.audit
            .lock()
            .expect("audit")
            .iter()
            .map(|event| event.action.clone())
            .collect()
    }
}

#[async_trait]
impl AuthStore for FakeAuthStore {
    async fn load_password_hash(&self, _admin_user_id: &str) -> AdminStoreResult<Option<String>> {
        Ok(None)
    }
    async fn create_password_hash_if_absent(
        &self,
        _admin_user_id: &str,
        _password_hash: &str,
    ) -> AdminStoreResult<bool> {
        Ok(true)
    }
    async fn load_admin_api_key(
        &self,
    ) -> AdminStoreResult<Option<gateway_admin::model::settings::AdminApiKey>> {
        Ok(None)
    }
    async fn load_session(
        &self,
        _session_id: &str,
    ) -> AdminStoreResult<Option<gateway_admin::model::auth::AdminSession>> {
        Ok(None)
    }
    async fn store_session(
        &self,
        _session_id: &str,
        _session: &gateway_admin::model::auth::AdminSession,
    ) -> AdminStoreResult<()> {
        Ok(())
    }
    async fn delete_session(
        &self,
        _session_id: &str,
    ) -> AdminStoreResult<Option<gateway_admin::model::auth::AdminSession>> {
        Ok(None)
    }
    async fn append_audit_event(&self, event: AdminAuditEvent) -> AdminStoreResult<()> {
        self.audit.lock().expect("audit").push(event);
        Ok(())
    }
}

/// 系统发起者上下文。
pub(crate) fn system_context() -> MutationContext {
    MutationContext {
        actor: MutationActor::System,
        request_id: "backup-runtime-test".to_owned(),
    }
}
