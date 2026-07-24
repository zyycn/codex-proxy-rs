//! RuntimeSnapshot 事实、编译、原子发布与版本收敛规则。

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use futures::future::BoxFuture;
use futures::{FutureExt as _, Stream, StreamExt as _, pin_mut, select_biased};
use futures_timer::Delay;

use crate::engine::CancellationToken;
use crate::engine::credential::{AccountSelectionPolicy, RotationStrategy};
use crate::engine::provider::{ProviderCatalogGeneration, ProviderRegistry};
use crate::error::RoutingError;
use crate::health::{HealthProbe, HealthState};
use crate::operation::Operation;
use crate::policy::{ClientApiKeyId, ClientPolicy, PlaintextClientApiKey, RateLimits};
use crate::task::{
    DaemonRestartPolicy, DaemonTask, ScheduledTask, WorkerContribution, WorkerCycleContext,
    WorkerDefinitionError, WorkerId, WorkerKind, WorkerRegistration, WorkerRunnable,
    WorkerSchedule, WorkerTaskError,
};

use super::{
    ConfigRevision, ModelCapabilities, ProviderCandidate, ProviderKind, ProviderModel,
    PublicModelId, RoutingContext, RoutingPlan, UpstreamModelId,
};

const RECONCILIATION_INTERVAL: Duration = Duration::from_secs(5);
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAXIMUM_BACKOFF: Duration = Duration::from_secs(30);
const UNUSED_LEASE_TTL: Duration = Duration::from_secs(30);
const UNUSED_LEASE_RENEWAL: Duration = Duration::from_secs(10);
const MAXIMUM_CATALOG_STABILITY_ATTEMPTS: usize = 4;

/// Store 在一个一致性读取中提供的调度设置事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotSettingsFacts {
    max_concurrent_per_account: u32,
    request_interval_ms: u64,
    rotation_strategy: String,
    model_mappings: BTreeMap<String, String>,
}

impl SnapshotSettingsFacts {
    #[must_use]
    pub fn new(
        max_concurrent_per_account: u32,
        request_interval_ms: u64,
        rotation_strategy: impl Into<String>,
        model_mappings: BTreeMap<String, String>,
    ) -> Self {
        Self {
            max_concurrent_per_account,
            request_interval_ms,
            rotation_strategy: rotation_strategy.into(),
            model_mappings,
        }
    }
}

/// Store 读取到的一个启用 Client API Key 策略事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotClientPolicyFacts {
    key_id: ClientApiKeyId,
    plaintext_key: PlaintextClientApiKey,
    provider_kind: String,
    limits: RateLimits,
}

impl SnapshotClientPolicyFacts {
    #[must_use]
    pub fn new(
        key_id: ClientApiKeyId,
        plaintext_key: PlaintextClientApiKey,
        provider_kind: impl Into<String>,
        limits: RateLimits,
    ) -> Self {
        Self {
            key_id,
            plaintext_key,
            provider_kind: provider_kind.into(),
            limits,
        }
    }
}

/// 一次一致性读取产生的全部 RuntimeSnapshot 持久事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotFacts {
    config_revision: ConfigRevision,
    observed_current_revision: ConfigRevision,
    settings: SnapshotSettingsFacts,
    client_policies: Vec<SnapshotClientPolicyFacts>,
}

impl SnapshotFacts {
    #[must_use]
    pub fn new(
        config_revision: ConfigRevision,
        observed_current_revision: ConfigRevision,
        settings: SnapshotSettingsFacts,
        client_policies: Vec<SnapshotClientPolicyFacts>,
    ) -> Self {
        Self {
            config_revision,
            observed_current_revision,
            settings,
            client_policies,
        }
    }

    #[must_use]
    pub const fn config_revision(&self) -> ConfigRevision {
        self.config_revision
    }

    #[must_use]
    pub const fn observed_current_revision(&self) -> ConfigRevision {
        self.observed_current_revision
    }
}

/// 不泄漏持久化实现细节的 Snapshot store 错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("runtime snapshot store is unavailable")]
pub struct SnapshotStoreError;

impl SnapshotStoreError {
    #[must_use]
    pub const fn unavailable() -> Self {
        Self
    }
}

/// 不泄漏订阅基础设施细节的通知错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("runtime snapshot notification is unavailable")]
pub struct SnapshotSubscriptionError;

