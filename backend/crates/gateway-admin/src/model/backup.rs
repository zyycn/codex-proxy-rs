//! S3 备份领域模型、状态机与稳定错误。
//!
//! 领域类型只携带事实，不承载基础设施细节。Secret 使用 [`secrecy::SecretString`]，
//! 全流程不实现 `Serialize`，避免被错误地写入 wire 或日志。管理员身份等审计事实
//! 只存在于 `admin_audit_events`，本模型不重复保存。

use std::fmt;

use chrono::{DateTime, Utc};
use secrecy::SecretString;
use uuid::Uuid;

use super::PageSize;

/// 备份触发来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupTriggerKind {
    Manual,
    Scheduled,
}

impl BackupTriggerKind {
    /// 稳定持久化值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Scheduled => "scheduled",
        }
    }

    /// 解析持久化值。
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "manual" => Some(Self::Manual),
            "scheduled" => Some(Self::Scheduled),
            _ => None,
        }
    }
}

impl fmt::Display for BackupTriggerKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 备份任务状态机（六状态）。
///
/// 迁移 `backup_records_lifecycle_ck` 与字段空值语义共同定义合法持久化行；
/// [`Self::allows_transition_to`] 只约束领域内的显式状态迁移，两层都必须满足。
/// 删除成功后记录行被硬删除，因此不存在 `deleted` 终态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BackupStatus {
    Queued,
    Dumping,
    Uploading,
    Completed,
    Failed,
    Deleting,
}

impl BackupStatus {
    /// 稳定持久化值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Dumping => "dumping",
            Self::Uploading => "uploading",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Deleting => "deleting",
        }
    }

    /// 解析持久化值。
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "dumping" => Some(Self::Dumping),
            "uploading" => Some(Self::Uploading),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "deleting" => Some(Self::Deleting),
            _ => None,
        }
    }

    /// 该状态是否消耗全局活跃名额。
    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Dumping | Self::Uploading)
    }

    /// 该状态是否允许进入删除流程。
    #[must_use]
    pub const fn can_be_deleted(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }

    /// 领域内允许的显式迁移。
    #[must_use]
    pub fn allows_transition_to(self, target: BackupStatus) -> bool {
        matches!(
            (self, target),
            (Self::Queued, Self::Dumping)
                | (Self::Queued, Self::Failed)
                | (Self::Dumping, Self::Uploading)
                | (Self::Dumping, Self::Failed)
                | (Self::Uploading, Self::Completed)
                | (Self::Uploading, Self::Failed)
                | (Self::Completed, Self::Deleting)
                | (Self::Failed, Self::Deleting)
        )
    }
}

impl fmt::Display for BackupStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// S3 存储配置更新命令；`secret_access_key` 为 `None` 表示保留旧值。
#[derive(Clone)]
pub struct UpdateBackupStorageCommand {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: Option<SecretString>,
    pub prefix: String,
    pub force_path_style: bool,
}

impl fmt::Debug for UpdateBackupStorageCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UpdateBackupStorageCommand")
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &self.secret_access_key.is_some())
            .field("prefix", &self.prefix)
            .field("force_path_style", &self.force_path_style)
            .finish()
    }
}

/// 调度配置更新命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateBackupScheduleCommand {
    pub schedule_enabled: bool,
    pub cron_expression: String,
    pub schedule_timezone: String,
    pub retention_days: u32,
    pub retention_count: u32,
}

/// 完整保存的备份设置（含 Secret）。
///
/// 只从仓储读取；不实现 `Serialize`，防止 Secret 进入 wire 或日志。
#[derive(Clone)]
pub struct BackupSettings {
    pub storage_revision: u64,
    pub endpoint: Option<String>,
    pub region: Option<String>,
    pub bucket: Option<String>,
    pub access_key_id: Option<String>,
    pub secret_access_key: Option<SecretString>,
    pub prefix: Option<String>,
    pub force_path_style: bool,
    pub schedule_enabled: bool,
    pub cron_expression: Option<String>,
    pub schedule_timezone: Option<String>,
    pub retention_days: u32,
    pub retention_count: u32,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_verified_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

impl fmt::Debug for BackupSettings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BackupSettings")
            .field("storage_revision", &self.storage_revision)
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &self.secret_access_key.is_some())
            .field("prefix", &self.prefix)
            .field("force_path_style", &self.force_path_style)
            .field("schedule_enabled", &self.schedule_enabled)
            .field("cron_expression", &self.cron_expression)
            .field("schedule_timezone", &self.schedule_timezone)
            .field("retention_days", &self.retention_days)
            .field("retention_count", &self.retention_count)
            .field("next_run_at", &self.next_run_at)
            .field("last_verified_at", &self.last_verified_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

impl BackupSettings {
    /// S3 存储配置是否完整（bucket/region/密钥/前缀齐全）。
    #[must_use]
    pub fn storage_configured(&self) -> bool {
        self.endpoint.is_some()
            && self.region.is_some()
            && self.bucket.is_some()
            && self.access_key_id.is_some()
            && self.secret_access_key.is_some()
            && self.prefix.is_some()
    }

    /// 探测是否已对当前 revision 成功；`last_verified_at` 非空即表示当前配置已通过探针。
    #[must_use]
    pub fn storage_verified(&self) -> bool {
        self.last_verified_at.is_some()
    }
}

/// 已经校验并可用于对象存储操作的存储配置快照。
#[derive(Clone)]
pub struct BackupStorageConfig {
    pub storage_revision: u64,
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: SecretString,
    pub prefix: String,
    pub force_path_style: bool,
}

impl fmt::Debug for BackupStorageConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BackupStorageConfig")
            .field("storage_revision", &self.storage_revision)
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"[REDACTED]")
            .field("prefix", &self.prefix)
            .field("force_path_style", &self.force_path_style)
            .finish()
    }
}

