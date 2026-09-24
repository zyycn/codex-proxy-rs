mod instance;

use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroUsize,
    path::PathBuf,
    sync::{
        Arc, Mutex as SyncMutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use futures::future::BoxFuture;
use gateway_admin::{
    model::{
        AdminError, AdminErrorKind, Revision,
        plugins::{
            instances::{
                PluginInstance, PluginInstanceRuntime, PluginInstanceRuntimeFailure,
                PluginInstanceRuntimeStatus, PluginInstanceSnapshot,
            },
            state::{
                ApplyPluginStateMigration, PluginStateConfiguration, PluginStateMigrationAction,
                PluginStateMigrationChange, PluginStateTransition,
            },
        },
    },
    ports::{
        plugin_accounts::PluginAccountAccess,
        plugin_client_keys::PluginClientKeyAccess,
        plugins::{PluginPreparation, PluginStateStore, PluginStateStoreErrorKind, PluginStore},
        provider::{ProviderAdmin, ProviderAdminRegistry},
        provider_extensions::ProviderAdminExtensionIndex,
    },
};
use gateway_core::{
    engine::{
        authentication::FrontendAuthenticationExtensionIndex,
        extensions::ProviderExtensionIndex,
        middleware::{MiddlewareExtensionIndex, MiddlewarePlan},
        nested::{AffinityLookupPort, NestedModelExecutionPort},
        observation::{RequestObserverExtensionIndex, RequestObserverPlan},
        policy::{RequestPolicyExtensionIndex, RequestPolicyPlan},
        provider::{Provider, ProviderRegistry},
    },
    provider_ports::{ProviderStoreErrorKind, ProviderStorePorts},
    routing::{ConfigRevision, ProviderKind},
    runtime::extensions::{
        ExtensionPreparationError, ExtensionPreparationPort, ExtensionSetId, ExtensionSetLease,
        ExtensionSetReference,
    },
};
use gateway_plugin_sdk::{
    Capability, Handshake, Stage,
    call::{
        host::{
            StateMigrationChange, StateMigrationRecord, StateMigrationRequest, StateMigrationResult,
        },
        provider::Registration,
    },
};
use secrecy::ExposeSecret as _;
use sha2::{Digest as _, Sha256};
use tokio::sync::{Mutex, Semaphore};

use crate::{
    PackageLimits, RpcLimits, RpcSession, ValidatedPackage,
    callback::{
        PluginAccountPortSlot, PluginAffinityPortSlot, PluginCallbackPorts, PluginCallbacks,
        PluginClientKeyPortSlot, PluginModelPortSlot, private_state::PluginPrivateState,
    },
};

use super::restart_circuit::{PluginRestartCircuitConfig, RestartCircuits, RestartIdentity};

const RUNTIME_SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
const MAXIMUM_PREPARATION_DIAGNOSTICS: usize = 8;

pub struct PluginRuntimeConfig {
    pub cache_directory: PathBuf,
    pub host_version: semver::Version,
    pub package_limits: PackageLimits,
    pub rpc_limits: RpcLimits,
    pub continuation_drain: ContinuationDrainConfig,
    pub restart_circuit: PluginRestartCircuitConfig,
}

#[derive(Debug, Clone, Copy)]
pub struct ContinuationDrainConfig {
    pub retention: Duration,
    pub maximum_generations: NonZeroUsize,
    pub maximum_per_instance: NonZeroUsize,
}

impl Default for ContinuationDrainConfig {
    fn default() -> Self {
        Self {
            retention: Duration::from_secs(30 * 60),
            maximum_generations: NonZeroUsize::new(64).unwrap_or(NonZeroUsize::MIN),
            maximum_per_instance: NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN),
        }
    }
}

/// 只索引有主人的候选；当前代次仅由 Core 的发布视图持有。
pub struct PluginRuntime {
    pub(super) store: Arc<dyn PluginStore>,
    state: Arc<dyn PluginStateStore>,
    config: PluginRuntimeConfig,
    prepare_lock: Mutex<()>,
    prepared: Mutex<BTreeMap<String, Weak<PreparedSet>>>,
    preparation_diagnostics: SyncMutex<BTreeMap<u64, PreparationDiagnostic>>,
    restart_circuits: Arc<RestartCircuits>,
    shutting_down: Arc<AtomicBool>,
    shutdown_lock: Mutex<()>,
    processes: Arc<gateway_host::process::ProcessSupervisor>,
    validators: Arc<Semaphore>,
    log_slots: Arc<Semaphore>,
    pub(super) account_ports: Arc<PluginAccountPortSlot>,
    client_key_ports: Arc<PluginClientKeyPortSlot>,
    model_ports: Arc<PluginModelPortSlot>,
    affinity_ports: Arc<PluginAffinityPortSlot>,
    providers: ProviderExtensionIndex,
    observers: RequestObserverExtensionIndex,
    policies: RequestPolicyExtensionIndex,
    middleware: MiddlewareExtensionIndex,
    authentications: FrontendAuthenticationExtensionIndex,
    admin_providers: ProviderAdminExtensionIndex,
    provider_ports: Option<ProviderStorePorts>,
    http: Arc<gateway_host::outbound::HttpClient>,
    network_policy: gateway_host::outbound::NetworkPolicy,
    continuations: Arc<crate::adapter::provider::ContinuationDrain>,
}

#[derive(Clone)]
enum PreparationDiagnostic {
    Preparing,
    Prepared { set_id: String },
    Failed(PluginInstanceRuntimeFailure),
}