impl SnapshotSubscriptionError {
    #[must_use]
    pub const fn unavailable() -> Self {
        Self
    }
}

/// 可丢失的配置 revision 通知流；权威 revision 始终由 Store 端口读取。
pub type SnapshotRevisionStream =
    Pin<Box<dyn Stream<Item = Result<ConfigRevision, SnapshotSubscriptionError>> + Send + 'static>>;

/// RuntimeSnapshot 持久事实的数据库中立端口。
pub trait SnapshotStorePort: Send + Sync {
    fn load_snapshot_facts(&self) -> BoxFuture<'_, Result<SnapshotFacts, SnapshotStoreError>>;

    fn current_config_revision(&self) -> BoxFuture<'_, Result<ConfigRevision, SnapshotStoreError>>;
}

/// 跨进程 revision 通知的基础设施中立端口。
pub trait SnapshotSubscriptionPort: Send + Sync {
    fn publish_snapshot_revision(
        &self,
        revision: ConfigRevision,
    ) -> BoxFuture<'_, Result<(), SnapshotSubscriptionError>>;

    fn subscribe_snapshot_revisions(
        &self,
    ) -> BoxFuture<'_, Result<SnapshotRevisionStream, SnapshotSubscriptionError>>;
}

/// 快照未发布时可安全记录的稳定错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RuntimeSnapshotCompileError {
    #[error("runtime snapshot store is unavailable")]
    StoreUnavailable,
    #[error("runtime configuration changed while the snapshot was loading")]
    RevisionChanged,
    #[error("runtime snapshot contains invalid frozen data")]
    InvalidData,
    #[error("provider model catalog changed while the snapshot was compiling")]
    CatalogChanged,
}

/// Store 一致性事实与 Provider 实时目录的唯一快照编译器。
#[derive(Clone)]
pub struct RuntimeSnapshotCompiler {
    store: Arc<dyn SnapshotStorePort>,
    providers: ProviderRegistry,
}

impl RuntimeSnapshotCompiler {
    #[must_use]
    pub const fn new(store: Arc<dyn SnapshotStorePort>, providers: ProviderRegistry) -> Self {
        Self { store, providers }
    }

    /// 读取一个 revision，并为已注册 Provider 查询实时模型目录。
    pub async fn compile(&self) -> Result<RuntimeSnapshot, RuntimeSnapshotCompileError> {
        for _ in 0..MAXIMUM_CATALOG_STABILITY_ATTEMPTS {
            let catalog_generations = self.providers.catalog_generations();
            let facts = self
                .store
                .load_snapshot_facts()
                .await
                .map_err(|_| RuntimeSnapshotCompileError::StoreUnavailable)?;
            if facts.config_revision != facts.observed_current_revision {
                return Err(RuntimeSnapshotCompileError::RevisionChanged);
            }
            let snapshot = compile_runtime_snapshot(facts, &self.providers).await?;
            let observed_generations = self.providers.catalog_generations();
            if catalog_generations == observed_generations {
                return Ok(snapshot.with_provider_catalog_generations(observed_generations));
            }
        }
        Err(RuntimeSnapshotCompileError::CatalogChanged)
    }
}

