//! Provider 平台、实例、模型目录、精确模型映射与请求级候选计划。

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::num::{NonZeroU32, NonZeroU64};
use std::sync::Arc;

use crate::engine::credential::AccountSelectionPolicy;
use crate::error::{IdentifierError, RoutingError, validate_text};
use crate::operation::{CapabilityRequirements, Feature, Operation, OperationKind};
use crate::policy::{ClientApiKeyId, ClientPolicy};

const MAX_REQUEST_ATTEMPTS: u32 = 32;

/// 一个具体 Provider endpoint 的 ID。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderInstanceId(String);

impl ProviderInstanceId {
    /// 校验并创建 Provider instance ID。
    pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = value.into();
        validate_text(&value, 128, false, Some("inst_"))?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProviderInstanceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// 编译进二进制的 Provider adapter slug。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderKind(String);

impl ProviderKind {
    /// 校验 Provider slug。
    pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = value.into();
        validate_text(&value, 64, true, None)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// 客户端请求中的模型名称。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PublicModelId(String);

impl PublicModelId {
    pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = value.into();
        validate_text(&value, 256, true, None)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PublicModelId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Provider 实际接收的模型名称。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UpstreamModelId(String);

impl UpstreamModelId {
    pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = value.into();
        validate_text(&value, 256, true, None)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for UpstreamModelId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// `runtime_settings.config_revision`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConfigRevision(NonZeroU64);

impl ConfigRevision {
    pub fn new(value: u64) -> Result<Self, RoutingError> {
        NonZeroU64::new(value)
            .map(Self)
            .ok_or(RoutingError::InvalidRevision)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Provider 实时目录报告的能力支持等级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SupportLevel {
    Native,
    Emulated,
    Unsupported,
    Unknown,
}

/// Provider 实时模型目录中的能力事实；不落 PostgreSQL。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCapabilities {
    operations: BTreeSet<OperationKind>,
    features: BTreeMap<Feature, SupportLevel>,
    context_window_tokens: u64,
    max_output_tokens: Option<u64>,
}

impl ModelCapabilities {
    #[must_use]
    pub fn new(
        operations: BTreeSet<OperationKind>,
        context_window_tokens: u64,
        max_output_tokens: Option<u64>,
    ) -> Self {
        Self {
            operations,
            features: BTreeMap::new(),
            context_window_tokens,
            max_output_tokens,
        }
    }

    #[must_use]
    pub fn with_feature(mut self, feature: Feature, support: SupportLevel) -> Self {
        self.features.insert(feature, support);
        self
    }

    #[must_use]
    pub fn match_requirements(
        &self,
        requirements: &CapabilityRequirements,
    ) -> Option<BTreeSet<Feature>> {
        if !self.operations.contains(&requirements.operation())
            || self.context_window_tokens < requirements.minimum_context_tokens()
            || requirements
                .requested_output_tokens()
                .is_some_and(|requested| {
                    self.max_output_tokens
                        .is_some_and(|maximum| requested > maximum)
                })
        {
            return None;
        }

        let mut emulated = BTreeSet::new();
        for feature in requirements.features() {
            match self
                .features
                .get(feature)
                .copied()
                .unwrap_or(SupportLevel::Unknown)
            {
                SupportLevel::Native => {}
                SupportLevel::Emulated => {
                    emulated.insert(*feature);
                }
                SupportLevel::Unsupported | SupportLevel::Unknown => return None,
            }
        }
        Some(emulated)
    }
}

/// Provider instance 的可重建运行健康状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InstanceHealth {
    Healthy,
    Degraded,
    Unavailable,
    CircuitOpen,
    Saturated,
}

impl InstanceHealth {
    const fn is_routable(self, allow_degraded: bool) -> bool {
        matches!(self, Self::Healthy) || (allow_degraded && matches!(self, Self::Degraded))
    }
}

/// `provider_instances` 与实时健康状态组成的只读快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderInstance {
    id: ProviderInstanceId,
    provider: ProviderKind,
    base_url: String,
    enabled: bool,
    health: InstanceHealth,
}

impl ProviderInstance {
    #[must_use]
    pub const fn new(
        id: ProviderInstanceId,
        provider: ProviderKind,
        base_url: String,
        enabled: bool,
        health: InstanceHealth,
    ) -> Self {
        Self {
            id,
            provider,
            base_url,
            enabled,
            health,
        }
    }

