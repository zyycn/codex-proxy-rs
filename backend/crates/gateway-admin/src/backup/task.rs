//! Backup Worker 贡献：单个可取消 `DaemonTask` 承担调度、执行、删除收敛与保留清理。
//!
//! 当前部署边界是单副本，因此不需要 Redis lease、fencing token 或 heartbeat。长时间
//! `pg_dump` 与上传由本 Daemon 自身持有；Host 只负责 panic 后重启、健康与关闭。

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use gateway_core::{
    lifecycle::CancellationToken,
    task::{DaemonTask, WorkerTaskError},
};
use tracing::{info, warn};

use crate::{
    model::backup::{
        BackupError, BackupObjectMetadata, BackupRecord, BackupStatus, BackupStorageConfig,
        BackupTriggerKind, build_backup_seed, code,
    },
    ports::backup::{
        BackupObjectStorePort, BackupRepository, DatabaseDumpPort, DumpRequest,
        StatusTransitionUpdate, UploadObjectRequest,
    },
};

impl From<BackupError> for WorkerTaskError {
    fn from(error: BackupError) -> Self {
        WorkerTaskError::safe(error.message())
    }
}

use super::policy::{BackupSchedule, decide_retention};

/// 循环间隔：管理员修改 Cron 后该时长内生效；无工作时可取消等待。
const BACKUP_LOOP_INTERVAL: Duration = Duration::from_secs(30);
/// 每个循环最多执行的保留删除数量。
const RETENTION_BATCH_SIZE: usize = 10;
/// 保留扫描一次读取的 completed 计划备份上限。
const RETENTION_SCAN_LIMIT: u32 = 1000;
/// 每个循环最多完成的待删除记录数量。
const PENDING_DELETION_BATCH: u32 = 20;

/// 备份 Daemon 任务。
pub struct BackupTask {
    repository: Arc<dyn BackupRepository>,
    dump: Arc<dyn DatabaseDumpPort>,
    object_store: Arc<dyn BackupObjectStorePort>,
}

impl BackupTask {
    /// 组合仓储、导出器与对象存储适配器。
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
}

impl DaemonTask for BackupTask {
    fn run(&self, cancellation: CancellationToken) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move { self.run_daemon(cancellation).await })
    }
}

impl BackupTask {
    async fn run_daemon(&self, cancellation: CancellationToken) -> Result<(), WorkerTaskError> {
        loop {
            if cancellation.is_cancelled() {
                return Ok(());
            }
            self.run_cycle(&cancellation).await?;

            tokio::select! {
                _ = cancellation.cancelled() => return Ok(()),
                _ = tokio::time::sleep(BACKUP_LOOP_INTERVAL) => {}
            }
        }
    }

    /// 执行一个周期：推进计划、恢复中间态、删除收敛、领取执行一个任务、保留清理。
    ///
    /// 对外暴露以便集成测试逐周期驱动；daemon 循环内部复用。
    ///
    /// # Errors
    ///
    /// 仓储或基础设施不可用、任务执行失败时返回 [`WorkerTaskError`]。
    pub async fn run_cycle(&self, cancellation: &CancellationToken) -> Result<(), WorkerTaskError> {
        if cancellation.is_cancelled() {
            return Ok(());
        }
        self.advance_schedule(Utc::now()).await?;
        self.recover_intermediate(cancellation).await?;
        self.finish_pending_deletions().await?;
        self.process_next_task(cancellation).await?;
        self.run_retention_batch(Utc::now()).await?;
        Ok(())
    }