impl BackupStorageConfig {
    /// 从完整设置推导对象存储配置；设置不完整时返回 `None`。
    #[must_use]
    pub fn from_settings(settings: &BackupSettings) -> Option<Self> {
        Some(Self {
            storage_revision: settings.storage_revision,
            endpoint: settings.endpoint.clone()?,
            region: settings.region.clone()?,
            bucket: settings.bucket.clone()?,
            access_key_id: settings.access_key_id.clone()?,
            secret_access_key: settings.secret_access_key.clone()?,
            prefix: settings.prefix.clone()?,
            force_path_style: settings.force_path_style,
        })
    }
}

/// 一条备份记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupRecord {
    pub id: String,
    pub trigger_kind: BackupTriggerKind,
    pub status: BackupStatus,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub object_key: String,
    pub size_bytes: Option<u64>,
    pub sha256: Option<String>,
    pub attempt_count: u32,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    /// 创建时确定的过期时间；手工和计划备份均可设置，到期自动清理。
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 备份记录分页查询。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupRecordListQuery {
    pub page: u32,
    pub page_size: PageSize,
    pub status: Option<BackupStatus>,
    pub trigger: Option<BackupTriggerKind>,
}

/// 备份记录分页结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupRecordPage {
    pub items: Vec<BackupRecord>,
    pub total: u64,
    pub page: u32,
    pub page_size: PageSize,
}

impl BackupRecordPage {
    /// 计算总页数；空集合视为一页。
    #[must_use]
    pub fn total_pages(&self) -> u32 {
        let size = u64::from(self.page_size.get());
        if self.total == 0 {
            1
        } else {
            self.total.div_ceil(size).min(u64::from(u32::MAX)) as u32
        }
    }
}

/// 创建备份任务的持久化数据；id 与对象 key 由领域生成。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupRecordSeed {
    pub id: String,
    pub trigger_kind: BackupTriggerKind,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub object_key: String,
    pub expires_at: Option<DateTime<Utc>>,
}

/// 连接测试结果；只携带稳定阶段与脱敏消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionTestResult {
    pub ok: bool,
    pub stage: &'static str,
    pub code: Option<&'static str>,
    pub message: String,
}

/// 对象存储探测阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionTestStage {
    PutObject,
    HeadObject,
    GetObject,
    DeleteObject,
}

impl ConnectionTestStage {
    /// 稳定 wire 值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PutObject => "putObject",
            Self::HeadObject => "headObject",
            Self::GetObject => "getObject",
            Self::DeleteObject => "deleteObject",
        }
    }
}

/// 上传到对象存储并期望远端校验的归档 metadata。
///
/// `size_bytes` 在上传时不写入 metadata，但 `HeadObject` 会返回远端大小，
/// 用于与本地归档校验一致。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupObjectMetadata {
    pub backup_id: String,
    pub sha256: String,
    pub created_at: DateTime<Utc>,
    pub size_bytes: u64,
}

impl BackupObjectMetadata {
    /// 构造上传 metadata。
    #[must_use]
    pub const fn new(
        backup_id: String,
        sha256: String,
        created_at: DateTime<Utc>,
        size_bytes: u64,
    ) -> Self {
        Self {
            backup_id,
            sha256,
            created_at,
            size_bytes,
        }
    }
}

/// 短时下载地址结果。
#[derive(Clone, PartialEq, Eq)]
pub struct DownloadUrlResult {
    pub url: String,
    pub file_name: String,
    pub expires_in: std::time::Duration,
}

impl fmt::Debug for DownloadUrlResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DownloadUrlResult")
            .field("url", &"[REDACTED]")
            .field("file_name", &self.file_name)
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

/// 备份基础设施错误：只携带稳定错误码与脱敏消息。
///
/// 该类型不构成第二套 kind 枚举；`gateway-admin` 边界统一映射为既有
/// `AdminErrorKind`。稳定 `error_code` 同时用于任务记录、日志与 UI。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct BackupError {
    code: &'static str,
    message: String,
}

impl BackupError {
    /// 构造基础设施错误。
    #[must_use]
    pub const fn new(code: &'static str, message: String) -> Self {
        Self { code, message }
    }