async fn compile_runtime_snapshot(
    facts: SnapshotFacts,
    providers: &ProviderRegistry,
) -> Result<RuntimeSnapshot, RuntimeSnapshotCompileError> {
    let provider_kinds = providers.provider_kinds().cloned().collect::<Vec<_>>();

    // 实时目录只提供公开模型与能力提示；查询失败时保留 Provider 透传语义。
    let mut provider_models = Vec::new();
    for provider in &provider_kinds {
        let Ok(models) = providers.query_model_capabilities(provider).await else {
            continue;
        };
        provider_models.extend(models.into_iter().map(|model| {
            ProviderModel::new(
                provider.clone(),
                model.upstream_model().clone(),
                model.capabilities().clone(),
            )
        }));
    }

    let model_mappings = facts.settings.model_mappings;
    let rotation_strategy = match facts.settings.rotation_strategy.as_str() {
        "smart" => RotationStrategy::Smart,
        "quota_reset_priority" => RotationStrategy::QuotaResetPriority,
        "round_robin" => RotationStrategy::RoundRobin,
        "sticky" => RotationStrategy::Sticky,
        _ => return Err(RuntimeSnapshotCompileError::InvalidData),
    };
    let selection_policy = AccountSelectionPolicy::new(
        rotation_strategy,
        NonZeroU32::new(facts.settings.max_concurrent_per_account)
            .ok_or(RuntimeSnapshotCompileError::InvalidData)?,
        Duration::from_millis(facts.settings.request_interval_ms),
    );
    let client_policies = facts
        .client_policies
        .into_iter()
        .map(|policy| {
            let provider = ProviderKind::new(policy.provider_kind)
                .map_err(|_| RuntimeSnapshotCompileError::InvalidData)?;
            Ok(ClientPolicy::new(
                policy.key_id,
                policy.plaintext_key,
                provider,
                true,
                policy.limits,
            ))
        })
        .collect::<Result<Vec<_>, RuntimeSnapshotCompileError>>()?;

    RuntimeSnapshot::new(
        facts.config_revision,
        selection_policy,
        provider_kinds,
        provider_models,
        client_policies,
    )
    .map_err(|_| RuntimeSnapshotCompileError::InvalidData)
    .map(|snapshot| snapshot.with_model_mappings(model_mappings))
}

/// 数据面使用的不可变配置快照。
#[derive(Debug, Clone)]
pub struct RuntimeSnapshot {
    revision: ConfigRevision,
    account_selection_policy: AccountSelectionPolicy,
    providers: Arc<BTreeSet<ProviderKind>>,
    provider_models: Arc<BTreeMap<ProviderKind, BTreeMap<UpstreamModelId, ModelCapabilities>>>,
    model_mappings: Arc<BTreeMap<String, String>>,
    provider_catalog_generations: Arc<BTreeMap<ProviderKind, ProviderCatalogGeneration>>,
    client_policies: Arc<BTreeMap<ClientApiKeyId, ClientPolicy>>,
}

impl RuntimeSnapshot {
    /// 校验 Provider、实时模型目录和 Client API Key，并构建快照。
    pub fn new(
        revision: ConfigRevision,
        account_selection_policy: AccountSelectionPolicy,
        providers: Vec<ProviderKind>,
        provider_models: Vec<ProviderModel>,
        client_policies: Vec<ClientPolicy>,
    ) -> Result<Self, RoutingError> {
        let mut provider_set = BTreeSet::new();
        for provider in providers {
            if !provider_set.insert(provider.clone()) {
                return Err(RoutingError::DuplicateEntity {
                    entity: "provider",
                    id: provider.to_string(),
                });
            }
        }

        let mut model_map =
            BTreeMap::<ProviderKind, BTreeMap<UpstreamModelId, ModelCapabilities>>::new();
        for model in provider_models {
            if !provider_set.contains(model.provider()) {
                return Err(RoutingError::NotFound {
                    entity: "provider",
                    id: model.provider().to_string(),
                });
            }
            let models = model_map.entry(model.provider).or_default();
            let upstream_model = model.upstream_model;
            if models
                .insert(upstream_model.clone(), model.capabilities)
                .is_some()
            {
                return Err(RoutingError::DuplicateEntity {
                    entity: "provider model",
                    id: upstream_model.to_string(),
                });
            }
        }

        let mut client_policy_map = BTreeMap::new();
        for policy in client_policies {
            let id = policy.key_id().clone();
            if client_policy_map.insert(id.clone(), policy).is_some() {
                return Err(RoutingError::DuplicateEntity {
                    entity: "client API key",
                    id: id.to_string(),
                });
            }
        }
        client_policy_map.retain(|_, policy| policy.enabled());

        Ok(Self {
            revision,
            account_selection_policy,
            providers: Arc::new(provider_set),
            provider_models: Arc::new(model_map),
            model_mappings: Arc::new(BTreeMap::new()),
            provider_catalog_generations: Arc::new(BTreeMap::new()),
            client_policies: Arc::new(client_policy_map),
        })
    }

    #[must_use]
    pub fn with_model_mappings(mut self, mappings: BTreeMap<String, String>) -> Self {
        self.model_mappings = Arc::new(mappings);
        self
    }

    #[must_use]
    fn with_provider_catalog_generations(
        mut self,
        generations: BTreeMap<ProviderKind, ProviderCatalogGeneration>,
    ) -> Self {
        self.provider_catalog_generations = Arc::new(generations);
        self
    }

