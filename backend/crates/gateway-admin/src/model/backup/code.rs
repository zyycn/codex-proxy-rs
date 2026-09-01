//! 备份领域的稳定错误码。

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
