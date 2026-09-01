//! 备份控制面所需的持久化与基础设施能力。
//!
//! 端口方法只接受/返回领域类型与既有错误体系：`BackupRepository` 复用
//! `AdminStoreError`；`DatabaseDumpPort` 与 `BackupObjectStorePort` 返回只携带稳定
//! 错误码的 [`BackupError`]。`gateway-store` 实现全部端口；`gateway-admin` 拥有
//! 状态机、Cron、保留策略与 BackupTask 算法。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::model::Revision;
use crate::model::{
    MutationContext,
    backup::{
        BackupError, BackupObjectMetadata, BackupRecord, BackupRecordListQuery, BackupRecordPage,
        BackupRecordSeed, BackupSettings, BackupStatus, BackupStorageConfig, ConnectionTestResult,
        UpdateBackupScheduleCommand, UpdateBackupStorageCommand,
    },
};
use crate::ports::store::AdminStoreResult;

/// Worker 状态迁移携带的字段更新；`None` 字段不写入。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusTransitionUpdate {
    pub size_bytes: Option<u64>,
    pub sha256: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// 备份配置、任务与计划游标的 PostgreSQL 仓储端口。
#[async_trait]
pub trait BackupRepository: Send + Sync {
    /// 读取完整设置（含 Secret）。
    async fn load_settings(&self) -> AdminStoreResult<BackupSettings>;

    /// 同事务更新 S3 存储配置、递增 revision、清空验证状态并写审计事件。
    ///
    /// 已有记录时禁止切换 endpoint/region/bucket/path-style（存储身份锁定）。
    /// 返回更新后的设置与新的全局 `config_revision`。
    async fn update_storage_settings(
        &self,
        command: UpdateBackupStorageCommand,
        context: &MutationContext,
    ) -> AdminStoreResult<(BackupSettings, Revision)>;

    /// 同事务更新调度配置、推进计划游标并写审计事件。
    async fn update_schedule_settings(
        &self,
        command: UpdateBackupScheduleCommand,
        next_run_at: Option<DateTime<Utc>>,
        context: &MutationContext,
    ) -> AdminStoreResult<BackupSettings>;

    /// CAS 记录探针成功；`storage_revision` 与当前配置不一致时丢弃结果（返回 `false`）。
    async fn record_verification(
        &self,
        storage_revision: u64,
        at: DateTime<Utc>,
    ) -> AdminStoreResult<bool>;

    /// 插入 queued 任务；存在活跃任务时返回 `backup.active_task_conflict`。
    async fn insert_backup_record(&self, seed: BackupRecordSeed) -> AdminStoreResult<BackupRecord>;

    /// 幂等插入 scheduled 任务；同一时间点或活跃冲突时返回 `false`（调用方推进游标即可）。
    async fn insert_scheduled_record(&self, seed: BackupRecordSeed) -> AdminStoreResult<bool>;

    /// 分页查询记录。
    async fn list_backup_records(
        &self,
        query: BackupRecordListQuery,
    ) -> AdminStoreResult<BackupRecordPage>;

    /// 读取单条记录。
    async fn load_backup_record(&self, id: &str) -> AdminStoreResult<Option<BackupRecord>>;

    /// 读取中间状态记录（dumping/uploading），供启动恢复。
    async fn list_intermediate_records(&self) -> AdminStoreResult<Vec<BackupRecord>>;

    /// 读取等待完成墓碑的记录（status = deleting）。
    async fn list_pending_deletions(&self, limit: u32) -> AdminStoreResult<Vec<BackupRecord>>;

    /// 读取 `expires_at` 已到期且非活跃的记录（completed/failed），供保留清理。
    async fn list_expired_records(&self, limit: u32) -> AdminStoreResult<Vec<BackupRecord>>;

    /// 原子领取最旧的 queued 任务并迁移到 dumping；无任务返回 `None`。
    async fn claim_next_queued(&self, now: DateTime<Utc>)
    -> AdminStoreResult<Option<BackupRecord>>;

    /// 条件迁移状态：要求当前状态一致；不匹配返回 `None`。
    async fn transition_status(
        &self,
        id: &str,
        from: BackupStatus,
        to: BackupStatus,
        update: StatusTransitionUpdate,
        now: DateTime<Utc>,
    ) -> AdminStoreResult<Option<BackupRecord>>;

    /// 迁移到删除流程（completed/failed → deleting）；供管理员与保留策略调用。
    async fn transition_to_deleting(
        &self,
        id: &str,
        now: DateTime<Utc>,
    ) -> AdminStoreResult<Option<BackupRecord>>;

    /// 远端删除成功后硬删除记录行。
    async fn delete_record(&self, id: &str) -> AdminStoreResult<()>;

    /// 条件推进计划游标；仅当计划表达式和时区仍是 `expected_*` 时才写入，返回是否推进。
    async fn advance_schedule_cursor(
        &self,
        next_run_at: DateTime<Utc>,
        expected_cron: &str,
        expected_timezone: &str,
    ) -> AdminStoreResult<bool>;