pub(super) struct PreparedSet {
    id: ExtensionSetId,
    providers: Arc<ProviderRegistry>,
    request_profile_providers: Vec<ProviderKind>,
    _observers: Option<Arc<dyn RequestObserverPlan>>,
    _policies: Option<Arc<dyn RequestPolicyPlan>>,
    _middleware: Option<Arc<dyn MiddlewarePlan>>,
    _authentication:
        Option<Arc<dyn gateway_core::engine::authentication::FrontendAuthenticationPlan>>,
    _admin_providers: Arc<ProviderAdminRegistry>,
    sessions: Vec<PreparedInstance>,
    failures: BTreeMap<String, PluginInstanceRuntimeFailure>,
    shutting_down: Arc<AtomicBool>,
    commands: Vec<Arc<crate::adapter::command_line::PluginCommand>>,
    management: Vec<crate::adapter::management::ManagementEntry>,
    pub(super) maintenance: Vec<super::worker::MaintenanceEntry>,
}

#[derive(Default)]
struct PreparedContributions {
    sessions: Vec<PreparedInstance>,
    providers: Vec<Arc<dyn Provider>>,
    request_profile_providers: Vec<ProviderKind>,
    admin_providers: Vec<Arc<dyn ProviderAdmin>>,
    observer_entries: Vec<crate::adapter::observer::ObserverEntry>,
    commands: Vec<Arc<crate::adapter::command_line::PluginCommand>>,
    management: Vec<crate::adapter::management::ManagementEntry>,
    maintenance: Vec<super::worker::MaintenanceEntry>,
    policy_entries: Vec<crate::adapter::policy::PolicyEntry>,
    authentication_entries:
        Vec<crate::adapter::frontend_authentication::FrontendAuthenticationEntry>,
}

impl PreparedContributions {
    fn append(&mut self, other: Self) {
        self.sessions.extend(other.sessions);
        self.providers.extend(other.providers);
        self.request_profile_providers
            .extend(other.request_profile_providers);
        self.admin_providers.extend(other.admin_providers);
        self.observer_entries.extend(other.observer_entries);
        self.commands.extend(other.commands);
        self.management.extend(other.management);
        self.maintenance.extend(other.maintenance);
        self.policy_entries.extend(other.policy_entries);
        self.authentication_entries
            .extend(other.authentication_entries);
    }
}

struct PreparedInstance {
    instance_id: String,
    artifact_sha256: String,
    revision: Revision,
    session: Arc<RpcSession>,
    private_state: Arc<PluginPrivateState>,
    continuation_provider: Option<Arc<crate::adapter::provider::PluginProvider>>,
}

impl PreparedSet {
    fn sessions_ready(&self) -> bool {
        self.sessions
            .iter()
            .all(|instance| instance.session.is_ready())
    }
}

impl ExtensionSetLease for PreparedSet {
    fn is_ready(&self) -> bool {
        self.sessions_ready() && self.can_serve()
    }

    fn can_serve(&self) -> bool {
        // 发布的是可用能力和故障绑定组成的完整计划；单个进程退出不能使原生转发失效。
        !self.shutting_down.load(Ordering::Acquire)
    }
}

impl Drop for PreparedSet {
    fn drop(&mut self) {
        for mut instance in self.sessions.drain(..) {
            if instance
                .continuation_provider
                .take()
                .is_some_and(|provider| provider.retire_continuation())
            {
                continue;
            }
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let session = instance.session;
                    session.quiesce();
                    session.shutdown(Duration::from_secs(1)).await;
                });
            }
        }
    }
}

