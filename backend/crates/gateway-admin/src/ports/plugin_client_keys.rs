//! 插件 Client Key 非秘密目录与受控预算管理端口

use async_trait::async_trait;
use gateway_core::{engine::budget::ClientBudgetStatus, policy::ClientApiKeyId};

use crate::model::{
    AdminError, MutationContext,
    client_keys::{
        ClientKeyBudgetWindow, ClientKeyBudgetWindowPeriod, ResetClientKeyBudget,
        SetClientKeyBudgetWindow, UpdateClientKeyBudgetLimits,
    },
    plugin_client_keys::{PluginClientKeyFacts, PluginClientKeyListQuery, PluginClientKeyPage},
    plugin_resources::PluginResourceOwner,
};

#[async_trait]
pub trait PluginClientKeyAccess: Send + Sync {
    async fn budget_window(
        &self,
        id: &ClientApiKeyId,
        period: ClientKeyBudgetWindowPeriod,
    ) -> Result<ClientKeyBudgetWindow, AdminError>;
    async fn set_budget_window(
        &self,
        owner: &PluginResourceOwner,
        command: SetClientKeyBudgetWindow,
        context: &MutationContext,
    ) -> Result<u64, AdminError>;

    async fn facts(&self, id: &ClientApiKeyId) -> Result<PluginClientKeyFacts, AdminError>;

    async fn budget(&self, id: &ClientApiKeyId) -> Result<ClientBudgetStatus, AdminError>;

    async fn update_budget_limits(
        &self,
        owner: &PluginResourceOwner,
        command: UpdateClientKeyBudgetLimits,
        context: &MutationContext,
    ) -> Result<ClientApiKeyId, AdminError>;

    async fn reset_budget(
        &self,
        owner: &PluginResourceOwner,
        command: ResetClientKeyBudget,
        context: &MutationContext,
    ) -> Result<ClientApiKeyId, AdminError>;

    async fn list(
        &self,
        query: PluginClientKeyListQuery,
    ) -> Result<PluginClientKeyPage, AdminError>;
}
