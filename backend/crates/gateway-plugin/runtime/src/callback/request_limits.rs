//! 从当前请求或已发布设置读取正文预算，不持有发布集合的强引用

use super::CallResources;
use gateway_core::runtime::{RuntimeSnapshotHandle, WeakRuntimeSnapshotHandle};
use gateway_plugin_sdk::{ErrorCode, PluginFault};
use std::sync::OnceLock;

#[derive(Default)]
pub(crate) struct RequestLimits {
    snapshots: OnceLock<WeakRuntimeSnapshotHandle>,
}

impl RequestLimits {
    pub(crate) fn bind(
        &self,
        snapshots: &RuntimeSnapshotHandle,
    ) -> Result<(), gateway_admin::model::AdminError> {
        self.snapshots
            .set(snapshots.downgrade())
            .map_err(|_| gateway_admin::model::AdminError::conflict("插件设置读取端口已经绑定"))
    }

    pub(super) fn body_bytes(&self, call: &CallResources) -> Result<usize, PluginFault> {
        if let Some(settings) = call.request_settings() {
            return Ok(settings.snapshot().responses_max_decompressed_body_bytes());
        }
        if let Some(bytes) = call.scope.request_body_limit {
            return Ok(bytes);
        }
        self.current_body_bytes()
    }

    pub(crate) fn current_body_bytes(&self) -> Result<usize, PluginFault> {
        self.snapshots
            .get()
            .and_then(|snapshots| snapshots.acquire().ok())
            .map(|snapshot| snapshot.responses_max_decompressed_body_bytes())
            .ok_or_else(|| PluginFault::new(ErrorCode::Fault, "request body limit is unavailable"))
    }
}

pub(crate) fn too_large() -> PluginFault {
    let mut fault = PluginFault::new(
        ErrorCode::InvalidInput,
        "request body exceeds the configured limit",
    );
    fault.http_status = Some(413);
    fault
}