impl PluginRuntime {
    #[must_use]
    pub fn new(
        store: Arc<dyn PluginStore>,
        state: Arc<dyn PluginStateStore>,
        config: PluginRuntimeConfig,
        providers: ProviderRegistry,
        admin_providers: ProviderAdminRegistry,
        http: Arc<gateway_host::outbound::HttpClient>,
        processes: Arc<gateway_host::process::ProcessSupervisor>,
    ) -> Self {
        let continuations =
            crate::adapter::provider::ContinuationDrain::new(config.continuation_drain);
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
            preparation_diagnostics: SyncMutex::new(BTreeMap::new()),
            restart_circuits: RestartCircuits::new(restart_circuit),
            shutting_down: Arc::new(AtomicBool::new(false)),
            shutdown_lock: Mutex::new(()),
            processes,
            validators: Arc::new(Semaphore::new(2)),
            account_ports: Arc::new(PluginAccountPortSlot::new()),
            client_key_ports: Arc::new(PluginClientKeyPortSlot::new()),
            model_ports: Arc::new(PluginModelPortSlot::new()),
            affinity_ports: Arc::new(PluginAffinityPortSlot::new()),
            providers: ProviderExtensionIndex::new(providers),
            observers: RequestObserverExtensionIndex::default(),
            policies: RequestPolicyExtensionIndex::default(),
            middleware: MiddlewareExtensionIndex::default(),
            authentications: FrontendAuthenticationExtensionIndex::default(),
            admin_providers: ProviderAdminExtensionIndex::new(admin_providers),
            provider_ports: None,
            http,
            network_policy: gateway_host::outbound::NetworkPolicy::default(),
            continuations,
        }
    }

    #[must_use]
    pub fn with_provider_ports(mut self, ports: ProviderStorePorts) -> Self {
        self.provider_ports = Some(ports);
        self
    }

    /// 宿主统一决定可到达的地址段；插件配置与请求不能自行放宽 DNS/IP 边界。
    #[must_use]
    pub fn with_network_policy(mut self, policy: gateway_host::outbound::NetworkPolicy) -> Self {
        self.network_policy = policy;
        self
    }

    /// Admin 初始化后一次性绑定窄账号端口；Runtime 仅保存 Weak，避免组合根强环。
    pub fn bind_account_ports(
        &self,
        access: &Arc<dyn PluginAccountAccess>,
    ) -> Result<(), AdminError> {
        self.account_ports.bind(access)
    }

    /// Key 目录由 Admin 组合并保活，Runtime 不取得明文访问端口。
    pub fn bind_client_key_ports(
        &self,
        access: &Arc<dyn PluginClientKeyAccess>,
    ) -> Result<(), AdminError> {
        self.client_key_ports.bind(access)
    }

    /// Core 激活后一次性绑定嵌套执行与亲和查询端口；Runtime 仅保存 Weak，避免组合根强环。
    pub fn bind_model_ports(
        &self,
        models: &Arc<dyn NestedModelExecutionPort>,
        affinity: &Arc<dyn AffinityLookupPort>,
    ) -> Result<(), AdminError> {
        self.model_ports.bind(models)?;
        self.affinity_ports.bind(affinity)
    }

    /// 调用方必须先停止接收 HTTP 与 Worker；这里拒绝新候选并等待全部插件 I/O 退出。
    pub async fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::Release);
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
        let mut sessions = self.continuations.sessions();
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
        self.continuations.finish_shutdown();
    }

    #[must_use]
    pub fn provider_registry(&self) -> ProviderRegistry {
        self.providers.registry()
    }

    #[must_use]
    pub fn observer_registry(&self) -> RequestObserverExtensionIndex {
        self.observers.clone()
    }

    #[must_use]
    pub fn policy_registry(&self) -> RequestPolicyExtensionIndex {
        self.policies.clone()
    }

    #[must_use]
    pub fn middleware_registry(&self) -> MiddlewareExtensionIndex {
        self.middleware.clone()
    }

    #[must_use]
    pub fn frontend_authentication_registry(&self) -> FrontendAuthenticationExtensionIndex {
        self.authentications.clone()
    }

    #[must_use]
    pub fn admin_registry(
        &self,
        snapshots: gateway_core::runtime::RuntimeSnapshotHandle,
    ) -> ProviderAdminRegistry {
        self.admin_providers.registry(snapshots)
    }

    fn record_preparation(&self, revision: u64, diagnostic: PreparationDiagnostic) {
        let mut diagnostics = self
            .preparation_diagnostics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        diagnostics.insert(revision, diagnostic);
        while diagnostics.len() > MAXIMUM_PREPARATION_DIAGNOSTICS {
            diagnostics.pop_first();
        }
    }

    fn preparation_diagnostic(&self, revision: u64) -> Option<PreparationDiagnostic> {
        self.preparation_diagnostics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&revision)
            .cloned()
    }

    fn restart_circuit_is_open(&self, identity: &RestartIdentity) -> bool {
        self.restart_circuits.is_open(identity)
    }

    async fn validate_request_profile_configurations(
        &self,
        revision: ConfigRevision,
        providers: &ProviderRegistry,
        provider_kinds: &[ProviderKind],
    ) -> Result<(), AdminError> {
        for provider_kind in provider_kinds {
            let provider = providers
                .get(provider_kind)
                .ok_or_else(|| AdminError::invalid("插件 Provider 注册结果缺失"))?;
            self.validate_provider_request_profiles(revision, provider_kind, provider.as_ref())
                .await?;
        }
        Ok(())
    }

    async fn validate_provider_request_profiles(
        &self,
        revision: ConfigRevision,
        provider_kind: &ProviderKind,
        provider: &dyn Provider,
    ) -> Result<(), AdminError> {
        let ports = self
            .provider_ports
            .as_ref()
            .ok_or_else(|| AdminError::unavailable("Provider 宿主端口不可用"))?;
        let configurations = ports
            .runtime_policy()
            .load_request_profile_configurations(revision, provider_kind)
            .await
            .map_err(|error| match error.kind() {
                ProviderStoreErrorKind::Unavailable => {
                    AdminError::unavailable("Provider 请求画像配置读取失败")
                }
                ProviderStoreErrorKind::Conflict => AdminError::conflict("插件候选配置版本已变化"),
                ProviderStoreErrorKind::InvalidData => {
                    AdminError::invalid("Provider 请求画像配置无效")
                }
            })?;
        configurations.iter().try_for_each(|configuration| {
            provider
                .resolve_request_profile(configuration)
                .map(|_| ())
                .map_err(|_| AdminError::invalid("已保存的 Provider 请求画像不受当前插件支持"))
        })
    }

    async fn prepare_snapshot(
        &self,
        source_revision: Revision,
        snapshot: PluginInstanceSnapshot,
        required_revision: Option<Revision>,
    ) -> Result<ExtensionSetReference, AdminError> {
        let target_revision = snapshot.config_revision.get();
        self.record_preparation(target_revision, PreparationDiagnostic::Preparing);
        let result = self
            .prepare_inner(source_revision, snapshot, required_revision)
            .await;
        match &result {
            Ok(reference) => self.record_preparation(
                target_revision,
                PreparationDiagnostic::Prepared {
                    set_id: reference.id().as_str().to_owned(),
                },
            ),
            Err(error) => self.record_preparation(
                target_revision,
                PreparationDiagnostic::Failed(runtime_failure_from_admin(error)),
            ),
        }
        result
    }

    async fn prepare_inner(
        &self,
        source_revision: Revision,
        snapshot: PluginInstanceSnapshot,
        required_revision: Option<Revision>,
    ) -> Result<ExtensionSetReference, AdminError> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(AdminError::unavailable("插件运行时正在关闭"));
        }
        if snapshot.instances.len() > 64 {
            return Err(AdminError::invalid("最多配置 64 个插件实例"));
        }
        self.continuations.reap_expired().await;
        let config_revision = ConfigRevision::new(source_revision.get())
            .map_err(|_| AdminError::invalid("插件候选事实版本无效"))?;
        let fingerprint = fingerprint(&snapshot)?;
        // 准备串行化与弱索引分锁；诊断和已发布请求不会等待外部 I/O。
        let _prepare = self.prepare_lock.lock().await;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(AdminError::unavailable("插件运行时正在关闭"));
        }
        let existing = {
            let mut prepared = self.prepared.lock().await;
            prepared.retain(|_, set| set.strong_count() > 0);
            prepared
                .get(&fingerprint)
                .and_then(Weak::upgrade)
                .filter(|set| set.sessions_ready())
        };
        if let Some(set) = existing {
            if snapshot.instances.iter().any(|instance| {
                required_revision == Some(instance.revision)
                    && set.failures.contains_key(&instance.id)
            }) {
                return Err(AdminError::unavailable("插件启动失败，请重新启用后重试"));
            }
            self.validate_request_profile_configurations(
                config_revision,
                &set.providers,
                &set.request_profile_providers,
            )
            .await?;
            return Ok(ExtensionSetReference::new(set.id.clone(), set));
        }
        let mut contributions = PreparedContributions::default();
        let mut failures = BTreeMap::new();
        let mut identities = BTreeSet::new();
        for instance in snapshot
            .instances
            .iter()
            .filter(|instance| instance.enabled)
        {
            if !identities.insert(instance.id.clone()) {
                return Err(AdminError::invalid("插件实例 ID 重复"));
            }
            let result = tokio::time::timeout(
                // 沿用准备阶段原有的 30 秒预算；打包校验与能力恢复也在预算内。
                Duration::from_secs(30),
                self.prepare_instance(instance.clone(), config_revision, snapshot.config_revision),
            )
            .await
            .unwrap_or_else(|_| Err(AdminError::unavailable("插件启动超时")));
            let result = result.and_then(|candidate| {
                self.validate_provider_addition(&contributions, &candidate)?;
                Ok(candidate)
            });
            match result {
                Ok(candidate) => contributions.append(candidate),
                Err(error) if required_revision == Some(instance.revision) => return Err(error),
                Err(error) => {
                    // 恢复失败只隔离该实例；绑定的拒绝策略仍保留，不能绕过认证或必需处理。
                    contributions
                        .policy_entries
                        .extend(crate::adapter::policy::unavailable_entries(instance)?);
                    if let Some(entry) =
                        crate::adapter::frontend_authentication::unavailable_entry(instance)
                    {
                        contributions.authentication_entries.push(entry);
                    }
                    failures.insert(instance.id.clone(), runtime_failure_from_admin(&error));
                }
            }
        }
        let PreparedContributions {
            sessions,
            providers,
            request_profile_providers,
            admin_providers,
            observer_entries,
            commands,
            management,
            maintenance,
            policy_entries,
            authentication_entries,
        } = contributions;
        let id = ExtensionSetId::new(uuid::Uuid::new_v4().to_string())
            .map_err(|_| AdminError::internal("插件集合 ID 无效"))?;
        let providers = self
            .providers
            .register(id.clone(), providers)
            .map_err(|_| AdminError::invalid("插件 Provider 注册冲突"))?;
        let observers = crate::adapter::observer::PluginObserverPlan::compile(
            observer_entries,
            self.config.rpc_limits.maximum_calls,
            self.config.rpc_limits.maximum_call_timeout,
            self.config.rpc_limits.maximum_frame_bytes,
        )
        .map(|plan| {
            self.observers
                .register(id.clone(), plan)
                .map_err(|_| AdminError::invalid("插件观察计划注册冲突"))
        })
        .transpose()?;
        let policy_plan = crate::adapter::policy::PluginRequestPolicyPlan::compile(
            policy_entries,
            self.config.rpc_limits.maximum_call_timeout,
            self.config.rpc_limits.maximum_frame_bytes,
        )?;
        let policies = policy_plan
            .as_ref()
            .filter(|plan| plan.has_request_policy())
            .cloned()
            .map(|plan| {
                self.policies
                    .register(id.clone(), plan)
                    .map_err(|_| AdminError::invalid("插件请求策略计划注册冲突"))
            })
            .transpose()?;
        let middleware = policy_plan
            .filter(|plan| plan.has_middleware())
            .map(|plan| {
                self.middleware
                    .register(id.clone(), plan)
                    .map_err(|_| AdminError::invalid("插件中间件计划注册冲突"))
            })
            .transpose()?;
        let authentication =
            crate::adapter::frontend_authentication::PluginFrontendAuthenticationPlan::compile(
                authentication_entries,
                self.config.rpc_limits.maximum_call_timeout,
            )?
            .map(|plan| {
                self.authentications
                    .register(id.clone(), plan)
                    .map_err(|_| AdminError::invalid("插件入口认证计划注册冲突"))
            })
            .transpose()?;
        let set = Arc::new(PreparedSet {
            _admin_providers: self
                .admin_providers
                .register(id.clone(), admin_providers)
                .map_err(|_| AdminError::invalid("插件管理 Provider 注册冲突"))?,
            id,
            providers,
            request_profile_providers,
            _observers: observers,
            _policies: policies,
            _middleware: middleware,
            _authentication: authentication,
            sessions,
            commands,
            management,
            maintenance,
            failures,
            shutting_down: self.shutting_down.clone(),
        });
        self.prepared
            .lock()
            .await
            .insert(fingerprint, Arc::downgrade(&set));
        Ok(ExtensionSetReference::new(set.id.clone(), set))
    }

    fn validate_provider_addition(
        &self,
        current: &PreparedContributions,
        candidate: &PreparedContributions,
    ) -> Result<(), AdminError> {
        // 先在私有候选中检查身份冲突，避免一个插件覆盖原生 Provider 或拖累其他实例。
        let id = ExtensionSetId::new(uuid::Uuid::new_v4().to_string())
            .map_err(|_| AdminError::internal("插件集合 ID 无效"))?;
        let _providers = self
            .providers
            .register(
                id.clone(),
                current
                    .providers
                    .iter()
                    .chain(&candidate.providers)
                    .cloned(),
            )
            .map_err(|_| AdminError::invalid("插件 Provider 注册冲突"))?;
        let _admin = self
            .admin_providers
            .register(
                id,
                current
                    .admin_providers
                    .iter()
                    .chain(&candidate.admin_providers)
                    .cloned(),
            )
            .map_err(|_| AdminError::invalid("插件管理 Provider 注册冲突"))?;
        Ok(())
    }

    /// CLI 复用同一候选准备与弱索引；读取帮助不会发布 Core 快照或执行命令。
    pub async fn prepare_command_line(&self) -> Result<crate::PluginCommandSession, AdminError> {
        let snapshot = self
            .store
            .load_instances()
            .await
            .map_err(|_| AdminError::unavailable("无法读取已安装插件"))?;
        let reference = self
            .prepare_snapshot(snapshot.config_revision, snapshot, None)
            .await?;
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

    pub(super) async fn prepared_set(
        &self,
        reference: &ExtensionSetReference,
    ) -> Result<Arc<PreparedSet>, AdminError> {
        self.prepared
            .lock()
            .await
            .values()
            .filter_map(Weak::upgrade)
            .find(|set| set.id == *reference.id())
            .ok_or_else(|| AdminError::conflict("插件候选已过期，请重试"))
    }

    async fn run_state_migration(
        &self,
        prepared: &ExtensionSetReference,
        transition: PluginStateTransition,
    ) -> Result<(), AdminError> {
        let set = self.prepared_set(prepared).await?;
        let instance = set
            .sessions
            .iter()
            .find(|instance| {
                instance.instance_id == transition.instance_id
                    && instance.artifact_sha256 == transition.artifact_sha256
            })
            .ok_or_else(|| AdminError::conflict("状态迁移候选插件已失效"))?;
        for namespace in &transition.namespaces {
            let mut batches = 0usize;
            loop {
                batches += 1;
                if batches > 200 {
                    return Err(AdminError::invalid("插件状态迁移超过有界批次数"));
                }
                let batch = self
                    .state
                    .migration_batch(&transition.id, &namespace.namespace, 64)
                    .await
                    .map_err(map_state_admin_error)?;
                let expected_keys = batch
                    .records
                    .iter()
                    .map(|record| record.key.clone())
                    .collect::<Vec<_>>();
                if batch.records.is_empty() {
                    self.state
                        .apply_migration_batch(ApplyPluginStateMigration {
                            transition_id: transition.id.clone(),
                            namespace: namespace.namespace.clone(),
                            cursor: batch.cursor,
                            expected_keys,
                            changes: Vec::new(),
                        })
                        .await
                        .map_err(map_state_admin_error)?;
                    break;
                }
                let request = StateMigrationRequest {
                    namespace: namespace.namespace.clone(),
                    from_schema_version: namespace.from_schema_version,
                    to_schema_version: namespace.to_schema_version,
                    records: batch
                        .records
                        .iter()
                        .map(|record| StateMigrationRecord {
                            key: record.key.clone(),
                            value: record.value.clone(),
                            version: record.version,
                        })
                        .collect(),
                };
                let reply = instance
                    .session
                    .call(
                        "plugin.state.migrate",
                        instance
                            .session
                            .context(Stage::Configuration, Duration::from_secs(30)),
                        serde_json::json!({}),
                        serde_json::to_vec(&request)
                            .map_err(|_| AdminError::internal("状态迁移批次无法编码"))?,
                    )
                    .await
                    .map_err(|_| AdminError::invalid("插件状态迁移调用失败"))?;
                if reply.result != serde_json::json!({}) {
                    return Err(AdminError::invalid("插件状态迁移结果信封无效"));
                }
                let result: StateMigrationResult = serde_json::from_slice(&reply.payload)
                    .map_err(|_| AdminError::invalid("插件状态迁移结果无效"))?;
                if result.changes.len() != batch.records.len()
                    || result
                        .changes
                        .iter()
                        .zip(&expected_keys)
                        .any(|(change, expected)| change.key() != expected)
                {
                    return Err(AdminError::invalid("插件状态迁移结果未逐项对应源批次"));
                }
                let mut changes = Vec::with_capacity(result.changes.len());
                for (change, source) in result.changes.into_iter().zip(&batch.records) {
                    let (key, action) = match change {
                        StateMigrationChange::Keep { key } => {
                            instance
                                .private_state
                                .validate_value(&namespace.namespace, &source.value)?;
                            (key, PluginStateMigrationAction::Keep)
                        }
                        StateMigrationChange::Replace { key, value } => {
                            instance
                                .private_state
                                .validate_value(&namespace.namespace, &value)?;
                            (key, PluginStateMigrationAction::Replace(value))
                        }
                        StateMigrationChange::Delete { key } => {
                            (key, PluginStateMigrationAction::Delete)
                        }
                    };
                    changes.push(PluginStateMigrationChange { key, action });
                }
                self.state
                    .apply_migration_batch(ApplyPluginStateMigration {
                        transition_id: transition.id.clone(),
                        namespace: namespace.namespace.clone(),
                        cursor: batch.cursor,
                        expected_keys,
                        changes,
                    })
                    .await
                    .map_err(map_state_admin_error)?;
            }
        }
        Ok(())
    }
}