    #[must_use]
    pub const fn id(&self) -> &ProviderInstanceId {
        &self.id
    }

    #[must_use]
    pub const fn provider(&self) -> &ProviderKind {
        &self.provider
    }

    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    #[must_use]
    pub const fn health(&self) -> InstanceHealth {
        self.health
    }
}

/// 一个 instance 实时发现的上游模型能力。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderModel {
    instance: ProviderInstanceId,
    upstream_model: UpstreamModelId,
    capabilities: ModelCapabilities,
}

impl ProviderModel {
    #[must_use]
    pub const fn new(
        instance: ProviderInstanceId,
        upstream_model: UpstreamModelId,
        capabilities: ModelCapabilities,
    ) -> Self {
        Self {
            instance,
            upstream_model,
            capabilities,
        }
    }

    #[must_use]
    pub const fn instance(&self) -> &ProviderInstanceId {
        &self.instance
    }

    #[must_use]
    pub const fn upstream_model(&self) -> &UpstreamModelId {
        &self.upstream_model
    }
}

/// Provider circuit 的 request-scoped observation fence。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderInstanceObservationToken {
    pub epoch: u64,
    pub fencing_token: u64,
    pub probe_fencing_token: Option<u64>,
}

/// 本次请求选择 Provider instance 时使用的动态过滤事实。
#[derive(Debug, Clone, Default)]
pub struct RoutingContext {
    /// 已认证 Client API Key 绑定的平台；模型名称不参与平台猜测。
    pub provider_kind: Option<ProviderKind>,
    pub allowed_instances: Option<BTreeSet<ProviderInstanceId>>,
    pub allow_degraded: bool,
    pub blocked_instances: BTreeSet<ProviderInstanceId>,
    pub provider_observation_tokens: BTreeMap<ProviderInstanceId, ProviderInstanceObservationToken>,
}

/// 已绑定 Provider、instance 与真实上游模型的请求候选。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCandidate {
    instance: ProviderInstance,
    upstream_model: UpstreamModelId,
    emulated_features: BTreeSet<Feature>,
    observation_token: Option<ProviderInstanceObservationToken>,
}

impl ProviderCandidate {
    #[must_use]
    pub const fn instance_snapshot(&self) -> &ProviderInstance {
        &self.instance
    }

    #[must_use]
    pub const fn instance(&self) -> &ProviderInstanceId {
        self.instance.id()
    }

    #[must_use]
    pub const fn provider(&self) -> &ProviderKind {
        self.instance.provider()
    }

    #[must_use]
    pub const fn upstream_model(&self) -> &UpstreamModelId {
        &self.upstream_model
    }

    #[must_use]
    pub const fn emulated_features(&self) -> &BTreeSet<Feature> {
        &self.emulated_features
    }

    #[must_use]
    pub const fn observation_token(&self) -> Option<ProviderInstanceObservationToken> {
        self.observation_token
    }
}

/// 一次请求冻结的平台内 Provider instance 尝试顺序。
#[derive(Debug, Clone)]
pub struct RoutingPlan {
    config_revision: ConfigRevision,
    account_selection_policy: AccountSelectionPolicy,
    public_model: PublicModelId,
    operation: OperationKind,
    max_attempts: NonZeroU32,
    candidates: Arc<[ProviderCandidate]>,
}

impl RoutingPlan {
    /// 冻结本请求允许的最大上游调用次数。
    #[must_use]
    pub fn with_max_attempts(mut self, max_attempts: NonZeroU32) -> Self {
        self.max_attempts = max_attempts;
        self
    }

