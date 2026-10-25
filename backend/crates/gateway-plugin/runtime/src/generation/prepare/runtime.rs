//! 插件运行时配置、端口装配与整体关闭；候选和调用均复用同一状态

use super::super::restart_circuit::{PluginRestartCircuitConfig, RestartCircuits, RestartIdentity};
use super::{
    diagnostics::PreparationDiagnostic,
    set::{PreparedInstance, PreparedSet},
};
use crate::{
    PackageLimits, RpcLimits, RpcSession,
    callback::{
        PluginAccountPortSlot, PluginAffinityPortSlot, PluginClientKeyPortSlot, PluginModelPortSlot,
    },
};
use gateway_admin::{
    model::AdminError,
    ports::{
        plugin_accounts::PluginAccountAccess,
        plugin_client_keys::PluginClientKeyAccess,
        plugins::{PluginStateStore, PluginStore},
    },
};
use gateway_core::{
    engine::{
        authentication::FrontendAuthenticationExtensionIndex,
        extensions::ExecutionExtensionIndex,
        nested::{AffinityLookupPort, NestedModelExecutionPort},
        observation::RequestObserverExtensionIndex,
        policy::RequestPolicyExtensionIndex,
    },
    lifecycle::CancellationToken,
    provider_ports::OAuthPendingFlowPort,
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex as SyncMutex, Weak},
    time::Duration,
};
use tokio::sync::{Mutex, Semaphore};

const RUNTIME_SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
pub(super) const MAXIMUM_PARALLEL_PREPARATIONS: usize = 2;
pub struct PluginRuntimeConfig {
    pub cache_directory: PathBuf,
    pub host_version: semver::Version,
    pub package_limits: PackageLimits,
    pub rpc_limits: RpcLimits,
    pub restart_circuit: PluginRestartCircuitConfig,
}

/// 只索引有主人的候选；当前代次仅由 Core 的发布视图持有
pub struct PluginRuntime {
    pub(super) request_limits: Arc<crate::callback::request_limits::RequestLimits>,
    pub(super) service_ports: Arc<crate::callback::services::ServicePorts>,
    pub(super) store: Arc<dyn PluginStore>,
    pub(super) state: Arc<dyn PluginStateStore>,
    pub(super) config: PluginRuntimeConfig,
    pub(super) prepare_lock: Mutex<()>,
    pub(super) prepared: Mutex<BTreeMap<String, Weak<PreparedSet>>>,
    pub(super) prepared_instances: Mutex<BTreeMap<String, Weak<PreparedInstance>>>,
    pub(super) preparation_diagnostics: SyncMutex<BTreeMap<u64, PreparationDiagnostic>>,
    pub(super) restart_circuits: Arc<RestartCircuits>,
    pub(super) shutting_down: CancellationToken,
    shutdown_lock: Mutex<()>,
    pub(super) processes: Arc<gateway_host::process::ProcessSupervisor>,
    pub(super) validators: Arc<Semaphore>,
    pub(super) configuration_schemas: Arc<super::super::configuration::ConfigurationSchemas>,
    pub(super) log_slots: Arc<Semaphore>,
    pub(super) account_ports: Arc<PluginAccountPortSlot>,
    pub(super) client_key_ports: Arc<PluginClientKeyPortSlot>,
    pub(super) resource_ports: Arc<crate::callback::PluginResourcePorts>,
    pub(super) model_ports: Arc<PluginModelPortSlot>,
    pub(super) affinity_ports: Arc<PluginAffinityPortSlot>,
    pub(super) observers: RequestObserverExtensionIndex,
    pub(super) policies: RequestPolicyExtensionIndex,
    pub(super) execution: ExecutionExtensionIndex,
    pub(super) authentications: FrontendAuthenticationExtensionIndex,
    pub(super) oauth_pending: Option<Arc<dyn OAuthPendingFlowPort>>,
    pub(super) http: Arc<gateway_host::outbound::HttpClient>,
}
impl PluginRuntime {
    #[must_use]
    pub fn new(
        store: Arc<dyn PluginStore>,
        state: Arc<dyn PluginStateStore>,
        config: PluginRuntimeConfig,
        http: Arc<gateway_host::outbound::HttpClient>,
        processes: Arc<gateway_host::process::ProcessSupervisor>,
    ) -> Self {
        let restart_circuit = config.restart_circuit;
        Self {
            store,
            state,
            log_slots: Arc::new(Semaphore::new(
                config
                    .rpc_limits
                    .maximum_callbacks
                    .min(Semaphore::MAX_PERMITS),
            )),
            config,
            prepare_lock: Mutex::new(()),
            prepared: Mutex::new(BTreeMap::new()),
            prepared_instances: Mutex::new(BTreeMap::new()),
            preparation_diagnostics: SyncMutex::new(BTreeMap::new()),
            restart_circuits: RestartCircuits::new(restart_circuit),
            shutting_down: CancellationToken::new(),
            shutdown_lock: Mutex::new(()),
            processes,
            validators: Arc::new(Semaphore::new(MAXIMUM_PARALLEL_PREPARATIONS)),
            configuration_schemas: Arc::default(),
            request_limits: Arc::default(),
            service_ports: Arc::new(crate::callback::services::ServicePorts::default()),
            account_ports: Arc::new(PluginAccountPortSlot::new()),
            client_key_ports: Arc::new(PluginClientKeyPortSlot::new()),
            resource_ports: Arc::new(crate::callback::PluginResourcePorts::new()),
            model_ports: Arc::new(PluginModelPortSlot::new()),
            affinity_ports: Arc::new(PluginAffinityPortSlot::new()),
            observers: RequestObserverExtensionIndex::default(),
            policies: RequestPolicyExtensionIndex::default(),
            execution: ExecutionExtensionIndex::default(),
            authentications: FrontendAuthenticationExtensionIndex::default(),
            oauth_pending: None,
            http,
        }
    }