#[async_trait]
impl gateway_admin::ports::plugin_management::PluginManagement for PluginRuntime {
    async fn authorize_models(
        &self,
        published: &ExtensionSetReference,
        target: &gateway_admin::model::plugins::management::PluginManagementTarget,
    ) -> Result<(), AdminError> {
        let set = self.prepared_set(published).await?;
        let entry = set
            .management
            .iter()
            .find(|entry| &entry.view.target == target)
            .ok_or_else(|| AdminError::conflict("插件页面版本已变化，请刷新页面"))?;
        if !entry.models_authorized {
            return Err(AdminError::new(
                gateway_admin::model::AdminErrorKind::Forbidden,
                "插件未声明模型访问能力",
            ));
        }
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
            .provider_ports
            .as_ref()
            .ok_or_else(|| AdminError::unavailable("插件登录状态端口不可用"))?
            .oauth_pending();
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
            .provider_ports
            .as_ref()
            .ok_or_else(|| AdminError::unavailable("插件登录状态端口不可用"))?
            .oauth_pending();
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

#[async_trait]
impl PluginPreparation for PluginRuntime {
    async fn configuration_ready(
        &self,
        instance: PluginInstance,
        metadata: &gateway_admin::model::plugins::PluginArtifactMetadata,
    ) -> Result<bool, AdminError> {
        if instance.artifact_sha256 != metadata.sha256 {
            return Err(AdminError::invalid("插件配置与制品不匹配"));
        }
        // 安装时已验证声明；只读列表不重新解包，也不占用安装与启用的校验配额。
        let schema = metadata.configuration_schema.clone();
        let secret_fields = metadata.secret_fields.iter().cloned().collect();
        tokio::task::spawn_blocking(move || {
            super::configuration::configuration_ready(&instance, &schema, &secret_fields)
        })
        .await
        .map_err(|_| AdminError::internal("插件校验任务失败"))?
    }

    async fn validate(
        &self,
        instance: PluginInstance,
    ) -> Result<PluginStateConfiguration, AdminError> {
        let artifact = self
            .store
            .load_artifact(&instance.artifact_sha256)
            .await
            .map_err(|_| AdminError::not_found("插件制品不存在"))?;
        let limits = self.config.package_limits;
        let slot = self
            .validators
            .clone()
            .try_acquire_owned()
            .map_err(|_| AdminError::unavailable("插件校验繁忙"))?;
        tokio::task::spawn_blocking(move || {
            let _slot = slot;
            let package =
                ValidatedPackage::read(artifact.archive, Some(&instance.artifact_sha256), limits)
                    .map_err(|_| AdminError::invalid("插件制品校验失败"))?;
            let (_, _, state) = super::configuration::validate(&instance, package.manifest())?;
            crate::adapter::observer::validate_bindings(package.manifest(), &instance.bindings)?;
            crate::adapter::policy::validate_bindings(package.manifest(), &instance.bindings)?;
            crate::adapter::frontend_authentication::validate_bindings(
                package.manifest(),
                &instance.bindings,
            )?;
            Ok(state)
        })
        .await
        .map_err(|_| AdminError::internal("插件校验任务失败"))?
    }
    async fn prepare(
        &self,
        source_revision: Revision,
        snapshot: PluginInstanceSnapshot,
    ) -> Result<ExtensionSetReference, AdminError> {
        let required_revision = snapshot.config_revision;
        self.prepare_snapshot(source_revision, snapshot, Some(required_revision))
            .await
    }

    async fn runtime_diagnostics(
        &self,
        snapshot: &PluginInstanceSnapshot,
        published_revision: Option<u64>,
        published: Option<&ExtensionSetReference>,
    ) -> Option<BTreeMap<String, PluginInstanceRuntime>> {
        let sets = {
            let mut prepared = self.prepared.lock().await;
            prepared.retain(|_, set| set.strong_count() > 0);
            prepared
                .values()
                .filter_map(Weak::upgrade)
                .collect::<Vec<_>>()
        };
        let open_restart_circuits = snapshot
            .instances
            .iter()
            .filter(|instance| instance.enabled)
            .filter_map(|instance| {
                let identity =
                    RestartIdentity::new(instance.id.clone(), instance_fingerprint(instance).ok()?);
                self.restart_circuit_is_open(&identity)
                    .then(|| instance.id.clone())
            })
            .collect::<BTreeSet<_>>();
        let published_set = published
            .and_then(|reference| sets.iter().find(|set| set.id == *reference.id()).cloned());
        let published_ready = published.is_some_and(ExtensionSetReference::can_serve);
        let preparation = self.preparation_diagnostic(snapshot.config_revision.get());
        let retained = self.continuations.retained_by_instance();
        let projection = InstanceRuntimeProjection {
            target_revision: snapshot.config_revision,
            published_revision,
            published_ready,
            published_set: published_set.as_deref(),
            sets: &sets,
            preparation: preparation.as_ref(),
            retained: &retained,
            open_restart_circuits: &open_restart_circuits,
        };
        Some(
            snapshot
                .instances
                .iter()
                .map(|instance| {
                    let runtime = projection.project(instance);
                    (instance.id.clone(), runtime)
                })
                .collect(),
        )
    }

    async fn activate_state(
        &self,
        prepared: &ExtensionSetReference,
        instance: &PluginInstance,
    ) -> Result<(), AdminError> {
        if !instance.enabled {
            return Ok(());
        }
        let set = self.prepared_set(prepared).await?;
        let prepared = set
            .sessions
            .iter()
            .find(|prepared| {
                prepared.instance_id == instance.id
                    && prepared.artifact_sha256 == instance.artifact_sha256
            })
            .ok_or_else(|| AdminError::conflict("插件候选实例已失效"))?;
        let owner = self
            .state
            .load_owner(prepared.private_state.owner_request(instance))
            .await
            .map_err(map_state_admin_error)?
            .ok_or_else(|| AdminError::conflict("插件状态 fence 尚未提交"))?;
        prepared.private_state.activate(owner);
        Ok(())
    }

    async fn quiesce_instance(&self, instance_id: &str, artifact_sha256: &str, revision: Revision) {
        let mut sessions = Vec::new();
        for set in self
            .prepared
            .lock()
            .await
            .values()
            .filter_map(Weak::upgrade)
        {
            for prepared in &set.sessions {
                if prepared.instance_id == instance_id
                    && prepared.artifact_sha256 == artifact_sha256
                    && prepared.revision == revision
                    && !sessions
                        .iter()
                        .any(|session| Arc::ptr_eq(session, &prepared.session))
                {
                    sessions.push(prepared.session.clone());
                }
            }
        }
        shutdown_sessions(sessions, Duration::from_secs(2)).await;
    }

    async fn migrate_state(
        &self,
        prepared: &ExtensionSetReference,
        transition: PluginStateTransition,
    ) -> Result<(), AdminError> {
        self.run_state_migration(prepared, transition).await
    }
}

struct InstanceRuntimeProjection<'a> {
    target_revision: Revision,
    published_revision: Option<u64>,
    published_ready: bool,
    published_set: Option<&'a PreparedSet>,
    sets: &'a [Arc<PreparedSet>],
    preparation: Option<&'a PreparationDiagnostic>,
    retained: &'a BTreeMap<String, u32>,
    open_restart_circuits: &'a BTreeSet<String>,
}

impl InstanceRuntimeProjection<'_> {
    fn project(&self, expected: &PluginInstance) -> PluginInstanceRuntime {
        let active = self.published_set.and_then(|set| {
            set.sessions
                .iter()
                .find(|instance| instance.instance_id == expected.id)
        });
        let actual_matches = active.is_some_and(|instance| {
            instance.revision == expected.revision
                && instance.artifact_sha256 == expected.artifact_sha256
        });
        let target_is_published = self.published_revision == Some(self.target_revision.get());
        let candidate_set = match self.preparation {
            Some(PreparationDiagnostic::Prepared { set_id }) => Some(set_id.as_str()),
            _ => None,
        };
        let published_set_id = self.published_set.map(|set| set.id.as_str());
        let mut draining_revisions = BTreeSet::new();
        for set in self.sets {
            if Some(set.id.as_str()) == published_set_id || Some(set.id.as_str()) == candidate_set {
                continue;
            }
            if let Some(instance) = set
                .sessions
                .iter()
                .find(|instance| instance.instance_id == expected.id)
                && instance.revision.get() < expected.revision.get()
            {
                draining_revisions.insert(instance.revision.get());
            }
        }
        if !expected.enabled
            && let Some(instance) = active
            && instance.revision != expected.revision
        {
            draining_revisions.insert(instance.revision.get());
        }

        let retained_continuations = self.retained.get(&expected.id).copied().unwrap_or_default();
        let diagnostic = active.map(|instance| instance.session.diagnostic());
        let mut failure = None;
        let status = if !expected.enabled {
            if active.is_some() || !draining_revisions.is_empty() || retained_continuations > 0 {
                PluginInstanceRuntimeStatus::Draining
            } else {
                PluginInstanceRuntimeStatus::Disabled
            }
        } else if target_is_published
            && let Some(reason) = self
                .published_set
                .and_then(|set| set.failures.get(&expected.id))
        {
            failure = Some(reason.clone());
            PluginInstanceRuntimeStatus::PreparationFailed
        } else if self.open_restart_circuits.contains(&expected.id) {
            failure = Some(runtime_failure(
                "restart_circuit_open",
                "插件实例连续异常退出，已暂停自动重启",
            ));
            PluginInstanceRuntimeStatus::Faulted
        } else if actual_matches
            && let Some(crate::rpc::RpcSessionDiagnostic::Failed { code, message }) = diagnostic
        {
            failure = Some(runtime_failure(code, message));
            PluginInstanceRuntimeStatus::Faulted
        } else if !target_is_published {
            match self.preparation {
                Some(PreparationDiagnostic::Preparing) => PluginInstanceRuntimeStatus::Preparing,
                Some(PreparationDiagnostic::Failed(reason)) => {
                    failure = Some(reason.clone());
                    PluginInstanceRuntimeStatus::PreparationFailed
                }
                Some(PreparationDiagnostic::Prepared { .. }) | None => {
                    PluginInstanceRuntimeStatus::AwaitingPublication
                }
            }
        } else if !actual_matches {
            failure = Some(runtime_failure(
                "published_instance_mismatch",
                "已发布插件实例与期望版本不一致",
            ));
            PluginInstanceRuntimeStatus::Blocked
        } else {
            match diagnostic {
                Some(crate::rpc::RpcSessionDiagnostic::Ready) if self.published_ready => {
                    PluginInstanceRuntimeStatus::Running
                }
                Some(crate::rpc::RpcSessionDiagnostic::Ready) => {
                    failure = Some(runtime_failure(
                        "published_set_unready",
                        "同一发布集合中存在未就绪插件实例",
                    ));
                    PluginInstanceRuntimeStatus::Blocked
                }
                Some(crate::rpc::RpcSessionDiagnostic::Quiescing) => {
                    PluginInstanceRuntimeStatus::Draining
                }
                Some(crate::rpc::RpcSessionDiagnostic::Failed { code, message }) => {
                    failure = Some(runtime_failure(code, message));
                    PluginInstanceRuntimeStatus::Faulted
                }
                None => {
                    failure = Some(runtime_failure(
                        "instance_not_published",
                        "已发布集合缺少该插件实例",
                    ));
                    PluginInstanceRuntimeStatus::Blocked
                }
            }
        };

        PluginInstanceRuntime {
            status,
            actual_revision: active.map(|instance| instance.revision.get()),
            actual_artifact_sha256: active.map(|instance| instance.artifact_sha256.clone()),
            failure,
            draining_revisions: draining_revisions.into_iter().collect(),
            retained_continuations,
        }
    }
}

