//! 插件宿主访问账号权威所需的窄端口。

use async_trait::async_trait;
use gateway_core::account::ProviderAccountId;

use crate::model::{
    AdminError, MutationContext,
    provider_credentials::{
        PluginAccountCredential, PluginAccountListQuery, PluginAccountPage,
        PluginAccountSaveResult, PreparedPluginAccountSave,
    },
};

/// Runtime 只取得账号读写用例，不接触 Store、SQL 或完整管理服务集合。
#[async_trait]
pub trait PluginAccountAccess: Send + Sync {
    async fn list(&self, query: PluginAccountListQuery) -> Result<PluginAccountPage, AdminError>;

    async fn get_runtime(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<crate::model::accounts::AccountRecord, AdminError>;

    async fn get_credential(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<PluginAccountCredential, AdminError>;

    /// 只读 Provider 已有额度观测；实现不得主动刷新上游或附加用量预测。
    async fn get_quota(
        &self,
        _account_id: &ProviderAccountId,
    ) -> Result<crate::model::provider_credentials::ProviderQuota, AdminError> {
        Err(AdminError::unavailable("插件额度事实查询暂不可用"))
    }

    async fn save(
        &self,
        command: PreparedPluginAccountSave,
        context: &MutationContext,
    ) -> Result<PluginAccountSaveResult, AdminError>;
}