    #[must_use]
    pub const fn config_revision(&self) -> ConfigRevision {
        self.config_revision
    }

    #[must_use]
    pub const fn account_selection_policy(&self) -> AccountSelectionPolicy {
        self.account_selection_policy
    }

    #[must_use]
    pub const fn public_model(&self) -> &PublicModelId {
        &self.public_model
    }

    #[must_use]
    pub const fn operation(&self) -> OperationKind {
        self.operation
    }

    #[must_use]
    pub const fn max_attempts(&self) -> NonZeroU32 {
        self.max_attempts
    }

    #[must_use]
    pub fn candidates(&self) -> &[ProviderCandidate] {
        &self.candidates
    }
}

/// 数据面使用的不可变配置快照。
#[derive(Debug, Clone)]
pub struct RuntimeSnapshot {
    revision: ConfigRevision,
    account_selection_policy: AccountSelectionPolicy,
    instances: Arc<BTreeMap<ProviderInstanceId, ProviderInstance>>,
    provider_models:
        Arc<BTreeMap<ProviderInstanceId, BTreeMap<UpstreamModelId, ModelCapabilities>>>,
    provider_model_mappings: Arc<BTreeMap<ProviderKind, BTreeMap<String, String>>>,
    client_policies: Arc<BTreeMap<ClientApiKeyId, ClientPolicy>>,
}