fn runtime_failure(code: &str, message: &str) -> PluginInstanceRuntimeFailure {
    PluginInstanceRuntimeFailure {
        code: code.to_owned(),
        message: message.to_owned(),
    }
}

fn runtime_failure_from_admin(error: &AdminError) -> PluginInstanceRuntimeFailure {
    let code = match error.kind() {
        AdminErrorKind::Invalid => "invalid",
        AdminErrorKind::Unauthorized => "unauthorized",
        AdminErrorKind::Forbidden => "forbidden",
        AdminErrorKind::NotFound => "not_found",
        AdminErrorKind::Conflict => "conflict",
        AdminErrorKind::RateLimited => "rate_limited",
        AdminErrorKind::BadGateway => "bad_gateway",
        AdminErrorKind::UpstreamResultUnknown => "upstream_result_unknown",
        AdminErrorKind::Unavailable => "unavailable",
        AdminErrorKind::Internal => "internal",
    };
    runtime_failure(code, error.message())
}

async fn shutdown_sessions(sessions: Vec<Arc<RpcSession>>, grace: Duration) {
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

fn map_state_admin_error(
    error: gateway_admin::ports::plugins::PluginStateStoreError,
) -> AdminError {
    match error.kind() {
        PluginStateStoreErrorKind::Invalid => AdminError::invalid("插件状态请求不合法"),
        PluginStateStoreErrorKind::NotFound => AdminError::not_found("插件状态迁移不存在"),
        PluginStateStoreErrorKind::Conflict => AdminError::conflict("插件状态版本或迁移游标冲突"),
        PluginStateStoreErrorKind::Quota => AdminError::conflict("插件状态超过声明配额"),
        PluginStateStoreErrorKind::PermissionDenied => {
            AdminError::conflict("插件状态 fence 已失效")
        }
        PluginStateStoreErrorKind::Unavailable => AdminError::unavailable("插件状态存储暂不可用"),
    }
}

impl ExtensionPreparationPort for PluginRuntime {
    fn prepare(
        &self,
        revision: ConfigRevision,
    ) -> BoxFuture<'_, Result<ExtensionSetReference, ExtensionPreparationError>> {
        Box::pin(async move {
            let snapshot = self
                .store
                .load_instances()
                .await
                .map_err(|_| ExtensionPreparationError)?;
            if snapshot.config_revision.get() != revision.get() {
                return Err(ExtensionPreparationError);
            }
            self.prepare_snapshot(snapshot.config_revision, snapshot, None)
                .await
                .map_err(|_| ExtensionPreparationError)
        })
    }
}

