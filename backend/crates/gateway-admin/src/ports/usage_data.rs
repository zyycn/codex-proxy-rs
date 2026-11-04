//! 原生请求用量的有界读取和事务写入端口

use crate::model::{auth::AdminAuditEvent, usage_data::*};
use async_trait::async_trait;
use gateway_core::lifecycle::CancellationToken;

#[async_trait]
pub trait UsageReadSession: Send {
    async fn next_batch(&mut self) -> UsageDataResult<UsageReadBatch>;
    fn is_finished(&self) -> bool;
    async fn close(&mut self);
}

pub struct StoredUsageWrite {
    pub changes: Vec<(UsageRecordChange, AdminAuditEvent)>,
}

pub struct StoredAggregateWrite {
    pub changes: Vec<(UsageAggregateChange, AdminAuditEvent)>,
}

#[async_trait]
pub trait UsageDataStore: Send + Sync {
    async fn open_read(
        &self,
        query: UsageReadQuery,
        cancellation: CancellationToken,
    ) -> UsageDataResult<Box<dyn UsageReadSession>>;
    async fn list_aggregates(
        &self,
        request: ListUsageAggregates,
    ) -> UsageDataResult<UsageAggregatePage>;
    async fn write_aggregates(
        &self,
        command: StoredAggregateWrite,
    ) -> UsageDataResult<UsageAggregateWriteResult>;
    async fn get_record(&self, request: GetUsageRecord) -> UsageDataResult<UsageRecord>;
    async fn list_records(&self, request: ListUsageRecords) -> UsageDataResult<UsageRecordPage>;
    async fn write_records(&self, command: StoredUsageWrite) -> UsageDataResult<UsageWriteResult>;
}
