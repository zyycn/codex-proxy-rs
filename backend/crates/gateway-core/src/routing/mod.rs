//! Provider、模型目录、精确模型映射与请求级候选计划。

pub mod snapshot;

pub use snapshot::RuntimeSnapshot;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::num::{NonZeroU32, NonZeroU64};
use std::sync::Arc;

use crate::engine::credential::{AccountSelectionPolicy, ProviderAccountId};
use crate::error::{IdentifierError, RoutingError, validate_text};
use crate::operation::{CapabilityRequirements, Feature, OperationKind};

const MAX_REQUEST_ATTEMPTS: u32 = 32;

/// `account_groups.id` 的核心值对象。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccountGroupId(String);

impl AccountGroupId {
    /// 校验并创建账号分组 ID。
    pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = value.into();
        let Some(suffix) = value.strip_prefix("grp_") else {
            return Err(IdentifierError::MissingPrefix { expected: "grp_" });
        };
        if suffix.len() != 32
            || !suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(IdentifierError::InvalidFormat);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AccountGroupId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// 快照中一个账号的 Provider 与分组归属。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeAccount {
    provider_kind: ProviderKind,
    group_ids: Arc<BTreeSet<AccountGroupId>>,
}

impl RuntimeAccount {
    #[must_use]
    pub fn new(provider_kind: ProviderKind, group_ids: BTreeSet<AccountGroupId>) -> Self {
        Self {
            provider_kind,
            group_ids: Arc::new(group_ids),
        }
    }

    #[must_use]
    pub const fn provider_kind(&self) -> &ProviderKind {
        &self.provider_kind
    }

    #[must_use]
    pub fn group_ids(&self) -> &BTreeSet<AccountGroupId> {
        &self.group_ids
    }
}

/// 全快照共享的账号、Provider 与分组反向索引。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeAccountDirectory {
    accounts: BTreeMap<ProviderAccountId, RuntimeAccount>,
    providers_with_accounts: BTreeSet<ProviderKind>,
    providers_by_group: BTreeMap<AccountGroupId, BTreeSet<ProviderKind>>,
}

impl RuntimeAccountDirectory {
    #[must_use]
    pub fn new(accounts: BTreeMap<ProviderAccountId, RuntimeAccount>) -> Self {
        let mut providers_with_accounts = BTreeSet::new();
        let mut providers_by_group = BTreeMap::<AccountGroupId, BTreeSet<ProviderKind>>::new();
        for account in accounts.values() {
            providers_with_accounts.insert(account.provider_kind.clone());
            for group_id in account.group_ids.iter() {
                providers_by_group
                    .entry(group_id.clone())
                    .or_default()
                    .insert(account.provider_kind.clone());
            }
        }
        Self {
            accounts,
            providers_with_accounts,
            providers_by_group,
        }
    }

    #[must_use]
    pub fn account(&self, account_id: &ProviderAccountId) -> Option<&RuntimeAccount> {
        self.accounts.get(account_id)
    }

    #[must_use]
    pub fn providers_with_accounts(&self) -> &BTreeSet<ProviderKind> {
        &self.providers_with_accounts
    }

    #[must_use]
    pub fn providers_for_groups<'a>(
        &self,
        group_ids: impl IntoIterator<Item = &'a AccountGroupId>,
    ) -> BTreeSet<ProviderKind> {
        group_ids
            .into_iter()
            .filter_map(|group_id| self.providers_by_group.get(group_id))
            .flatten()
            .cloned()
            .collect()
    }
}

/// 历史请求保存的账号范围种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountRoutingScopeKind {
    All,
    Groups,
}

impl AccountRoutingScopeKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Groups => "groups",
        }
    }
}

/// 请求开始时冻结的分组 ID 与名称。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingGroupSnapshot {
    id: AccountGroupId,
    name: String,
}

impl RoutingGroupSnapshot {
    #[must_use]
    pub fn new(id: AccountGroupId, name: String) -> Self {
        Self { id, name }
    }

    #[must_use]
    pub const fn id(&self) -> &AccountGroupId {
        &self.id
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// 请求历史所需的完整、稳定账号范围快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRoutingSnapshot {
    kind: AccountRoutingScopeKind,
    groups: Arc<[RoutingGroupSnapshot]>,
}

impl AccountRoutingSnapshot {
    #[must_use]
    pub fn all() -> Self {
        Self {
            kind: AccountRoutingScopeKind::All,
            groups: Arc::from([]),
        }
    }

