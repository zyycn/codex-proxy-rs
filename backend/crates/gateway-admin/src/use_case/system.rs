//! 系统管理用例

use std::sync::Arc;

use super::plugin_update::{ConfirmedPluginRestart, PluginSystemUpdatePreflight};
use crate::model::{MutationContext, system::SystemRestartPlan};
use async_trait::async_trait;

use crate::{
    model::{
        AdminError, AdminErrorKind,
        system::{
            SystemOperationAccepted, SystemUpdateChannel, SystemUpdateDetail, SystemUpdateStatus,
            SystemVersion,
        },
    },
    ports::system::{
        SystemOperationError, SystemOperationErrorKind, SystemOperations, SystemUpdateEventStream,
    },
};

/// API 消费的系统管理服务
#[async_trait]
pub trait SystemService: Send + Sync {
    async fn version(&self) -> Result<SystemVersion, AdminError>;
    async fn update_detail(
        &self,
        refresh: bool,
        channel: Option<SystemUpdateChannel>,
    ) -> Result<SystemUpdateDetail, AdminError>;
    fn update_events(&self) -> SystemUpdateEventStream;
    async fn perform_update(
        &self,
        target_version: Option<String>,
        channel: Option<SystemUpdateChannel>,
    ) -> Result<SystemOperationAccepted, AdminError>;
    async fn update_status(&self) -> Result<SystemUpdateStatus, AdminError>;
    async fn rollback(&self) -> Result<SystemOperationAccepted, AdminError>;
    async fn restart_plan(&self) -> Result<SystemRestartPlan, AdminError>;
    async fn restart(
        &self,
        confirmation: Option<SystemRestartPlan>,
        context: &MutationContext,
    ) -> Result<SystemOperationAccepted, AdminError>;
}

/// 保持 Host 能力窄边界的默认系统用例
pub(crate) struct DefaultSystemService {
    operations: Arc<dyn SystemOperations>,
    preflight: Arc<PluginSystemUpdatePreflight>,
}

impl DefaultSystemService {
    #[must_use]
    pub(crate) fn new(
        operations: Arc<dyn SystemOperations>,
        preflight: Arc<PluginSystemUpdatePreflight>,
    ) -> Self {
        Self {
            operations,
            preflight,
        }
    }
}

#[async_trait]
impl SystemService for DefaultSystemService {
    async fn version(&self) -> Result<SystemVersion, AdminError> {
        self.operations.version().await.map_err(map_system_error)
    }

    async fn update_detail(
        &self,
        refresh: bool,
        channel: Option<SystemUpdateChannel>,
    ) -> Result<SystemUpdateDetail, AdminError> {
        self.operations
            .update_detail(refresh, channel)
            .await
            .map_err(map_system_error)
    }

    fn update_events(&self) -> SystemUpdateEventStream {
        self.operations.update_events()
    }

    async fn perform_update(
        &self,
        target_version: Option<String>,
        channel: Option<SystemUpdateChannel>,
    ) -> Result<SystemOperationAccepted, AdminError> {
        let target_version = target_version
            .map(|version| version.trim().to_owned())
            .filter(|version| !version.is_empty());
        self.operations
            .perform_update(target_version, channel, self.preflight.clone())
            .await
            .map_err(map_system_error)
    }

    async fn update_status(&self) -> Result<SystemUpdateStatus, AdminError> {
        self.operations
            .update_status()
            .await
            .map_err(map_system_error)
    }

    async fn rollback(&self) -> Result<SystemOperationAccepted, AdminError> {
        self.operations
            .rollback(self.preflight.clone())
            .await
            .map_err(map_system_error)
    }

    async fn restart_plan(&self) -> Result<SystemRestartPlan, AdminError> {
        let candidate = self
            .operations
            .restart_candidate()
            .await
            .map_err(map_system_error)?;
        self.preflight
            .plan(candidate)
            .await
            .map_err(map_system_error)
    }

    async fn restart(
        &self,
        confirmation: Option<SystemRestartPlan>,
        context: &MutationContext,
    ) -> Result<SystemOperationAccepted, AdminError> {
        self.operations
            .restart(Arc::new(ConfirmedPluginRestart {
                preflight: self.preflight.clone(),
                confirmation,
                context: context.clone(),
            }))
            .await
            .map_err(map_system_error)
    }
}

fn map_system_error(error: SystemOperationError) -> AdminError {
    let kind = match error.kind() {
        SystemOperationErrorKind::Invalid => AdminErrorKind::Invalid,
        SystemOperationErrorKind::Conflict => AdminErrorKind::Conflict,
        SystemOperationErrorKind::Upstream => AdminErrorKind::BadGateway,
        SystemOperationErrorKind::Internal => AdminErrorKind::Internal,
    };
    let message = match kind {
        AdminErrorKind::Invalid => "系统操作请求不合法",
        AdminErrorKind::Conflict => error.message(),
        AdminErrorKind::BadGateway => "系统更新服务请求失败",
        AdminErrorKind::Internal => "系统操作失败",
        _ => "系统操作失败",
    };
    AdminError::new(kind, message)
}
