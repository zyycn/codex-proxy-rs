//! 通用用量端口的插件管理测试夹具，非本测试调用路径禁止访问
use super::TestPluginPorts;
use gateway_admin::{model::usage_data::*, ports::usage_data::*};
#[async_trait::async_trait]
impl UsageDataStore for TestPluginPorts {
    async fn get_record(&self, _: GetUsageRecord) -> UsageDataResult<UsageRecord> {
        unreachable!("unexpected usage call")
    }
    async fn list_records(&self, _: ListUsageRecords) -> UsageDataResult<UsageRecordPage> {
        unreachable!("unexpected usage call")
    }
    async fn write_records(&self, _: StoredUsageWrite) -> UsageDataResult<UsageWriteResult> {
        unreachable!("unexpected usage call")
    }
    async fn list_aggregates(&self, _: ListUsageAggregates) -> UsageDataResult<UsageAggregatePage> {
        unreachable!("unexpected usage call")
    }
    async fn write_aggregates(
        &self,
        _: StoredAggregateWrite,
    ) -> UsageDataResult<UsageAggregateWriteResult> {
        unreachable!("unexpected usage call")
    }
}
