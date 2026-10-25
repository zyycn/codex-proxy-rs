//! 发布集合及其保活、候选贡献与配置身份指纹

use crate::{RpcSession, callback::private_state::PluginPrivateState};
use gateway_admin::model::{
    AdminError, Revision,
    plugins::instances::{PluginInstanceRuntimeFailure, PluginInstanceSnapshot},
};
use gateway_core::{
    engine::{
        extensions::ExecutionExtensionPlans, observation::RequestObserverPlan,
        policy::RequestPolicyPlan,
    },
    lifecycle::CancellationToken,
    routing::extensions::{ExtensionSetId, ExtensionSetLease},
};
use secrecy::ExposeSecret as _;
use sha2::{Digest as _, Sha256};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

pub(super) struct PreparedSet {
    pub(super) id: ExtensionSetId,
    pub(super) _observers: Option<Arc<dyn RequestObserverPlan>>,
    pub(super) _policies: Option<Arc<dyn RequestPolicyPlan>>,
    pub(super) _execution: Option<Arc<ExecutionExtensionPlans>>,
    pub(super) _authentication:
        Option<Arc<dyn gateway_core::engine::authentication::FrontendAuthenticationPlan>>,
    pub(super) sessions: Vec<Arc<PreparedInstance>>,
    pub(super) failures: BTreeMap<String, PluginInstanceRuntimeFailure>,
    pub(super) shutting_down: CancellationToken,
    pub(super) commands: Vec<Arc<crate::adapter::command_line::PluginCommand>>,
    pub(super) management: Vec<crate::adapter::management::ManagementEntry>,
    pub(super) model_aliases: Vec<gateway_core::routing::ContributedModelAlias>,
}

#[derive(Clone, Default)]
pub(super) struct PreparedContributions {
    pub(super) observer_entries: Vec<crate::adapter::observer::ObserverEntry>,
    pub(super) commands: Vec<Arc<crate::adapter::command_line::PluginCommand>>,
    pub(super) management: Vec<crate::adapter::management::ManagementEntry>,
    pub(super) middleware_entries: Vec<crate::adapter::middleware::MiddlewareEntry>,
    pub(super) policy_entries: Vec<crate::adapter::policy::PolicyEntry>,
    pub(super) upstream_entries: Vec<crate::adapter::upstream_adapter::AdapterEntry>,
    pub(super) authentication_entries:
        Vec<crate::adapter::frontend_authentication::FrontendAuthenticationEntry>,
    pub(super) model_aliases: Vec<gateway_core::routing::ContributedModelAlias>,
}

impl PreparedContributions {
    pub(super) fn append(&mut self, other: Self) {
        self.observer_entries.extend(other.observer_entries);
        self.commands.extend(other.commands);
        self.management.extend(other.management);
        self.model_aliases.extend(other.model_aliases);
        self.policy_entries.extend(other.policy_entries);
        self.middleware_entries.extend(other.middleware_entries);
        self.upstream_entries.extend(other.upstream_entries);
        self.authentication_entries
            .extend(other.authentication_entries);
    }
}

pub(super) struct PreparedInstance {
    pub(super) instance_id: String,
    pub(super) artifact_sha256: String,
    pub(super) revision: Revision,
    pub(super) session: Arc<RpcSession>,
    pub(super) private_state: Arc<PluginPrivateState>,
    pub(super) maintenance: bool,
    pub(super) contributions: PreparedContributions,
}

impl PreparedSet {
    pub(super) fn sessions_ready(&self) -> bool {
        self.sessions
            .iter()
            .all(|instance| instance.session.is_ready())
    }
}

impl ExtensionSetLease for PreparedSet {
    fn is_ready(&self) -> bool {
        self.sessions_ready() && self.can_serve()
    }

    fn model_aliases(&self) -> &[gateway_core::routing::ContributedModelAlias] {
        &self.model_aliases
    }

    fn can_serve(&self) -> bool {
        // 发布的是可用能力和故障绑定组成的完整计划；单个进程退出不能使原生转发失效
        !self.shutting_down.is_cancelled()
    }
}

impl Drop for PreparedInstance {
    fn drop(&mut self) {
        // 发布集合只共享同一配置的实例，最后一个实例持有者释放后才关闭进程
        // 先同步停止新调用，随后有界等待在途 I/O 并回收进程
        self.session.quiesce();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let session = Arc::clone(&self.session);
            handle.spawn(async move {
                session.shutdown(Duration::from_secs(1)).await;
            });
        }
    }
}
pub(super) fn fingerprint(snapshot: &PluginInstanceSnapshot) -> Result<String, AdminError> {
    let mut instances: Vec<_> = snapshot
        .instances
        .iter()
        .filter(|instance| instance.enabled)
        .collect();
    instances.sort_by(|left, right| left.id.cmp(&right.id));
    let mut hash = Sha256::new();
    for instance in instances {
        hash.update(instance_fingerprint(instance)?.as_bytes());
    }
    Ok(hex::encode(hash.finalize()))
}

pub(super) fn instance_fingerprint(
    instance: &gateway_admin::model::plugins::instances::PluginInstance,
) -> Result<String, AdminError> {
    let secrets: BTreeMap<_, _> = instance
        .secrets
        .iter()
        .map(|(name, value)| (name, value.expose_secret()))
        .collect();
    let mut value = serde_json::json!({"id":instance.id,"artifact":instance.artifact_sha256,"revision":instance.revision.get(),"trusted":instance.trusted_process,"configuration":instance.configuration,"secrets":secrets,"bindings":instance.bindings});
    // JSONB 恢复可以改变对象键顺序，不能因此替换已经准备好的同一配置
    value.sort_all_objects();
    let bytes = serde_json::to_vec(&value).map_err(|_| AdminError::invalid("插件配置无法编码"))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}