    #[must_use]
    pub fn provider_catalog_generations(
        &self,
    ) -> &BTreeMap<ProviderKind, ProviderCatalogGeneration> {
        &self.provider_catalog_generations
    }

    #[must_use]
    pub const fn revision(&self) -> ConfigRevision {
        self.revision
    }

    /// 返回目录发现模型与设置映射的并集，仅用于公开模型展示。
    #[must_use]
    pub fn public_models_for_provider(&self, provider: &ProviderKind) -> Vec<PublicModelId> {
        let mut models = BTreeSet::new();
        if let Some(discovered) = self.provider_models.get(provider) {
            models.extend(
                discovered
                    .keys()
                    .filter_map(|model| PublicModelId::new(model.as_str().to_owned()).ok()),
            );
        }
        models.extend(
            self.model_mappings
                .keys()
                .filter_map(|model| PublicModelId::new(model.clone()).ok()),
        );
        models.into_iter().collect()
    }

    #[must_use]
    pub fn public_models(&self) -> Vec<PublicModelId> {
        self.providers
            .iter()
            .flat_map(|provider| self.public_models_for_provider(provider))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// 透明代理不以目录白名单拒绝模型；只要求该 Provider 已注册。
    #[must_use]
    pub fn contains_public_model_for_provider(
        &self,
        _public_model: &PublicModelId,
        provider: &ProviderKind,
    ) -> bool {
        self.providers.contains(provider)
    }

    #[must_use]
    pub fn mapped_model(&self, requested: &str) -> String {
        self.model_mappings
            .get(requested)
            .cloned()
            .unwrap_or_else(|| requested.to_owned())
    }

    pub fn client_policies(&self) -> impl Iterator<Item = &ClientPolicy> {
        self.client_policies.values()
    }

    pub fn plan(
        &self,
        public_model: &PublicModelId,
        operation: &Operation,
        context: &RoutingContext,
    ) -> Result<RoutingPlan, RoutingError> {
        let requirements = operation.capability_requirements();
        let mut candidates = Vec::new();

        for provider in self.providers.iter() {
            if context
                .provider_kind
                .as_ref()
                .is_some_and(|expected| expected != provider)
                || context.blocked_providers.contains(provider)
            {
                continue;
            }
            let upstream_model = UpstreamModelId::new(self.mapped_model(public_model.as_str()))
                .map_err(|_| RoutingError::InvalidIdentifier)?;
            let emulated_features = match self
                .provider_models
                .get(provider)
                .and_then(|models| models.get(&upstream_model))
            {
                Some(capabilities) => {
                    let Some(emulated) = capabilities.match_requirements(&requirements) else {
                        continue;
                    };
                    emulated
                }
                None => BTreeSet::new(),
            };
            candidates.push(ProviderCandidate {
                provider: provider.clone(),
                upstream_model,
                emulated_features,
            });
        }

        if candidates.is_empty() {
            return Err(RoutingError::NoCapableProvider {
                model: public_model.as_str().to_owned(),
            });
        }

        Ok(RoutingPlan {
            config_revision: self.revision,
            account_selection_policy: self.account_selection_policy,
            public_model: public_model.clone(),
            operation: operation.kind(),
            max_attempts: NonZeroU32::new(super::MAX_REQUEST_ATTEMPTS)
                .expect("constant request attempt limit is non-zero"),
            candidates: Arc::from(candidates),
        })
    }
}

/// 请求级冻结失败；此状态必须 fail closed。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("runtime snapshot is unavailable")]
pub struct RuntimeSnapshotUnavailable;

/// RuntimeSnapshot 原子发布和请求级冻结句柄。
#[derive(Clone, Default)]
pub struct RuntimeSnapshotHandle {
    current: Arc<RwLock<Option<Arc<RuntimeSnapshot>>>>,
}

impl RuntimeSnapshotHandle {
    #[must_use]
    pub fn new(initial: RuntimeSnapshot) -> Self {
        Self {
            current: Arc::new(RwLock::new(Some(Arc::new(initial)))),
        }
    }

    pub fn publish(&self, snapshot: RuntimeSnapshot) {
        *write_unpoisoned(&self.current) = Some(Arc::new(snapshot));
    }

    pub fn suspend(&self) {
        *write_unpoisoned(&self.current) = None;
    }