    /// 按完成时间倒序读取 completed 计划备份，供保留策略扫描（最多 `limit` 条）。
    async fn list_scheduled_completed_desc(
        &self,
        limit: u32,
    ) -> AdminStoreResult<Vec<BackupRecord>>;
}

/// `pg_dump` 导出与本地暂存端口。
#[async_trait]
pub trait DatabaseDumpPort: Send + Sync {
    /// 创建归档；完成后原子落盘。取消时终止子进程并清理部分文件。
    async fn dump(&self, request: DumpRequest) -> Result<DumpArtifact, BackupError>;

    /// 检查暂存目录是否存在完整归档。
    async fn inspect_staging(&self, backup_id: &str)
    -> Result<Option<StagedArtifact>, BackupError>;

    /// 清理暂存目录中该备份的全部文件。
    async fn cleanup_staging(&self, backup_id: &str) -> Result<(), BackupError>;
}

/// 一次 `pg_dump` 请求。
#[derive(Debug, Clone)]
pub struct DumpRequest {
    pub backup_id: String,
    pub cancellation: gateway_core::lifecycle::CancellationToken,
}

/// 已完成并落盘的归档事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpArtifact {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub sha256: String,
}

/// 暂存目录中已存在的完整归档。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedArtifact {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub sha256: String,
}

/// S3 兼容对象存储适配器端口。
///
/// 方法接受已经校验的存储配置；SDK client 的构造与缓存属于 adapter 实现细节。
#[async_trait]
pub trait BackupObjectStorePort: Send + Sync {
    /// 对专用探针对象执行 Put/Head/Get/Delete。
    async fn test_connection(
        &self,
        config: &BackupStorageConfig,
    ) -> Result<ConnectionTestResult, BackupError>;

    /// 分片流式上传本地归档；取消时中止 multipart 并返回 `backup.cancelled`。
    async fn upload_file(
        &self,
        config: &BackupStorageConfig,
        request: UploadObjectRequest,
    ) -> Result<(), BackupError>;

    /// 读取对象大小与 metadata；对象不存在返回 `Ok(None)`。
    async fn head_object(
        &self,
        config: &BackupStorageConfig,
        object_key: &str,
    ) -> Result<Option<BackupObjectMetadata>, BackupError>;

    /// 删除对象；对象不存在视为成功。
    async fn delete_object(
        &self,
        config: &BackupStorageConfig,
        object_key: &str,
    ) -> Result<(), BackupError>;

    /// 创建短时预签名下载 URL。
    async fn presigned_download(
        &self,
        config: &BackupStorageConfig,
        object_key: &str,
        file_name: &str,
        ttl: Duration,
    ) -> Result<String, BackupError>;
}

/// 一次对象上传请求。
#[derive(Debug, Clone)]
pub struct UploadObjectRequest {
    pub object_key: String,
    pub source: PathBuf,
    pub metadata: BackupObjectMetadata,
    pub cancellation: gateway_core::lifecycle::CancellationToken,
}

/// 备份控制面所需的全部 Store 能力集合。
#[derive(Clone)]
pub struct BackupStorePorts {
    repository: Arc<dyn BackupRepository>,
    dump: Arc<dyn DatabaseDumpPort>,
    object_store: Arc<dyn BackupObjectStorePort>,
}

impl BackupStorePorts {
    /// 组合备份端口。
    #[must_use]
    pub fn new(
        repository: Arc<dyn BackupRepository>,
        dump: Arc<dyn DatabaseDumpPort>,
        object_store: Arc<dyn BackupObjectStorePort>,
    ) -> Self {
        Self {
            repository,
            dump,
            object_store,
        }
    }

    /// 备份任务/配置仓储。
    #[must_use]
    pub fn repository(&self) -> Arc<dyn BackupRepository> {
        self.repository.clone()
    }

    /// `pg_dump` 导出与暂存。
    #[must_use]
    pub fn dump(&self) -> Arc<dyn DatabaseDumpPort> {
        self.dump.clone()
    }

    /// S3/R2 对象存储适配器。
    #[must_use]
    pub fn object_store(&self) -> Arc<dyn BackupObjectStorePort> {
        self.object_store.clone()
    }

    /// 测试与不可用路径使用的空实现；任何调用都返回不可用错误。
    #[doc(hidden)]
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            repository: Arc::new(UnavailableBackupRepository),
            dump: Arc::new(UnavailableDumpPort),
            object_store: Arc::new(UnavailableObjectStore),
        }
    }
}

struct UnavailableBackupRepository;

