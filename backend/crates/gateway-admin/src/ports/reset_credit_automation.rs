//! 自动重置卡计划的持久化端口，配置变更沿用管理事务与审计

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::store::AdminStoreResult;
use crate::model::{MutationContext, reset_credit_automation::ResetCreditAutomation};

#[async_trait]
pub trait ResetCreditAutomationStore: Send + Sync {
    async fn reset_credit_automation(
        &self,
        account_id: &str,
    ) -> AdminStoreResult<Option<ResetCreditAutomation>>;

    /// 只读取到期的本地计划，按到期时间稳定排序
    async fn due_reset_credit_automations(
        &self,
        now: DateTime<Utc>,
        limit: u32,
    ) -> AdminStoreResult<Vec<String>>;

    /// 按 record.revision 比较写入，成功返回新版本；配置变更传入审计上下文
    async fn save_reset_credit_automation(
        &self,
        record: &ResetCreditAutomation,
        context: Option<&MutationContext>,
    ) -> AdminStoreResult<u64>;
}
