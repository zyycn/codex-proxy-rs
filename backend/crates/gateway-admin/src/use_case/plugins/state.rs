//! 插件私有状态迁移的开始、撤销与存储错误映射

use std::sync::Arc;

use crate::{
    model::{
        AdminError, Revision,
        plugins::state::{PluginStateConfiguration, PluginStateTransition},
    },
    ports::plugins::{PluginStateStore, PluginStateStoreError, PluginStateStoreErrorKind},
};

pub(super) struct PluginStateService {
    store: Arc<dyn PluginStateStore>,
}

impl PluginStateService {
    #[must_use]
    pub(super) fn new(store: Arc<dyn PluginStateStore>) -> Self {
        Self { store }
    }

    pub(super) async fn transition_required(
        &self,
        instance_id: &str,
        target: &PluginStateConfiguration,
    ) -> Result<bool, AdminError> {
        self.store
            .transition_required(instance_id, target)
            .await
            .map_err(map_state_error)
    }

    pub(super) async fn begin_transition(
        &self,
        instance_id: &str,
        expected_instance_revision: Revision,
        artifact_sha256: &str,
        target: PluginStateConfiguration,
    ) -> Result<PluginStateTransition, AdminError> {
        self.store
            .begin_transition(
                instance_id,
                expected_instance_revision,
                artifact_sha256,
                target,
            )
            .await
            .map_err(map_state_error)
    }

    pub(super) async fn abort_transition(&self, transition_id: &str) {
        if let Err(error) = self.store.abort_transition(transition_id).await {
            tracing::warn!(
                error_kind = ?error.kind(),
                "failed to discard plugin state staging generation"
            );
        }
    }
}

fn map_state_error(error: PluginStateStoreError) -> AdminError {
    match error.kind() {
        PluginStateStoreErrorKind::Invalid => AdminError::invalid("插件状态声明不合法"),
        PluginStateStoreErrorKind::NotFound => AdminError::not_found("插件状态实例不存在"),
        PluginStateStoreErrorKind::Conflict => {
            AdminError::conflict("插件状态版本不兼容或已发生并发变更")
        }
        PluginStateStoreErrorKind::Quota => AdminError::conflict("插件状态迁移超过声明配额"),
        PluginStateStoreErrorKind::PermissionDenied => {
            AdminError::conflict("插件状态 fence 已失效")
        }
        PluginStateStoreErrorKind::Unavailable => AdminError::unavailable("插件状态存储暂不可用"),
    }
}
