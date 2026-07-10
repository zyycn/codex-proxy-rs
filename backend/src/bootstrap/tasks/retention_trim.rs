//! PostgreSQL 增长表保留期清理任务。

use std::num::NonZeroU32;

use chrono::Utc;
use tracing::{info, warn};

use crate::{
    telemetry::{
        account_usage::store::PgAccountUsageStore, buckets::store::PgRequestBucketStore,
        ops::store::PgOpsErrorLogStore, usage::store::PgUsageRecordStore,
    },
    upstream::openai::fingerprint::PgFingerprintStore,
};

use super::{
    coordinator::SchedulerHandle,
    periodic::{spawn_periodic_task, PeriodicTaskConfig, PeriodicTaskRunner, TaskFuture},
};

const RETENTION_TRIM_INTERVAL_SECS: u64 = 60 * 60;
const ACCOUNT_MODEL_USAGE_LIMIT: NonZeroU32 = NonZeroU32::new(100).unwrap();
const FINGERPRINT_HISTORY_LIMIT: NonZeroU32 = NonZeroU32::new(100).unwrap();

pub struct RetentionTrimTask {
    usage_records: PgUsageRecordStore,
    ops_errors: PgOpsErrorLogStore,
    buckets: PgRequestBucketStore,
    account_usage: PgAccountUsageStore,
    fingerprints: PgFingerprintStore,
}

impl RetentionTrimTask {
    pub fn new(
        usage_records: PgUsageRecordStore,
        ops_errors: PgOpsErrorLogStore,
        buckets: PgRequestBucketStore,
        account_usage: PgAccountUsageStore,
        fingerprints: PgFingerprintStore,
    ) -> Self {
        Self {
            usage_records,
            ops_errors,
            buckets,
            account_usage,
            fingerprints,
        }
    }

    pub fn start(self) -> SchedulerHandle {
        spawn_periodic_task(
            self,
            PeriodicTaskConfig::new(
                RETENTION_TRIM_INTERVAL_SECS,
                "PostgreSQL 保留期清理任务已启动",
                "PostgreSQL 保留期清理任务已关闭",
            ),
        )
    }
}

impl PeriodicTaskRunner for RetentionTrimTask {
    fn tick(&mut self) -> TaskFuture<'_, ()> {
        Box::pin(async move {
            let now = Utc::now();
            match self.usage_records.trim_to_retention(now).await {
                Ok(deleted) if deleted > 0 => info!(deleted, "已清理过期成功使用事实"),
                Ok(_) => {}
                Err(error) => warn!(error = %error, "清理成功使用事实失败"),
            }
            match self.ops_errors.trim_to_retention(now).await {
                Ok(deleted) if deleted > 0 => info!(deleted, "已清理过期运维错误事实"),
                Ok(_) => {}
                Err(error) => warn!(error = %error, "清理运维错误事实失败"),
            }
            match self.buckets.trim_to_retention(now).await {
                Ok(deleted) if deleted > 0 => info!(deleted, "已清理过期请求时间桶"),
                Ok(_) => {}
                Err(error) => warn!(error = %error, "清理请求时间桶失败"),
            }
            match self
                .account_usage
                .trim_model_usage_to_limit(ACCOUNT_MODEL_USAGE_LIMIT)
                .await
            {
                Ok(deleted) if deleted > 0 => {
                    info!(deleted, "已清理超限账号模型用量统计")
                }
                Ok(_) => {}
                Err(error) => warn!(error = %error, "清理超限账号模型用量统计失败"),
            }
            match self
                .fingerprints
                .trim_update_history_to_limit(FINGERPRINT_HISTORY_LIMIT)
                .await
            {
                Ok(deleted) if deleted > 0 => {
                    info!(deleted, "已清理超限指纹更新历史")
                }
                Ok(_) => {}
                Err(error) => warn!(error = %error, "清理超限指纹更新历史失败"),
            }
        })
    }
}