    #[must_use]
    pub fn revision(&self) -> Option<ConfigRevision> {
        read_unpoisoned(&self.current)
            .as_ref()
            .map(|snapshot| snapshot.revision())
    }

    #[must_use]
    pub fn provider_catalog_generations(
        &self,
    ) -> Option<BTreeMap<ProviderKind, ProviderCatalogGeneration>> {
        read_unpoisoned(&self.current)
            .as_ref()
            .map(|snapshot| snapshot.provider_catalog_generations().clone())
    }

    /// 冻结当前 Arc；后续发布不改变已经开始的请求。
    pub fn acquire(&self) -> Result<Arc<RuntimeSnapshot>, RuntimeSnapshotUnavailable> {
        read_unpoisoned(&self.current)
            .clone()
            .ok_or(RuntimeSnapshotUnavailable)
    }
}

impl HealthProbe for RuntimeSnapshotHandle {
    fn name(&self) -> &'static str {
        "runtime_snapshot"
    }

    fn check(&self) -> BoxFuture<'_, HealthState> {
        Box::pin(async move {
            if self.revision().is_some() {
                HealthState::Healthy
            } else {
                HealthState::Unhealthy("Runtime snapshot is unavailable".to_owned())
            }
        })
    }
}

/// Admin 提交配置后触发本进程刷新与跨进程通知的对象安全端口。
pub trait SnapshotControl: Send + Sync {
    fn publish_committed(&self, committed_revision: ConfigRevision) -> BoxFuture<'_, ()>;
}

/// 配置提交后的本进程快照发布与跨进程失效通知。
#[derive(Clone)]
pub struct RuntimeSnapshotPublisher {
    compiler: Arc<RuntimeSnapshotCompiler>,
    snapshots: RuntimeSnapshotHandle,
    subscriptions: Arc<dyn SnapshotSubscriptionPort>,
}

impl RuntimeSnapshotPublisher {
    #[must_use]
    pub const fn new(
        compiler: Arc<RuntimeSnapshotCompiler>,
        snapshots: RuntimeSnapshotHandle,
        subscriptions: Arc<dyn SnapshotSubscriptionPort>,
    ) -> Self {
        Self {
            compiler,
            snapshots,
            subscriptions,
        }
    }

    /// 重新编译并原子替换本进程快照。
    pub async fn refresh(&self) -> Result<ConfigRevision, RuntimeSnapshotCompileError> {
        let snapshot = self.compiler.compile().await?;
        let revision = snapshot.revision();
        self.snapshots.publish(snapshot);
        Ok(revision)
    }

    pub fn suspend(&self) {
        self.snapshots.suspend();
    }

    #[must_use]
    pub fn published_revision(&self) -> Option<ConfigRevision> {
        self.snapshots.revision()
    }

    #[must_use]
    fn provider_catalogs_need_refresh(&self) -> bool {
        self.snapshots.provider_catalog_generations().as_ref()
            != Some(&self.compiler.providers.catalog_generations())
    }

    /// 数据库提交不能被目录或通知基础设施的暂时故障伪装成回滚。
    async fn publish_committed_inner(&self, committed_revision: ConfigRevision) {
        if self.refresh().await.is_err() {
            self.suspend();
        }
        let _ = self
            .subscriptions
            .publish_snapshot_revision(committed_revision)
            .await;
    }

    /// 交给 Host 的周期对账与长驻订阅任务。
    pub fn worker_contributions(&self) -> Result<Vec<WorkerContribution>, WorkerDefinitionError> {
        let reconciliation_id = WorkerId::try_new(
            WorkerKind::RuntimeSnapshotReconciliation,
            "runtime_snapshot",
        )?;
        let schedule = WorkerSchedule::try_new(
            RECONCILIATION_INTERVAL,
            INITIAL_BACKOFF,
            MAXIMUM_BACKOFF,
            UNUSED_LEASE_TTL,
            UNUSED_LEASE_RENEWAL,
        )?;
        let reconciliation = WorkerRegistration::try_new(
            reconciliation_id,
            WorkerRunnable::Scheduled {
                schedule,
                lease: None,
                task: Box::new(RuntimeSnapshotReconciliationTask {
                    store: Arc::clone(&self.compiler.store),
                    publisher: self.clone(),
                }),
            },
        )?;
        let subscription_id =
            WorkerId::try_new(WorkerKind::RuntimeChangeSubscription, "runtime_snapshot")?;
        let restart = DaemonRestartPolicy::try_new(INITIAL_BACKOFF, MAXIMUM_BACKOFF)?;
        let subscription = WorkerRegistration::try_new(
            subscription_id,
            WorkerRunnable::Daemon {
                restart,
                task: Box::new(RuntimeSnapshotSubscriptionTask {
                    subscriptions: Arc::clone(&self.subscriptions),
                    publisher: self.clone(),
                }),
            },
        )?;
        Ok(vec![
            WorkerContribution::Registration(reconciliation),
            WorkerContribution::Registration(subscription),
        ])
    }
}

