//! 有界保留期删除，不承载周期、暂停或重试策略

use std::num::NonZeroU32;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::store::AdminStoreResult;
use crate::model::retention::{RetentionPolicy, RetentionTarget};

#[async_trait]
pub trait RetentionStore: Send + Sync {
    async fn load_policy(&self) -> AdminStoreResult<RetentionPolicy>;

    /// 每次最多删除 limit 行；请求关联事件随请求事务级联删除
    async fn purge_batch(
        &self,
        target: RetentionTarget,
        now: DateTime<Utc>,
        policy: RetentionPolicy,
        limit: NonZeroU32,
    ) -> AdminStoreResult<u64>;
}