    #[must_use]
    pub fn groups(groups: Vec<RoutingGroupSnapshot>) -> Self {
        Self {
            kind: AccountRoutingScopeKind::Groups,
            groups: Arc::from(groups),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> AccountRoutingScopeKind {
        self.kind
    }

    #[must_use]
    pub fn groups_snapshot(&self) -> &[RoutingGroupSnapshot] {
        &self.groups
    }
}

/// Key 持久 binding 编译出的账号权限。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientRoutingScope {
    AllAccounts,
    Restricted {
        bound_groups: Arc<[RoutingGroupSnapshot]>,
        enabled_group_ids: Arc<BTreeSet<AccountGroupId>>,
        provider_kinds: Arc<BTreeSet<ProviderKind>>,
    },
}

impl ClientRoutingScope {
    #[must_use]
    pub fn all_accounts() -> Self {
        Self::AllAccounts
    }

    pub fn restricted(
        bound_groups: Vec<RoutingGroupSnapshot>,
        enabled_group_ids: BTreeSet<AccountGroupId>,
        provider_kinds: BTreeSet<ProviderKind>,
    ) -> Result<Self, RoutingError> {
        if bound_groups.is_empty() {
            return Err(RoutingError::InvalidAccountScope);
        }
        Ok(Self::Restricted {
            bound_groups: Arc::from(bound_groups),
            enabled_group_ids: Arc::new(enabled_group_ids),
            provider_kinds: Arc::new(provider_kinds),
        })
    }
}

/// 一次认证随 RuntimeSnapshot 冻结的账号目录与 Key 权限。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenAccountScope {
    directory: Arc<RuntimeAccountDirectory>,
    client_scope: ClientRoutingScope,
}

impl FrozenAccountScope {
    #[must_use]
    pub const fn new(
        directory: Arc<RuntimeAccountDirectory>,
        client_scope: ClientRoutingScope,
    ) -> Self {
        Self {
            directory,
            client_scope,
        }
    }

    #[must_use]
    pub fn allows(&self, account_id: &ProviderAccountId) -> bool {
        let Some(account) = self.directory.account(account_id) else {
            return false;
        };
        match &self.client_scope {
            ClientRoutingScope::AllAccounts => true,
            ClientRoutingScope::Restricted {
                enabled_group_ids, ..
            } => account
                .group_ids()
                .iter()
                .any(|group_id| enabled_group_ids.contains(group_id)),
        }
    }

    #[must_use]
    pub fn provider_kinds(&self) -> &BTreeSet<ProviderKind> {
        match &self.client_scope {
            ClientRoutingScope::AllAccounts => self.directory.providers_with_accounts(),
            ClientRoutingScope::Restricted { provider_kinds, .. } => provider_kinds,
        }
    }

    #[must_use]
    pub fn routing_snapshot(&self) -> AccountRoutingSnapshot {
        match &self.client_scope {
            ClientRoutingScope::AllAccounts => AccountRoutingSnapshot::all(),
            ClientRoutingScope::Restricted { bound_groups, .. } => {
                AccountRoutingSnapshot::groups(bound_groups.to_vec())
            }
        }
    }

    #[must_use]
    pub const fn directory(&self) -> &Arc<RuntimeAccountDirectory> {
        &self.directory
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

    /// 从客户端 OpenAI wire 读取模型名。
    ///
    /// 该值只作为路由查询键；具体模型字符串能否被上游接受由 Provider 决定，
    /// 因而不能把内部标识长度或控制字符规则当作入站 schema gate。
    ///
    /// # Errors
    ///
    /// 空模型名无法参与路由时返回错误。
    pub fn from_client_wire(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = value.into();
        if value.is_empty() {
            return Err(IdentifierError::Empty);
        }
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

    /// 从未命中显式映射的客户端模型名构造同名上游模型。
    ///
    /// 目录和配置仍使用 [`Self::new`] 的内部标识约束；客户端 wire 只要求非空，
    /// 具体模型字符串是否可用由绑定 Provider 决定。
    pub fn from_client_wire(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = value.into();
        if value.is_empty() {
            return Err(IdentifierError::Empty);
        }
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
    max_output_tokens: Option<u64>,
    upstream_validates_features: bool,
}

/// Provider 为客户端模型目录提供的展示与交互能力。
///
/// 该值不参与路由，也不把任一 Provider 的 wire 类型带入 Core。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelPresentation {
    display_name: Option<String>,
    description: Option<String>,
    default_reasoning_effort: Option<String>,
    supported_reasoning_efforts: Vec<String>,
    context_window_tokens: Option<u64>,
    image_input: bool,
    agent_tools: bool,
    parallel_tool_calls: bool,
    search_tool: bool,
    image_detail_original: bool,
    verbosity: bool,
    service_tiers: Vec<ModelServiceTier>,
    hidden: bool,
}

/// Provider 声明给 Codex 客户端的服务档位。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelServiceTier {
    id: String,
    name: String,
    description: String,
    speed_tier: Option<String>,
}

impl ModelServiceTier {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            speed_tier: None,
        }
    }

