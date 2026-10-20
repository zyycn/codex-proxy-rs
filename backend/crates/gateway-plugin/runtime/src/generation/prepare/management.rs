//! 按发布集合适配插件管理接口与命令行会话

use super::PluginRuntime;
use async_trait::async_trait;
use gateway_admin::model::AdminError;
use gateway_core::routing::extensions::ExtensionSetReference;

impl PluginRuntime {
    /// CLI 复用同一候选准备与弱索引；读取帮助不会发布 Core 快照或执行命令
    pub async fn prepare_command_line(&self) -> Result<crate::PluginCommandSession, AdminError> {
        let snapshot = self
            .store
            .load_instances()
            .await
            .map_err(|_| AdminError::unavailable("无法读取已安装插件"))?;
        let reference = self.prepare_snapshot(snapshot, None, None).await?;
        let set = self.prepared_set(&reference).await?;
        Ok(crate::PluginCommandSession::new(
            reference,
            set.commands.clone(),
            set.sessions
                .iter()
                .map(|instance| instance.session.clone())
                .collect(),
            self.store.clone(),
            self.config.rpc_limits,
        ))
    }
}
#[async_trait]
impl gateway_admin::ports::plugin_management::PluginManagement for PluginRuntime {
    async fn validate_target(
        &self,
        published: &ExtensionSetReference,
        target: &gateway_admin::model::plugins::management::PluginManagementTarget,
    ) -> Result<(), AdminError> {
        let set = self.prepared_set(published).await?;
        set.management
            .iter()
            .find(|entry| &entry.view.target == target)
            .ok_or_else(|| AdminError::conflict("插件页面版本已变化，请刷新页面"))?;

        Ok(())
    }

    async fn start_callback(
        &self,
        published: &ExtensionSetReference,
        target: &gateway_admin::model::plugins::management::PluginManagementTarget,
        command: gateway_admin::model::plugins::management::StartPluginManagementCallback,
        context: &gateway_admin::model::auth::AdminRequestContext,
    ) -> Result<gateway_admin::model::plugins::management::PluginManagementCallbackTicket, AdminError>
    {
        let store = self
            .oauth_pending
            .as_ref()
            .ok_or_else(|| AdminError::unavailable("插件登录状态端口不可用"))?;
        self.prepared_set(published)
            .await?
            .management
            .iter()
            .find(|entry| &entry.view.target == target)
            .ok_or_else(|| AdminError::conflict("插件页面版本已变化，请刷新页面"))?
            .start_callback(store.as_ref(), command, context)
            .await
    }

    async fn callback(
        &self,
        published: &ExtensionSetReference,
        target: &gateway_admin::model::plugins::management::PluginManagementTarget,
        state: &str,
        request: gateway_admin::model::plugins::management::PluginManagementRequest,
    ) -> Result<gateway_admin::model::plugins::management::PluginManagementResponse, AdminError>
    {
        let store = self
            .oauth_pending
            .as_ref()
            .ok_or_else(|| AdminError::unavailable("插件登录状态端口不可用"))?;
        self.prepared_set(published)
            .await?
            .management
            .iter()
            .find(|entry| &entry.view.target == target)
            .ok_or_else(|| AdminError::conflict("插件页面版本已变化，请刷新页面"))?
            .callback(store.as_ref(), state, request, self.config.rpc_limits)
            .await
    }

    async fn views(
        &self,
        published: &ExtensionSetReference,
    ) -> Result<Vec<gateway_admin::model::plugins::management::PluginManagementView>, AdminError>
    {
        Ok(self
            .prepared_set(published)
            .await?
            .management
            .iter()
            .map(|entry| entry.view.clone())
            .collect())
    }

    async fn resource(
        &self,
        published: &ExtensionSetReference,
        target: &gateway_admin::model::plugins::management::PluginManagementTarget,
        path: &str,
        public: bool,
    ) -> Result<gateway_admin::model::plugins::management::PluginManagementResponse, AdminError>
    {
        self.prepared_set(published)
            .await?
            .management
            .iter()
            .find(|entry| &entry.view.target == target)
            .ok_or_else(|| AdminError::conflict("插件页面版本已变化，请刷新页面"))?
            .resource(path, public)
    }

    async fn handle(
        &self,
        published: &ExtensionSetReference,
        target: &gateway_admin::model::plugins::management::PluginManagementTarget,
        request: gateway_admin::model::plugins::management::PluginManagementRequest,
    ) -> Result<gateway_admin::model::plugins::management::PluginManagementResponse, AdminError>
    {
        self.prepared_set(published)
            .await?
            .management
            .iter()
            .find(|entry| &entry.view.target == target)
            .ok_or_else(|| AdminError::conflict("插件页面版本已变化，请刷新页面"))?
            .handle(request, self.config.rpc_limits)
            .await
    }
}