impl SnapshotControl for RuntimeSnapshotPublisher {
    fn publish_committed(&self, committed_revision: ConfigRevision) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.publish_committed_inner(committed_revision).await;
        })
    }
}

struct RuntimeSnapshotReconciliationTask {
    store: Arc<dyn SnapshotStorePort>,
    publisher: RuntimeSnapshotPublisher,
}

impl ScheduledTask for RuntimeSnapshotReconciliationTask {
    fn run_cycle(
        &self,
        _context: WorkerCycleContext,
    ) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            let persisted_revision = match self.store.current_config_revision().await {
                Ok(revision) => revision,
                Err(_) => {
                    self.publisher.suspend();
                    return Err(WorkerTaskError::safe(
                        "runtime snapshot revision is unavailable",
                    ));
                }
            };
            let configuration_changed = runtime_revision_needs_refresh(
                self.publisher.published_revision().map(ConfigRevision::get),
                persisted_revision.get(),
            );
            if !configuration_changed && !self.publisher.provider_catalogs_need_refresh() {
                return Ok(());
            }
            if self.publisher.refresh().await.is_err() {
                // 已提交的配置无法确认时必须 fail closed；仅目录重编译失败时继续
                // 服务旧的不可变快照，单调代次会让下一周期再次尝试。
                if configuration_changed {
                    self.publisher.suspend();
                }
                return Err(WorkerTaskError::safe(
                    "runtime snapshot reconciliation failed",
                ));
            }
            Ok(())
        })
    }
}

struct RuntimeSnapshotSubscriptionTask {
    subscriptions: Arc<dyn SnapshotSubscriptionPort>,
    publisher: RuntimeSnapshotPublisher,
}

impl DaemonTask for RuntimeSnapshotSubscriptionTask {
    fn run(&self, cancellation: CancellationToken) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            let mut retry_delay = INITIAL_BACKOFF;
            loop {
                if cancellation.is_cancelled() {
                    return Ok(());
                }
                let subscription = self.subscriptions.subscribe_snapshot_revisions().await;
                let mut subscription = match subscription {
                    Ok(subscription) => {
                        retry_delay = INITIAL_BACKOFF;
                        subscription
                    }
                    Err(_) => {
                        wait_or_cancel(&cancellation, retry_delay).await;
                        retry_delay = (retry_delay * 2).min(MAXIMUM_BACKOFF);
                        continue;
                    }
                };
                loop {
                    let cancelled = cancellation.cancelled().fuse();
                    let next = subscription.next().fuse();
                    pin_mut!(cancelled, next);
                    let notified = select_biased! {
                        _ = cancelled => return Ok(()),
                        next = next => next,
                    };
                    match notified {
                        Some(Ok(_)) => {
                            if self.publisher.refresh().await.is_err() {
                                self.publisher.suspend();
                            }
                        }
                        Some(Err(_)) | None => break,
                    }
                }
            }
        })
    }
}

async fn wait_or_cancel(cancellation: &CancellationToken, duration: Duration) {
    let cancelled = cancellation.cancelled().fuse();
    let delay = Delay::new(duration).fuse();
    pin_mut!(cancelled, delay);
    select_biased! {
        _ = cancelled => {},
        _ = delay => {},
    }
}

/// 当前发布版本与持久版本不一致时必须重载；缺失和回退同样 fail closed。
#[must_use]
pub fn runtime_revision_needs_refresh(
    published_revision: Option<u64>,
    persisted_revision: u64,
) -> bool {
    published_revision != Some(persisted_revision)
}

fn read_unpoisoned<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn write_unpoisoned<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