    #[must_use]
    pub fn with_speed_tier(mut self, speed_tier: impl Into<String>) -> Self {
        self.speed_tier = Some(speed_tier.into());
        self
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    #[must_use]
    pub fn speed_tier(&self) -> Option<&str> {
        self.speed_tier.as_deref()
    }
}

impl ModelPresentation {
    #[must_use]
    pub fn new(display_name: Option<String>, description: Option<String>) -> Self {
        Self {
            display_name,
            description,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn with_reasoning(
        mut self,
        default_effort: Option<String>,
        supported_efforts: Vec<String>,
    ) -> Self {
        self.default_reasoning_effort = default_effort;
        self.supported_reasoning_efforts = supported_efforts;
        self
    }

    #[must_use]
    pub const fn with_context_window_tokens(mut self, context_window_tokens: Option<u64>) -> Self {
        self.context_window_tokens = context_window_tokens;
        self
    }

    #[must_use]
    pub const fn with_image_input(mut self, image_input: bool) -> Self {
        self.image_input = image_input;
        self
    }

    #[must_use]
    pub const fn with_agent_tools(mut self, agent_tools: bool, parallel_tool_calls: bool) -> Self {
        self.agent_tools = agent_tools;
        self.parallel_tool_calls = parallel_tool_calls;
        self
    }

    #[must_use]
    pub const fn with_search_tool(mut self, search_tool: bool) -> Self {
        self.search_tool = search_tool;
        self
    }

    #[must_use]
    pub const fn with_image_detail_original(mut self, image_detail_original: bool) -> Self {
        self.image_detail_original = image_detail_original;
        self
    }

    #[must_use]
    pub const fn with_verbosity(mut self, verbosity: bool) -> Self {
        self.verbosity = verbosity;
        self
    }

    #[must_use]
    pub fn with_service_tiers(mut self, service_tiers: Vec<ModelServiceTier>) -> Self {
        self.service_tiers = service_tiers;
        self
    }

    #[must_use]
    pub const fn with_hidden(mut self, hidden: bool) -> Self {
        self.hidden = hidden;
        self
    }

    #[must_use]
    pub fn display_name(&self) -> Option<&str> {
        self.display_name.as_deref()
    }

    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    #[must_use]
    pub fn default_reasoning_effort(&self) -> Option<&str> {
        self.default_reasoning_effort.as_deref()
    }

    #[must_use]
    pub fn supported_reasoning_efforts(&self) -> &[String] {
        &self.supported_reasoning_efforts
    }

    #[must_use]
    pub const fn context_window_tokens(&self) -> Option<u64> {
        self.context_window_tokens
    }

    #[must_use]
    pub const fn image_input(&self) -> bool {
        self.image_input
    }

    #[must_use]
    pub const fn agent_tools(&self) -> bool {
        self.agent_tools
    }

    #[must_use]
    pub const fn parallel_tool_calls(&self) -> bool {
        self.parallel_tool_calls
    }

    #[must_use]
    pub const fn search_tool(&self) -> bool {
        self.search_tool
    }

    #[must_use]
    pub const fn image_detail_original(&self) -> bool {
        self.image_detail_original
    }

    #[must_use]
    pub const fn verbosity(&self) -> bool {
        self.verbosity
    }

    #[must_use]
    pub fn service_tiers(&self) -> &[ModelServiceTier] {
        &self.service_tiers
    }

    #[must_use]
    pub const fn hidden(&self) -> bool {
        self.hidden
    }
}

/// 一个公开模型及其 Provider 编译后的客户端画像。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicModelProfile {
    model: PublicModelId,
    presentation: ModelPresentation,
}

impl PublicModelProfile {
    #[must_use]
    pub const fn new(model: PublicModelId, presentation: ModelPresentation) -> Self {
        Self {
            model,
            presentation,
        }
    }

    #[must_use]
    pub const fn model(&self) -> &PublicModelId {
        &self.model
    }

