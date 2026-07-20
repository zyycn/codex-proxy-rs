//! 系统管理用例。

use std::sync::Arc;

use async_trait::async_trait;

use crate::{
    model::{
        AdminError, AdminErrorKind,
        system::{SystemOperationAccepted, SystemUpdateDetail, SystemUpdateStatus, SystemVersion},
    },
    ports::system::{
        SystemOperationError, SystemOperationErrorKind, SystemOperations, SystemUpdateEventStream,
    },
};

/// API 消费的系统管理服务。
#[async_trait]
pub trait SystemService: Send + Sync {
    async fn version(&self) -> Result<SystemVersion, AdminError>;
    async fn update_detail(&self, refresh: bool) -> Result<SystemUpdateDetail, AdminError>;
    fn update_events(&self) -> SystemUpdateEventStream;
    async fn perform_update(
        &self,
        target_version: Option<String>,
    ) -> Result<SystemOperationAccepted, AdminError>;
    async fn update_status(&self) -> Result<SystemUpdateStatus, AdminError>;
    async fn rollback(&self) -> Result<SystemOperationAccepted, AdminError>;
    async fn restart(&self) -> Result<SystemOperationAccepted, AdminError>;
}

/// 保持 Host 能力窄边界的默认系统用例。
pub(crate) struct DefaultSystemService {
    operations: Arc<dyn SystemOperations>,
}

impl DefaultSystemService {
    #[must_use]
    pub(crate) const fn new(operations: Arc<dyn SystemOperations>) -> Self {
        Self { operations }
    }
}

#[async_trait]
impl SystemService for DefaultSystemService {
    async fn version(&self) -> Result<SystemVersion, AdminError> {
        self.operations.version().await.map_err(map_system_error)
    }

    async fn update_detail(&self, refresh: bool) -> Result<SystemUpdateDetail, AdminError> {
        self.operations
            .update_detail(refresh)
            .await
            .map_err(map_system_error)
    }

    fn update_events(&self) -> SystemUpdateEventStream {
        self.operations.update_events()
    }

    async fn perform_update(
        &self,
        target_version: Option<String>,
    ) -> Result<SystemOperationAccepted, AdminError> {
        let target_version = target_version
            .map(|version| version.trim().to_owned())
            .filter(|version| !version.is_empty());
        self.operations
            .perform_update(target_version)
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
        self.operations.rollback().await.map_err(map_system_error)
    }

    async fn restart(&self) -> Result<SystemOperationAccepted, AdminError> {
        self.operations.restart().await.map_err(map_system_error)
    }
}

fn map_system_error(error: SystemOperationError) -> AdminError {
    let kind = match error.kind() {
        SystemOperationErrorKind::Invalid => AdminErrorKind::Invalid,
        SystemOperationErrorKind::Conflict => AdminErrorKind::Conflict,
        SystemOperationErrorKind::Upstream => AdminErrorKind::BadGateway,
        SystemOperationErrorKind::Internal => AdminErrorKind::Internal,
    };
    AdminError::new(kind, error.message())
}