    /// 稳定错误码，例如 `backup.s3_auth_failed`。
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }

    /// 脱敏后的可操作消息。
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// 构造稳定错误码常量。
pub mod code {
    /// 配置或参数非法。
    pub const INVALID_CONFIG: &str = "backup.invalid_config";
    /// Cron 表达式非法。
    pub const INVALID_CRON: &str = "backup.invalid_cron";
    /// IANA 时区非法。
    pub const INVALID_TIMEZONE: &str = "backup.invalid_timezone";
    /// 记录不存在。
    pub const RECORD_NOT_FOUND: &str = "backup.record_not_found";
    /// 已存在活跃任务，拒绝创建。
    pub const ACTIVE_TASK_CONFLICT: &str = "backup.active_task_conflict";
    /// 记录状态与期望不符，无法迁移。
    pub const STATE_CONFLICT: &str = "backup.state_conflict";
    /// 已有记录时禁止切换存储目标。
    pub const STORAGE_IDENTITY_LOCKED: &str = "backup.storage_identity_locked";
    /// `pg_dump` 非零退出或失败。
    pub const PG_DUMP_FAILED: &str = "backup.pg_dump_failed";
    /// 暂存磁盘空间不足。
    pub const STAGING_SPACE_EXHAUSTED: &str = "backup.staging_space_exhausted";
    /// S3 认证失败。
    pub const S3_AUTH_FAILED: &str = "backup.s3_auth_failed";
    /// S3 权限拒绝。
    pub const S3_PERMISSION_DENIED: &str = "backup.s3_permission_denied";
    /// S3 上传失败。
    pub const S3_UPLOAD_FAILED: &str = "backup.s3_upload_failed";
    /// 远端对象校验失败。
    pub const REMOTE_VERIFICATION_FAILED: &str = "backup.remote_verification_failed";
    /// 任务被取消。
    pub const CANCELLED: &str = "backup.cancelled";
    /// 仓储或对象存储暂不可用。
    pub const STORE_UNAVAILABLE: &str = "backup.store_unavailable";
    /// 上游 S3 服务返回无效或失败响应。
    pub const UPSTREAM_FAILURE: &str = "backup.upstream_failure";
}

/// 构建对象 key：`{prefix}/YYYY/MM/DD/codex-proxy-rs_{UTC seconds}_{backup id}.dump`。
///
/// 规范化规则：prefix 不得为空、不得以前导 `/` 开头、不得包含 `..`、反斜杠或控制字符；
/// 输出统一使用 `/` 分隔且不含空段。
pub fn build_object_key(
    prefix: &str,
    backup_id: &str,
    at: DateTime<Utc>,
) -> Result<String, BackupError> {
    let prefix = normalize_prefix(prefix)?;
    Ok(format!(
        "{prefix}/{}/{:02}/{:02}/codex-proxy-rs_{}_{}.dump",
        at.format("%Y"),
        at.format("%m"),
        at.format("%d"),
        at.format("%Y%m%d_%H%M%S"),
        short_backup_id(backup_id),
    ))
}

/// 下载文件名：`codex-proxy-rs_{短 id}.dump`，与对象 key 的文件名一致。
#[must_use]
pub fn build_download_file_name(backup_id: &str) -> String {
    format!("codex-proxy-rs_{}.dump", short_backup_id(backup_id))
}

/// 截取备份 id 的可读短标识：去掉 `backup_` 前缀，最多保留 8 位 hex。
#[must_use]
fn short_backup_id(backup_id: &str) -> String {
    let hex = backup_id.strip_prefix("backup_").unwrap_or(backup_id);
    hex.get(..8).unwrap_or(hex).to_owned()
}

/// 规范化并校验对象 prefix。
///
/// 返回去掉尾部 `/` 的规范 prefix；空串、前导 `/`、`..` 段、反斜杠和控制字符均拒绝。
pub fn normalize_prefix(prefix: &str) -> Result<String, BackupError> {
    if prefix.is_empty() || prefix.starts_with('/') {
        return Err(BackupError::new(
            code::INVALID_CONFIG,
            "存储对象前缀必须非空且不能以前导 / 开头".to_owned(),
        ));
    }
    if prefix.bytes().any(|byte| byte.is_ascii_control()) || prefix.contains('\\') {
        return Err(BackupError::new(
            code::INVALID_CONFIG,
            "存储对象前缀包含非法字符".to_owned(),
        ));
    }
    if prefix.split('/').any(|segment| segment == "..") {
        return Err(BackupError::new(
            code::INVALID_CONFIG,
            "存储对象前缀不能包含 .. 路径段".to_owned(),
        ));
    }
    Ok(prefix.trim_end_matches('/').to_owned())
}

/// 生成新的备份记录 id。
#[must_use]
pub fn new_backup_id() -> String {
    format!("backup_{}", Uuid::now_v7().simple())
}

/// 从触发类型与计划时间构造可持久化 seed；id 与对象 key 一次生成保持一致。
pub fn build_backup_seed(
    trigger_kind: BackupTriggerKind,
    scheduled_at: Option<DateTime<Utc>>,
    prefix: &str,
    at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
) -> Result<BackupRecordSeed, BackupError> {
    let id = new_backup_id();
    let object_key = build_object_key(prefix, &id, at)?;
    Ok(BackupRecordSeed {
        id,
        trigger_kind,
        scheduled_at,
        object_key,
        expires_at,
    })
}