impl RuntimeSnapshot {
    /// 校验 instance、实时模型目录和 Client API Key，并构建快照。
    pub fn new(
        revision: ConfigRevision,
        account_selection_policy: AccountSelectionPolicy,
        instances: Vec<ProviderInstance>,
        provider_models: Vec<ProviderModel>,
        client_policies: Vec<ClientPolicy>,
    ) -> Result<Self, RoutingError> {
        let mut instance_map = BTreeMap::new();
        for instance in instances {
            let id = instance.id().clone();
            if instance_map.insert(id.clone(), instance).is_some() {
                return Err(RoutingError::DuplicateEntity {
                    entity: "provider instance",
                    id: id.to_string(),
                });
            }
        }

        let mut model_map =
            BTreeMap::<ProviderInstanceId, BTreeMap<UpstreamModelId, ModelCapabilities>>::new();
        for model in provider_models {
            if !instance_map.contains_key(model.instance()) {
                return Err(RoutingError::NotFound {
                    entity: "provider instance",
                    id: model.instance().to_string(),
                });
            }
            let models = model_map.entry(model.instance).or_default();
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
            instances: Arc::new(instance_map),
            provider_models: Arc::new(model_map),
            provider_model_mappings: Arc::new(BTreeMap::new()),
            client_policies: Arc::new(client_policy_map),
        })
    }

    #[must_use]
    pub fn with_provider_model_mappings(
        mut self,
        mappings: BTreeMap<ProviderKind, BTreeMap<String, String>>,
    ) -> Self {
        self.provider_model_mappings = Arc::new(mappings);
        self
    }

    #[must_use]
    pub const fn revision(&self) -> ConfigRevision {
        self.revision
    }

    /// 返回目录发现模型与设置别名的并集，仅用于 `/v1/models` 展示。
    #[must_use]
    pub fn public_models_for_provider(&self, provider: &ProviderKind) -> Vec<PublicModelId> {
        let mut models = BTreeSet::new();
        for (instance_id, discovered) in self.provider_models.iter() {
            let Some(instance) = self.instances.get(instance_id) else {
                continue;
            };
            if !instance.enabled || instance.provider() != provider {
                continue;
            }
            models.extend(
                discovered
                    .keys()
                    .filter_map(|model| PublicModelId::new(model.as_str().to_owned()).ok()),
            );
        }
        if let Some(mappings) = self.provider_model_mappings.get(provider) {
            models.extend(
                mappings
                    .keys()
                    .filter_map(|model| PublicModelId::new(model.clone()).ok()),
            );
        }
        models.into_iter().collect()
    }

    #[must_use]
    pub fn public_models(&self) -> Vec<PublicModelId> {
        let providers = self
            .instances
            .values()
            .map(|instance| instance.provider().clone())
            .collect::<BTreeSet<_>>();
        providers
            .iter()
            .flat_map(|provider| self.public_models_for_provider(provider))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// 透明代理不以目录白名单拒绝模型；只要求该平台至少有一个启用 instance。
    #[must_use]
    pub fn contains_public_model_for_provider(
        &self,
        _public_model: &PublicModelId,
        provider: &ProviderKind,
    ) -> bool {
        self.instances
            .values()
            .any(|instance| instance.enabled && instance.provider() == provider)
    }

    #[must_use]
    pub fn instance_ids_for_provider(
        &self,
        provider: &ProviderKind,
    ) -> BTreeSet<ProviderInstanceId> {
        self.instances
            .iter()
            .filter_map(|(id, instance)| {
                (instance.enabled && instance.provider() == provider).then_some(id.clone())
            })
            .collect()
    }

    #[must_use]
    pub fn provider_for_instance(&self, instance_id: &ProviderInstanceId) -> Option<&ProviderKind> {
        self.instances
            .get(instance_id)
            .map(ProviderInstance::provider)
    }

    #[must_use]
    pub fn mapped_model(&self, provider: &ProviderKind, requested: &str) -> String {
        self.provider_model_mappings
            .get(provider)
            .and_then(|mapping| mapping.get(requested))
            .cloned()
            .unwrap_or_else(|| requested.to_owned())
    }

    pub fn client_policies(&self) -> impl Iterator<Item = &ClientPolicy> {
        self.client_policies.values()
    }

    #[must_use]
    pub fn client_policy(&self, key_id: &ClientApiKeyId) -> Option<&ClientPolicy> {
        self.client_policies.get(key_id)
    }

    pub fn plan(
        &self,
        public_model: &PublicModelId,
        operation: &Operation,
        context: &RoutingContext,
    ) -> Result<RoutingPlan, RoutingError> {
        let upstream_model = context.provider_kind.as_ref().map_or_else(
            || public_model.as_str().to_owned(),
            |provider| self.mapped_model(provider, public_model.as_str()),
        );
        let upstream_model =
            UpstreamModelId::new(upstream_model).map_err(|_| RoutingError::InvalidIdentifier)?;
        let requirements = operation.capability_requirements();
        let mut candidates = Vec::new();

        for instance in self.instances.values() {
            if !self.instance_is_eligible(instance, context) {
                continue;
            }
            let emulated_features = match self
                .provider_models
                .get(instance.id())
                .and_then(|models| models.get(&upstream_model))
            {
                Some(capabilities) => {
                    let Some(emulated) = capabilities.match_requirements(&requirements) else {
                        continue;
                    };
                    emulated
                }
                // 目录用于展示与能力提示，不是模型白名单；未知模型交给上游判断。
                None => BTreeSet::new(),
            };
            candidates.push(ProviderCandidate {
                instance: instance.clone(),
                upstream_model: upstream_model.clone(),
                emulated_features,
                observation_token: context
                    .provider_observation_tokens
                    .get(instance.id())
                    .copied(),
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
            max_attempts: NonZeroU32::new(MAX_REQUEST_ATTEMPTS)
                .expect("constant request attempt limit is non-zero"),
            candidates: Arc::from(candidates),
        })
    }

    fn instance_is_eligible(&self, instance: &ProviderInstance, context: &RoutingContext) -> bool {
        instance.enabled
            && instance.health.is_routable(context.allow_degraded)
            && context
                .provider_kind
                .as_ref()
                .is_none_or(|provider| instance.provider() == provider)
            && !context.blocked_instances.contains(instance.id())
            && context
                .allowed_instances
                .as_ref()
                .is_none_or(|allowed| allowed.contains(instance.id()))
    }
}