    #[must_use]
    pub const fn presentation(&self) -> &ModelPresentation {
        &self.presentation
    }
}

impl ModelCapabilities {
    #[must_use]
    pub fn new(operations: BTreeSet<OperationKind>, max_output_tokens: Option<u64>) -> Self {
        Self {
            operations,
            features: BTreeMap::new(),
            max_output_tokens,
            upstream_validates_features: false,
        }
    }

    #[must_use]
    pub fn with_feature(mut self, feature: Feature, support: SupportLevel) -> Self {
        self.features.insert(feature, support);
        self
    }

    /// 将请求形态 feature 的最终合法性判断交给上游 wire API。
    #[must_use]
    pub const fn with_upstream_feature_validation(mut self) -> Self {
        self.upstream_validates_features = true;
        self
    }

    #[must_use]
    pub fn match_requirements(
        &self,
        requirements: &CapabilityRequirements,
    ) -> Option<BTreeSet<Feature>> {
        if !self.operations.contains(&requirements.operation())
            || requirements
                .requested_output_tokens()
                .is_some_and(|requested| {
                    self.max_output_tokens
                        .is_some_and(|maximum| requested > maximum)
                })
        {
            return None;
        }

        if self.upstream_validates_features {
            return Some(BTreeSet::new());
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

/// 一个 Provider 实时发现的上游模型能力。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderModel {
    provider: ProviderKind,
    upstream_model: UpstreamModelId,
    capabilities: ModelCapabilities,
    presentation: Option<ModelPresentation>,
}

impl ProviderModel {
    #[must_use]
    pub const fn new(
        provider: ProviderKind,
        upstream_model: UpstreamModelId,
        capabilities: ModelCapabilities,
    ) -> Self {
        Self {
            provider,
            upstream_model,
            capabilities,
            presentation: None,
        }
    }

    #[must_use]
    pub fn with_presentation(mut self, presentation: ModelPresentation) -> Self {
        self.presentation = Some(presentation);
        self
    }

    #[must_use]
    pub const fn provider(&self) -> &ProviderKind {
        &self.provider
    }

    #[must_use]
    pub const fn upstream_model(&self) -> &UpstreamModelId {
        &self.upstream_model
    }

    #[must_use]
    pub const fn presentation(&self) -> Option<&ModelPresentation> {
        self.presentation.as_ref()
    }
}

/// 本次请求选择 Provider 时使用的动态过滤事实。
#[derive(Debug, Clone, Default)]
pub struct RoutingContext {
    /// 管理端 connection test 显式限制的 Provider；普通请求留空。
    pub required_provider: Option<ProviderKind>,
    pub blocked_providers: BTreeSet<ProviderKind>,
}

/// 已绑定 Provider 的请求候选；模型端点携带真实上游模型，原生端点不虚构模型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCandidate {
    provider: ProviderKind,
    upstream_model: Option<UpstreamModelId>,
    emulated_features: BTreeSet<Feature>,
    account_scope: Arc<FrozenAccountScope>,
}

impl ProviderCandidate {
    #[must_use]
    pub const fn provider(&self) -> &ProviderKind {
        &self.provider
    }

    #[must_use]
    pub const fn upstream_model(&self) -> Option<&UpstreamModelId> {
        self.upstream_model.as_ref()
    }

    #[must_use]
    pub const fn emulated_features(&self) -> &BTreeSet<Feature> {
        &self.emulated_features
    }

    #[must_use]
    pub const fn account_scope(&self) -> &Arc<FrozenAccountScope> {
        &self.account_scope
    }
}

/// 一次请求冻结的 Provider 尝试顺序。
#[derive(Debug, Clone)]
pub struct RoutingPlan {
    config_revision: ConfigRevision,
    account_selection_policy: AccountSelectionPolicy,
    operation: OperationKind,
    max_attempts: NonZeroU32,
    account_scope: Arc<FrozenAccountScope>,
    candidates: Arc<[ProviderCandidate]>,
}

impl RoutingPlan {
    #[must_use]
    pub const fn config_revision(&self) -> ConfigRevision {
        self.config_revision
    }

    #[must_use]
    pub const fn account_selection_policy(&self) -> AccountSelectionPolicy {
        self.account_selection_policy
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
    pub const fn account_scope(&self) -> &Arc<FrozenAccountScope> {
        &self.account_scope
    }

    #[must_use]
    pub fn candidates(&self) -> &[ProviderCandidate] {
        &self.candidates
    }
}