fn fingerprint(snapshot: &PluginInstanceSnapshot) -> Result<String, AdminError> {
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

fn instance_fingerprint(
    instance: &gateway_admin::model::plugins::instances::PluginInstance,
) -> Result<String, AdminError> {
    let secrets: BTreeMap<_, _> = instance
        .secrets
        .iter()
        .map(|(name, value)| (name, value.expose_secret()))
        .collect();
    let mut value = serde_json::json!({"id":instance.id,"artifact":instance.artifact_sha256,"revision":instance.revision.get(),"trusted":instance.trusted_process,"configuration":instance.configuration,"secrets":secrets,"grants":instance.grants,"bindings":instance.bindings});
    // JSONB 恢复可以改变对象键顺序，不能因此替换已经准备好的同一配置。
    value.sort_all_objects();
    let bytes = serde_json::to_vec(&value).map_err(|_| AdminError::invalid("插件配置无法编码"))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn continuation_authorization_fingerprint(
    instance: &gateway_admin::model::plugins::instances::PluginInstance,
    plugin_id: &str,
    provider_id: &str,
) -> Result<String, AdminError> {
    let secret_digests: BTreeMap<_, _> = instance
        .secrets
        .iter()
        .map(|(name, value)| {
            (
                name,
                hex::encode(Sha256::digest(value.expose_secret().as_bytes())),
            )
        })
        .collect();
    let mut value = serde_json::json!({
        "plugin": plugin_id,
        "instance": instance.id,
        "provider": provider_id,
        "trusted": instance.trusted_process,
        "configuration": instance.configuration,
        "secret_digests": secret_digests,
        "grants": instance.grants,
        "bindings": instance.bindings,
    });
    value.sort_all_objects();
    let bytes =
        serde_json::to_vec(&value).map_err(|_| AdminError::invalid("插件续写授权无法编码"))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}