#[async_trait]
impl BackupRepository for UnavailableBackupRepository {
    async fn load_settings(&self) -> AdminStoreResult<BackupSettings> {
        Err(disabled_store())
    }
    async fn update_storage_settings(
        &self,
        _command: UpdateBackupStorageCommand,
        _context: &MutationContext,
    ) -> AdminStoreResult<(BackupSettings, Revision)> {
        Err(disabled_store())
    }
    async fn update_schedule_settings(
        &self,
        _command: UpdateBackupScheduleCommand,
        _next_run_at: Option<DateTime<Utc>>,
        _context: &MutationContext,
    ) -> AdminStoreResult<BackupSettings> {
        Err(disabled_store())
    }
    async fn record_verification(
        &self,
        _storage_revision: u64,
        _at: DateTime<Utc>,
    ) -> AdminStoreResult<bool> {
        Err(disabled_store())
    }
    async fn insert_backup_record(
        &self,
        _seed: BackupRecordSeed,
    ) -> AdminStoreResult<BackupRecord> {
        Err(disabled_store())
    }
    async fn insert_scheduled_record(&self, _seed: BackupRecordSeed) -> AdminStoreResult<bool> {
        Err(disabled_store())
    }
    async fn list_backup_records(
        &self,
        _query: BackupRecordListQuery,
    ) -> AdminStoreResult<BackupRecordPage> {
        Err(disabled_store())
    }
    async fn load_backup_record(&self, _id: &str) -> AdminStoreResult<Option<BackupRecord>> {
        Err(disabled_store())
    }
    async fn list_intermediate_records(&self) -> AdminStoreResult<Vec<BackupRecord>> {
        Err(disabled_store())
    }
    async fn list_pending_deletions(&self, _limit: u32) -> AdminStoreResult<Vec<BackupRecord>> {
        Err(disabled_store())
    }
    async fn list_expired_records(&self, _limit: u32) -> AdminStoreResult<Vec<BackupRecord>> {
        Err(disabled_store())
    }
    async fn claim_next_queued(
        &self,
        _now: DateTime<Utc>,
    ) -> AdminStoreResult<Option<BackupRecord>> {
        Err(disabled_store())
    }
    async fn transition_status(
        &self,
        _id: &str,
        _from: BackupStatus,
        _to: BackupStatus,
        _update: StatusTransitionUpdate,
        _now: DateTime<Utc>,
    ) -> AdminStoreResult<Option<BackupRecord>> {
        Err(disabled_store())
    }
    async fn transition_to_deleting(
        &self,
        _id: &str,
        _now: DateTime<Utc>,
    ) -> AdminStoreResult<Option<BackupRecord>> {
        Err(disabled_store())
    }
    async fn delete_record(&self, _id: &str) -> AdminStoreResult<()> {
        Err(disabled_store())
    }
    async fn advance_schedule_cursor(
        &self,
        _next_run_at: DateTime<Utc>,
        _expected_cron: &str,
        _expected_timezone: &str,
    ) -> AdminStoreResult<bool> {
        Err(disabled_store())
    }
    async fn list_scheduled_completed_desc(
        &self,
        _limit: u32,
    ) -> AdminStoreResult<Vec<BackupRecord>> {
        Err(disabled_store())
    }
}

struct UnavailableDumpPort;

#[async_trait]
impl DatabaseDumpPort for UnavailableDumpPort {
    async fn dump(&self, _request: DumpRequest) -> Result<DumpArtifact, BackupError> {
        Err(disabled_infra())
    }
    async fn inspect_staging(
        &self,
        _backup_id: &str,
    ) -> Result<Option<StagedArtifact>, BackupError> {
        Err(disabled_infra())
    }
    async fn cleanup_staging(&self, _backup_id: &str) -> Result<(), BackupError> {
        Err(disabled_infra())
    }
}

struct UnavailableObjectStore;

#[async_trait]
impl BackupObjectStorePort for UnavailableObjectStore {
    async fn test_connection(
        &self,
        _config: &BackupStorageConfig,
    ) -> Result<ConnectionTestResult, BackupError> {
        Err(disabled_infra())
    }
    async fn upload_file(
        &self,
        _config: &BackupStorageConfig,
        _request: UploadObjectRequest,
    ) -> Result<(), BackupError> {
        Err(disabled_infra())
    }
    async fn head_object(
        &self,
        _config: &BackupStorageConfig,
        _object_key: &str,
    ) -> Result<Option<BackupObjectMetadata>, BackupError> {
        Err(disabled_infra())
    }
    async fn delete_object(
        &self,
        _config: &BackupStorageConfig,
        _object_key: &str,
    ) -> Result<(), BackupError> {
        Err(disabled_infra())
    }
    async fn presigned_download(
        &self,
        _config: &BackupStorageConfig,
        _object_key: &str,
        _file_name: &str,
        _ttl: std::time::Duration,
    ) -> Result<String, BackupError> {
        Err(disabled_infra())
    }
}

fn disabled_store() -> crate::ports::store::AdminStoreError {
    crate::ports::store::AdminStoreError::new(
        crate::ports::store::AdminStoreErrorKind::Unavailable,
        "backup",
        "backup store is disabled in this configuration",
    )
}

fn disabled_infra() -> BackupError {
    BackupError::new(
        crate::model::backup::code::STORE_UNAVAILABLE,
        "backup infrastructure is disabled in this configuration".to_owned(),
    )
}