    /// 推进到期计划并幂等插入 scheduled 任务。
    async fn advance_schedule(&self, now: DateTime<Utc>) -> Result<(), WorkerTaskError> {
        let settings = self.repository.load_settings().await.map_err(repo_error)?;
        if !settings.schedule_enabled {
            return Ok(());
        }
        let cron = settings.cron_expression.as_deref().unwrap_or_default();
        let timezone = settings.schedule_timezone.as_deref().unwrap_or_default();
        if cron.is_empty() || timezone.is_empty() {
            return Ok(());
        }
        let schedule = BackupSchedule::parse(cron, timezone)
            .map_err(|_| WorkerTaskError::safe("backup schedule is invalid"))?;

        let due = match settings.next_run_at {
            Some(next_run_at) if next_run_at <= now => true,
            Some(_) => false,
            None => {
                // 刚启用计划但尚无游标：直接初始化。
                if let Some(next_run_at) = schedule.next_after(now) {
                    let _ = self
                        .repository
                        .advance_schedule_cursor(next_run_at, cron, timezone)
                        .await
                        .map_err(repo_error)?;
                }
                return Ok(());
            }
        };
        if !due {
            return Ok(());
        }

        let scheduled_at = match schedule.last_firing_at_or_before(now) {
            Some(scheduled_at) => scheduled_at,
            None => {
                // 无法从计划推导最近到期时间：推进游标避免反复触发。
                if let Some(next_run_at) = schedule.next_after(now) {
                    let _ = self
                        .repository
                        .advance_schedule_cursor(next_run_at, cron, timezone)
                        .await
                        .map_err(repo_error)?;
                }
                return Ok(());
            }
        };
        let prefix = settings.prefix.as_deref().unwrap_or_default();
        let expires_at = if settings.retention_days > 0 {
            Some(now + chrono::Duration::days(i64::from(settings.retention_days)))
        } else {
            None
        };
        let seed = build_backup_seed(
            BackupTriggerKind::Scheduled,
            Some(scheduled_at),
            prefix,
            now,
            expires_at,
        )
        .map_err(infra_error)?;
        let inserted = self
            .repository
            .insert_scheduled_record(seed)
            .await
            .map_err(repo_error)?;
        if inserted {
            info!(scheduled_at = %scheduled_at, "计划备份任务已创建");
        } else {
            warn!(scheduled_at = %scheduled_at, "计划时间点冲突，跳过并推进游标");
        }
        if let Some(next_run_at) = schedule.next_after(now) {
            let _ = self
                .repository
                .advance_schedule_cursor(next_run_at, cron, timezone)
                .await
                .map_err(repo_error)?;
        }
        Ok(())
    }