    #[must_use]
    pub fn with_oauth_pending(mut self, port: Arc<dyn OAuthPendingFlowPort>) -> Self {
        self.oauth_pending = Some(port);
        self
    }

    /// API 完成组装后绑定唯一内部 HTTP 分派端口；不持有服务端强引用
    pub fn bind_http(
        &self,
        dispatcher: &Arc<dyn gateway_core::engine::middleware::http::Dispatcher>,
    ) -> Result<(), AdminError> {
        self.service_ports.bind_http(dispatcher)
    }

    /// 读取当前发布设置，同时避免回调反向保活自身所在的插件集合
    pub fn bind_snapshots(
        &self,
        snapshots: &gateway_core::runtime::RuntimeSnapshotHandle,
    ) -> Result<(), AdminError> {
        self.request_limits.bind(snapshots)
    }

    pub fn bind_services(
        &self,
        registry: &Arc<gateway_admin::public_service::Registry>,
    ) -> Result<(), AdminError> {
        self.service_ports.bind(registry)
    }

    pub fn bind_account_ports(
        &self,
        access: &Arc<dyn PluginAccountAccess>,
    ) -> Result<(), AdminError> {
        self.account_ports.bind(access)
    }

    /// Key 目录与预算端口由 Admin 组合并保活，完整字段由对应公开合同返回
    pub fn bind_client_key_ports(
        &self,
        access: &Arc<dyn PluginClientKeyAccess>,
    ) -> Result<(), AdminError> {
        self.client_key_ports.bind(access)
    }

    /// Core 激活后一次性绑定嵌套执行与亲和查询端口；Runtime 仅保存 Weak，避免组合根强环
    pub fn bind_model_ports(
        &self,
        models: &Arc<dyn NestedModelExecutionPort>,
        affinity: &Arc<dyn AffinityLookupPort>,
    ) -> Result<(), AdminError> {
        self.model_ports.bind(models)?;
        self.affinity_ports.bind(affinity)
    }

    /// 调用方必须先停止接收 HTTP 与 Worker；这里拒绝新候选并等待全部插件 I/O 退出
    pub async fn shutdown(&self) {
        self.shutting_down.cancel();
        let _shutdown = self.shutdown_lock.lock().await;
        let _prepare = self.prepare_lock.lock().await;
        let sets = {
            let mut prepared = self.prepared.lock().await;
            prepared.retain(|_, set| set.strong_count() > 0);
            let sets = prepared
                .values()
                .filter_map(Weak::upgrade)
                .collect::<Vec<_>>();
            prepared.clear();
            sets
        };
        self.prepared_instances.lock().await.clear();
        let mut sessions = Vec::new();
        for set in &sets {
            for instance in &set.sessions {
                if !sessions
                    .iter()
                    .any(|session| Arc::ptr_eq(session, &instance.session))
                {
                    sessions.push(Arc::clone(&instance.session));
                }
            }
        }
        shutdown_sessions(sessions, RUNTIME_SHUTDOWN_GRACE).await;
    }

    #[must_use]
    pub fn observer_registry(&self) -> RequestObserverExtensionIndex {
        self.observers.clone()
    }

    #[must_use]
    pub fn policy_registry(&self) -> RequestPolicyExtensionIndex {
        self.policies.clone()
    }

    /// 执行计划按同一发布身份解析，索引不延长代次寿命
    #[must_use]
    pub fn execution_registry(&self) -> ExecutionExtensionIndex {
        self.execution.clone()
    }

    #[must_use]
    pub fn frontend_authentication_registry(&self) -> FrontendAuthenticationExtensionIndex {
        self.authentications.clone()
    }
}
pub(super) async fn shutdown_sessions(sessions: Vec<Arc<RpcSession>>, grace: Duration) {
    for session in &sessions {
        session.quiesce();
    }
    let deadline = tokio::time::Instant::now() + grace;
    futures::future::join_all(
        sessions
            .iter()
            .map(|session| session.shutdown_until(deadline)),
    )
    .await;
}
impl PluginRuntime {
    pub fn bind_resource_ports(
        &self,
        access: &Arc<dyn gateway_admin::ports::plugin_resources::PluginResourceAccess>,
    ) -> Result<(), AdminError> {
        self.resource_ports.bind(access)
    }
}

impl PluginRuntime {
    pub(super) fn restart_circuit_is_open(&self, identity: &RestartIdentity) -> bool {
        self.restart_circuits.is_open(identity)
    }
}
