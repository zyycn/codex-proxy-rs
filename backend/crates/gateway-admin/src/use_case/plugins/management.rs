//! 插件管理请求的目标解析、身份授权与运行时分派

use std::sync::Arc;

use gateway_core::runtime::{RuntimeSnapshotHandle, extensions::ExtensionSetReference};

use crate::{
    model::{
        AdminError,
        plugins::{
            instances::PluginInstance,
            management::{
                PluginManagementRequest, PluginManagementResponse, PluginManagementTarget,
                PluginManagementView,
            },
        },
    },
    ports::{plugin_management::PluginManagement, plugins::PluginStore},
};

/// 菜单来自发布视图；每次业务动作复核持久实例，旧页面不能继续使用已停用或换版的目标
pub struct PluginManagementService {
    runtime: Arc<dyn PluginManagement>,
    store: Arc<dyn PluginStore>,
    published: RuntimeSnapshotHandle,
}

impl PluginManagementService {
    /// 页面发起模型请求前复核持久目标与当前发布代次；长响应可重复调用以收敛撤销
    pub async fn validate_target(&self, target: &PluginManagementTarget) -> Result<(), AdminError> {
        let reference = self.authorize(target).await?;
        self.runtime.validate_target(&reference, target).await
    }

    pub async fn start_callback(
        &self,
        target: &PluginManagementTarget,
        command: crate::model::plugins::management::StartPluginManagementCallback,
        context: &crate::model::auth::AdminRequestContext,
    ) -> Result<crate::model::plugins::management::PluginManagementCallbackTicket, AdminError> {
        let reference = self.authorize(target).await?;
        self.runtime
            .start_callback(&reference, target, command, context)
            .await
    }

    pub async fn callback(
        &self,
        target: &PluginManagementTarget,
        state: &str,
        request: PluginManagementRequest,
    ) -> Result<PluginManagementResponse, AdminError> {
        let reference = self.authorize(target).await?;
        self.runtime
            .callback(&reference, target, state, request)
            .await
    }

    #[must_use]
    pub fn new(
        runtime: Arc<dyn PluginManagement>,
        store: Arc<dyn PluginStore>,
        published: RuntimeSnapshotHandle,
    ) -> Self {
        Self {
            runtime,
            store,
            published,
        }
    }

    pub async fn views(&self) -> Result<Vec<PluginManagementView>, AdminError> {
        let Some(reference) = self.reference()? else {
            return Ok(Vec::new());
        };
        let mut views = self.runtime.views(&reference).await?;
        let snapshot = self
            .store
            .load_instances()
            .await
            .map_err(|_| AdminError::unavailable("无法复核插件页面授权"))?;
        views.retain(|view| {
            snapshot
                .instances
                .iter()
                .any(|instance| matches(instance, &view.target))
        });
        Ok(views)
    }

    pub async fn resource(
        &self,
        target: &PluginManagementTarget,
        path: &str,
        public: bool,
    ) -> Result<PluginManagementResponse, AdminError> {
        let reference = self.authorize(target).await?;
        self.runtime
            .resource(&reference, target, path, public)
            .await
    }

    pub async fn handle(
        &self,
        target: &PluginManagementTarget,
        request: PluginManagementRequest,
    ) -> Result<PluginManagementResponse, AdminError> {
        let reference = self.authorize(target).await?;
        self.runtime.handle(&reference, target, request).await
    }

    fn reference(&self) -> Result<Option<ExtensionSetReference>, AdminError> {
        self.published
            .acquire()
            .map(|snapshot| snapshot.extensions().cloned())
            .map_err(|_| AdminError::unavailable("插件发布视图暂不可用"))
    }

    async fn authorize(
        &self,
        target: &PluginManagementTarget,
    ) -> Result<ExtensionSetReference, AdminError> {
        let reference = self
            .reference()?
            .ok_or_else(|| AdminError::not_found("插件管理扩展不存在"))?;
        let current = self
            .store
            .management_target_is_current(target)
            .await
            .map_err(|_| AdminError::unavailable("无法复核插件管理授权"))?;
        if !current {
            return Err(AdminError::conflict("插件版本或授权已变化，请刷新页面"));
        }
        Ok(reference)
    }
}

fn matches(instance: &PluginInstance, target: &PluginManagementTarget) -> bool {
    instance.enabled
        && instance.trusted_process
        && instance.id == target.instance_id
        && instance.artifact_sha256 == target.artifact_sha256
        && instance.revision.get() == target.revision
}