    /// 恢复中间状态（dumping/uploading/deleting）。
    async fn recover_intermediate(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<(), WorkerTaskError> {
        let records = self
            .repository
            .list_intermediate_records()
            .await
            .map_err(repo_error)?;
        for record in records {
            match record.status {
                BackupStatus::Dumping => self.recover_dumping(&record, cancellation).await?,
                BackupStatus::Uploading => self.recover_uploading(&record, cancellation).await?,
                _ => {}
            }
        }
        Ok(())
    }

    /// dumping：有完整暂存则继续上传，否则清理暂存并标记失败。
    async fn recover_dumping(
        &self,
        record: &BackupRecord,
        cancellation: &CancellationToken,
    ) -> Result<(), WorkerTaskError> {
        match self.dump.inspect_staging(&record.id).await? {
            Some(artifact) => {
                let now = Utc::now();
                let update = StatusTransitionUpdate {
                    size_bytes: Some(artifact.size_bytes),
                    sha256: Some(artifact.sha256.clone()),
                    ..Default::default()
                };
                let Some(uploading_record) = self
                    .repository
                    .transition_status(
                        &record.id,
                        BackupStatus::Dumping,
                        BackupStatus::Uploading,
                        update,
                        now,
                    )
                    .await
                    .map_err(repo_error)?
                else {
                    return Ok(());
                };
                self.upload_and_verify(
                    &uploading_record,
                    &artifact.path,
                    artifact.size_bytes,
                    &artifact.sha256,
                    now,
                    cancellation,
                )
                .await?;
            }
            None => {
                let _ = self.dump.cleanup_staging(&record.id).await;
                self.fail_task(record, code::PG_DUMP_FAILED, "导出进程中断，暂存归档不完整")
                    .await?;
            }
        }
        Ok(())
    }

    /// uploading：远端匹配则补记完成，有完整暂存则重试上传，否则失败。
    async fn recover_uploading(
        &self,
        record: &BackupRecord,
        cancellation: &CancellationToken,
    ) -> Result<(), WorkerTaskError> {
        let settings = self.repository.load_settings().await.map_err(repo_error)?;
        let Some(config) = BackupStorageConfig::from_settings(&settings) else {
            return Ok(());
        };
        if let Some(remote) = self
            .object_store
            .head_object(&config, &record.object_key)
            .await?
            && record.size_bytes == Some(remote.size_bytes)
            && record.sha256.as_deref() == Some(remote.sha256.as_str())
        {
            let now = Utc::now();
            self.repository
                .transition_status(
                    &record.id,
                    BackupStatus::Uploading,
                    BackupStatus::Completed,
                    StatusTransitionUpdate::default(),
                    now,
                )
                .await
                .map_err(repo_error)?;
            return Ok(());
        }
        match self.dump.inspect_staging(&record.id).await? {
            Some(artifact) => {
                self.upload_and_verify(
                    record,
                    &artifact.path,
                    artifact.size_bytes,
                    &artifact.sha256,
                    Utc::now(),
                    cancellation,
                )
                .await
            }
            None => {
                self.fail_task(
                    record,
                    code::S3_UPLOAD_FAILED,
                    "远端对象缺失且本地无完整暂存",
                )
                .await
            }
        }
    }

    /// 完成待删除记录（deleting → DeleteObject → 硬删除）。
    async fn finish_pending_deletions(&self) -> Result<(), WorkerTaskError> {
        let settings = self.repository.load_settings().await.map_err(repo_error)?;
        let Some(config) = BackupStorageConfig::from_settings(&settings) else {
            return Ok(());
        };
        let records = self
            .repository
            .list_pending_deletions(PENDING_DELETION_BATCH)
            .await
            .map_err(repo_error)?;
        for record in records {
            if self
                .object_store
                .delete_object(&config, &record.object_key)
                .await
                .is_ok()
            {
                self.repository
                    .delete_record(&record.id)
                    .await
                    .map_err(repo_error)?;
            }
        }
        Ok(())
    }

    /// 领取一个 queued 任务并执行导出、上传与远端校验。
    async fn process_next_task(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<(), WorkerTaskError> {
        let now = Utc::now();
        let Some(record) = self
            .repository
            .claim_next_queued(now)
            .await
            .map_err(repo_error)?
        else {
            return Ok(());
        };
        info!(
            backup_id = %record.id,
            attempt = record.attempt_count,
            "领取备份任务"
        );
        if let Err(error) = self.execute_task(&record, cancellation).await {
            warn!(backup_id = %record.id, error = %error, "备份任务执行失败");
        }
        Ok(())
    }

    /// 执行单个任务；内部处理全部状态迁移。
    async fn execute_task(
        &self,
        record: &BackupRecord,
        cancellation: &CancellationToken,
    ) -> Result<(), WorkerTaskError> {
        let settings = self.repository.load_settings().await.map_err(repo_error)?;
        if BackupStorageConfig::from_settings(&settings).is_none() {
            return self
                .fail_task(record, code::INVALID_CONFIG, "备份存储配置不完整")
                .await;
        }
        if !settings.storage_verified() {
            return self
                .fail_task(record, code::INVALID_CONFIG, "备份存储配置尚未通过连接测试")
                .await;
        }

        let now = Utc::now();
        let artifact = match self
            .dump
            .dump(DumpRequest {
                backup_id: record.id.clone(),
                cancellation: cancellation.clone(),
            })
            .await
        {
            Ok(artifact) => artifact,
            Err(error) => {
                let (error_code, message) = classify_dump_error(error);
                return self.fail_task(record, error_code, message).await;
            }
        };

        // dumping → uploading，持久化 size/sha256。
        let update = StatusTransitionUpdate {
            size_bytes: Some(artifact.size_bytes),
            sha256: Some(artifact.sha256.clone()),
            ..Default::default()
        };
        let Some(uploading_record) = self
            .repository
            .transition_status(
                &record.id,
                BackupStatus::Dumping,
                BackupStatus::Uploading,
                update,
                now,
            )
            .await
            .map_err(repo_error)?
        else {
            return Ok(());
        };

        self.upload_and_verify(
            &uploading_record,
            &artifact.path,
            artifact.size_bytes,
            &artifact.sha256,
            now,
            cancellation,
        )
        .await?;
        let _ = self.dump.cleanup_staging(&record.id).await;
        Ok(())
    }

    /// 上传 + 远端校验，内部完成终态迁移。
    async fn upload_and_verify(
        &self,
        record: &BackupRecord,
        source: &std::path::Path,
        size_bytes: u64,
        sha256: &str,
        now: DateTime<Utc>,
        cancellation: &CancellationToken,
    ) -> Result<(), WorkerTaskError> {
        let settings = self.repository.load_settings().await.map_err(repo_error)?;
        let Some(config) = BackupStorageConfig::from_settings(&settings) else {
            return Ok(());
        };
        let store = self.object_store.clone();
        let metadata =
            BackupObjectMetadata::new(record.id.clone(), sha256.to_owned(), now, size_bytes);
        if let Err(error) = store
            .upload_file(
                &config,
                UploadObjectRequest {
                    object_key: record.object_key.clone(),
                    source: source.to_path_buf(),
                    metadata,
                    cancellation: cancellation.clone(),
                },
            )
            .await
        {
            let _ = store.delete_object(&config, &record.object_key).await;
            self.fail_task(
                record,
                error.code(),
                format!("上传到对象存储失败：{}", error.message()),
            )
            .await?;
            return Ok(());
        }

        match store.head_object(&config, &record.object_key).await {
            Ok(Some(remote)) if remote.size_bytes == size_bytes && remote.sha256 == sha256 => {
                if self
                    .repository
                    .transition_status(
                        &record.id,
                        BackupStatus::Uploading,
                        BackupStatus::Completed,
                        StatusTransitionUpdate::default(),
                        Utc::now(),
                    )
                    .await
                    .map_err(repo_error)?
                    .is_some()
                {
                    info!(
                        backup_id = %record.id,
                        size_bytes,
                        "备份完成"
                    );
                }
            }
            Ok(Some(remote)) => {
                let size_matches = remote.size_bytes == size_bytes;
                let sha256_matches = remote.sha256 == sha256;
                warn!(
                    backup_id = %record.id,
                    expected_size_bytes = size_bytes,
                    remote_size_bytes = remote.size_bytes,
                    sha256_matches,
                    "备份远端对象校验不一致"
                );
                let _ = store.delete_object(&config, &record.object_key).await;
                self.fail_task(
                    record,
                    code::REMOTE_VERIFICATION_FAILED,
                    match (size_matches, sha256_matches) {
                        (false, true) => "远端对象大小不一致",
                        (true, false) => "远端对象 sha256 校验值不一致",
                        (false, false) => "远端对象大小和 sha256 校验值均不一致",
                        (true, true) => "远端对象校验状态异常",
                    },
                )
                .await?;
            }
            Ok(None) => {
                warn!(backup_id = %record.id, "上传完成后未找到远端对象");
                self.fail_task(
                    record,
                    code::REMOTE_VERIFICATION_FAILED,
                    "上传完成后未找到远端对象",
                )
                .await?;
            }
            Err(error) => {
                let _ = store.delete_object(&config, &record.object_key).await;
                self.fail_task(
                    record,
                    error.code(),
                    format!("远端对象校验失败：{}", error.message()),
                )
                .await?;
            }
        }
        Ok(())
    }

    /// 把活跃任务写入失败终态并清理暂存。
    async fn fail_task(
        &self,
        record: &BackupRecord,
        error_code: &'static str,
        message: impl Into<String>,
    ) -> Result<(), WorkerTaskError> {
        let message = message.into();
        warn!(
            backup_id = %record.id,
            error_code,
            error_message = %message,
            "备份任务失败"
        );
        let now = Utc::now();
        let update = StatusTransitionUpdate {
            error_code: Some(error_code.to_string()),
            error_message: Some(message),
            completed_at: Some(now),
            ..Default::default()
        };
        let _ = self
            .repository
            .transition_status(&record.id, record.status, BackupStatus::Failed, update, now)
            .await
            .map_err(repo_error)?;
        let _ = self.dump.cleanup_staging(&record.id).await;
        Ok(())
    }

    /// 执行一小批到期保留清理：先处理 `expires_at` 已到期的记录，再按天数/份数清理计划备份。
    async fn run_retention_batch(&self, now: DateTime<Utc>) -> Result<(), WorkerTaskError> {
        let settings = self.repository.load_settings().await.map_err(repo_error)?;

        // 1. expires_at 已到期的记录（手动或计划）无条件进入删除。
        let expired = self
            .repository
            .list_expired_records(RETENTION_BATCH_SIZE as u32)
            .await
            .map_err(repo_error)?;
        for record in expired {
            let Some(record) = self
                .repository
                .transition_to_deleting(&record.id, now)
                .await
                .map_err(repo_error)?
            else {
                continue;
            };
            self.finalize_deletion(&record).await?;
        }

        // 2. 计划备份按 retentionDays / retentionCount 清理。
        let records = self
            .repository
            .list_scheduled_completed_desc(RETENTION_SCAN_LIMIT)
            .await
            .map_err(repo_error)?;
        let decisions = decide_retention(
            settings.retention_days,
            settings.retention_count,
            now,
            &records,
        );
        for decision in decisions.into_iter().take(RETENTION_BATCH_SIZE) {
            let Some(record) = self
                .repository
                .transition_to_deleting(&decision.record_id, now)
                .await
                .map_err(repo_error)?
            else {
                continue;
            };
            self.finalize_deletion(&record).await?;
        }
        Ok(())
    }

    /// 对已进入 deleting 的记录执行对象删除并硬删除记录行。
    async fn finalize_deletion(&self, record: &BackupRecord) -> Result<(), WorkerTaskError> {
        let settings = self.repository.load_settings().await.map_err(repo_error)?;
        let Some(config) = BackupStorageConfig::from_settings(&settings) else {
            return Ok(());
        };
        if self
            .object_store
            .delete_object(&config, &record.object_key)
            .await
            .is_ok()
        {
            self.repository
                .delete_record(&record.id)
                .await
                .map_err(repo_error)?;
        }
        Ok(())
    }
}

fn classify_dump_error(error: BackupError) -> (&'static str, &'static str) {
    match error.code() {
        code::CANCELLED => (code::CANCELLED, "备份已取消"),
        code::STAGING_SPACE_EXHAUSTED => (code::STAGING_SPACE_EXHAUSTED, "暂存磁盘空间不足"),
        _ => (code::PG_DUMP_FAILED, "数据库导出失败"),
    }
}

fn repo_error(error: crate::ports::store::AdminStoreError) -> WorkerTaskError {
    WorkerTaskError::safe(format!("backup store failed: {error}"))
}

fn infra_error(error: BackupError) -> WorkerTaskError {
    WorkerTaskError::safe(error.message())
}
