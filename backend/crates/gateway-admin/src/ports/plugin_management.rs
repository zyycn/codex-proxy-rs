//! 插件管理页面、资源访问与授权回调的运行时端口

use async_trait::async_trait;
use gateway_core::runtime::extensions::ExtensionSetReference;

use crate::model::{
    AdminError,
    plugins::management::{
        PluginManagementRequest, PluginManagementResponse, PluginManagementTarget,
        PluginManagementView,
    },
};

/// 仅按 Admin 固定的发布引用分派；Runtime 不自行选择当前代次，也不拥有管理员身份
#[async_trait]
pub trait PluginManagement: Send + Sync {
    /// 复核固定发布代次中的可用目标；调用方已校验持久实例仍为当前版本
    async fn validate_target(
        &self,
        published: &ExtensionSetReference,
        target: &PluginManagementTarget,
    ) -> Result<(), AdminError>;

    async fn start_callback(
        &self,
        published: &ExtensionSetReference,
        target: &PluginManagementTarget,
        command: crate::model::plugins::management::StartPluginManagementCallback,
        context: &crate::model::auth::AdminRequestContext,
    ) -> Result<crate::model::plugins::management::PluginManagementCallbackTicket, AdminError>;

    async fn callback(
        &self,
        published: &ExtensionSetReference,
        target: &PluginManagementTarget,
        state: &str,
        request: PluginManagementRequest,
    ) -> Result<PluginManagementResponse, AdminError>;

    async fn views(
        &self,
        published: &ExtensionSetReference,
    ) -> Result<Vec<PluginManagementView>, AdminError>;

    async fn resource(
        &self,
        published: &ExtensionSetReference,
        target: &PluginManagementTarget,
        path: &str,
        public: bool,
    ) -> Result<PluginManagementResponse, AdminError>;

    async fn handle(
        &self,
        published: &ExtensionSetReference,
        target: &PluginManagementTarget,
        request: PluginManagementRequest,
    ) -> Result<PluginManagementResponse, AdminError>;
}
