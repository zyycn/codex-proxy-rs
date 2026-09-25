//! RuntimeSnapshot 事实、编译、原子发布与版本收敛规则。

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;

use crate::account::{
    AccountConcurrency, AccountSelectionPolicy, ProviderAccountId, RotationStrategy,
};
use crate::concurrency::ConcurrencyQueuePolicy;
use crate::operation::{Operation, OperationKind};
use crate::policy::{
    ClientApiKeyId, ClientPolicy, CodexClientMinVersions, CodexClientVersion,
    PlaintextClientApiKey, RateLimits,
};
use crate::validation::RoutingError;

use super::{
    AccountGroupId, ClientRoutingScope, ConfigRevision, FrozenAccountScope, ModelCapabilities,
    ProviderCandidate, ProviderCatalogGeneration, ProviderCatalogPort, ProviderKind, ProviderModel,
    PublicModelId, RoutingContext, RoutingGroupSnapshot, RoutingPlan, RuntimeAccount,
    RuntimeAccountDirectory, UpstreamModelId,
};

const MAXIMUM_CATALOG_STABILITY_ATTEMPTS: usize = 4;

/// Store 在一个一致性读取中提供的调度设置事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotSettingsFacts {
    pricing: Arc<crate::metering::PricingOverrides>,
    request_profiles: BTreeMap<ProviderKind, crate::account::OpaqueProviderData>,
    request_location_enabled: bool,
    request_location: crate::account::RequestLocation,
    max_concurrent_per_account: u32,
    max_waiting_per_key: u32,
    max_waiting_per_account: u32,
    concurrency_wait_timeout_seconds: u32,
    responses_max_decompressed_body_bytes: u64,
    request_interval_ms: u64,
    rotation_strategy: String,
    model_mappings: BTreeMap<String, String>,
    min_codex_desktop_version: Option<String>,
    min_codex_cli_version: Option<String>,
}

impl SnapshotSettingsFacts {
    #[must_use]
    pub fn with_pricing(mut self, pricing: crate::metering::PricingOverrides) -> Self {
        self.pricing = Arc::new(pricing);
        self
    }

    #[must_use]
    pub fn with_request_profiles(
        mut self,
        profiles: BTreeMap<ProviderKind, crate::account::OpaqueProviderData>,
    ) -> Self {
        self.request_profiles = profiles;
        self
    }

    #[must_use]
    pub const fn with_responses_max_decompressed_body_bytes(mut self, bytes: u64) -> Self {
        self.responses_max_decompressed_body_bytes = bytes;
        self
    }

    #[must_use]
    pub fn with_request_location(
        mut self,
        location: crate::account::RequestLocation,
        enabled: bool,
    ) -> Self {
        self.request_location_enabled = enabled;
        self.request_location = location;
        self
    }

    #[must_use]
    pub const fn with_concurrency_queues(
        mut self,
        max_waiting_per_key: u32,
        max_waiting_per_account: u32,
        timeout_seconds: u32,
    ) -> Self {
        self.max_waiting_per_key = max_waiting_per_key;
        self.max_waiting_per_account = max_waiting_per_account;
        self.concurrency_wait_timeout_seconds = timeout_seconds;
        self
    }

    #[must_use]
    pub fn new(
        max_concurrent_per_account: u32,
        request_interval_ms: u64,
        rotation_strategy: impl Into<String>,
        model_mappings: BTreeMap<String, String>,
        min_codex_desktop_version: Option<String>,
        min_codex_cli_version: Option<String>,
    ) -> Self {
        Self {
            request_profiles: BTreeMap::new(),
            pricing: Arc::default(),
            request_location_enabled: false,
            request_location: crate::account::RequestLocation::default(),
            max_concurrent_per_account,
            max_waiting_per_key: 0,
            max_waiting_per_account: 0,
            concurrency_wait_timeout_seconds: 30,
            responses_max_decompressed_body_bytes: 64 * 1024 * 1024,
            request_interval_ms,
            rotation_strategy: rotation_strategy.into(),
            model_mappings,
            min_codex_desktop_version,
            min_codex_cli_version,
        }
    }
}

/// Store 读取到的一个启用 Client API Key 策略事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotClientPolicyFacts {
    request_profiles: BTreeMap<ProviderKind, crate::account::OpaqueProviderData>,
    key_id: ClientApiKeyId,
    plaintext_key: PlaintextClientApiKey,
    group_ids: Vec<AccountGroupId>,
    limits: RateLimits,
}

impl SnapshotClientPolicyFacts {
    #[must_use]
    pub fn with_request_profiles(
        mut self,
        profiles: BTreeMap<ProviderKind, crate::account::OpaqueProviderData>,
    ) -> Self {
        self.request_profiles = profiles;
        self
    }

    #[must_use]
    pub fn new(
        key_id: ClientApiKeyId,
        plaintext_key: PlaintextClientApiKey,
        group_ids: Vec<AccountGroupId>,
        limits: RateLimits,
    ) -> Self {
        Self {
            key_id,
            request_profiles: BTreeMap::new(),
            plaintext_key,
            group_ids,
            limits,
        }
    }
}

/// Store 读取到的账号分组事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotAccountGroupFacts {
    disable_fast: bool,
    id: AccountGroupId,
    name: String,
    enabled: bool,
}

impl SnapshotAccountGroupFacts {
    #[must_use]
    pub const fn with_disable_fast(mut self, disable_fast: bool) -> Self {
        self.disable_fast = disable_fast;
        self
    }

    #[must_use]
    pub fn new(id: AccountGroupId, name: String, enabled: bool) -> Self {
        Self {
            id,
            name,
            enabled,
            disable_fast: false,
        }
    }
}

/// Store 读取到的账号及其固有 Provider 事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotProviderAccountFacts {
    account_id: ProviderAccountId,
    provider_kind: String,
    model_access: crate::account::AccountModelAccess,
}

impl SnapshotProviderAccountFacts {
    #[must_use]
    pub fn new(account_id: ProviderAccountId, provider_kind: impl Into<String>) -> Self {
        Self {
            account_id,
            provider_kind: provider_kind.into(),
            model_access: crate::account::AccountModelAccess::all(),
        }
    }
}

impl SnapshotProviderAccountFacts {
    #[must_use]
    pub fn with_model_access(mut self, model_access: crate::account::AccountModelAccess) -> Self {
        self.model_access = model_access;
        self
    }
}

/// Store 读取到的一条分组成员关系。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotAccountGroupMemberFacts {
    group_id: AccountGroupId,
    account_id: ProviderAccountId,
}

impl SnapshotAccountGroupMemberFacts {
    #[must_use]
    pub const fn new(group_id: AccountGroupId, account_id: ProviderAccountId) -> Self {
        Self {
            group_id,
            account_id,
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
    account_groups: Vec<SnapshotAccountGroupFacts>,
    provider_accounts: Vec<SnapshotProviderAccountFacts>,
    group_memberships: Vec<SnapshotAccountGroupMemberFacts>,
}

impl SnapshotFacts {
    #[must_use]
    pub fn new(
        config_revision: ConfigRevision,
        observed_current_revision: ConfigRevision,
        settings: SnapshotSettingsFacts,
        client_policies: Vec<SnapshotClientPolicyFacts>,
        account_groups: Vec<SnapshotAccountGroupFacts>,
        provider_accounts: Vec<SnapshotProviderAccountFacts>,
        group_memberships: Vec<SnapshotAccountGroupMemberFacts>,
    ) -> Self {
        Self {
            config_revision,
            observed_current_revision,
            settings,
            client_policies,
            account_groups,
            provider_accounts,
            group_memberships,
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

/// RuntimeSnapshot 持久事实的数据库中立端口。
pub trait SnapshotStorePort: Send + Sync {
    fn load_snapshot_facts(&self) -> BoxFuture<'_, Result<SnapshotFacts, SnapshotStoreError>>;

    fn current_config_revision(&self) -> BoxFuture<'_, Result<ConfigRevision, SnapshotStoreError>>;
}

/// 快照未发布时可安全记录的稳定错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RuntimeSnapshotCompileError {
    #[error("runtime extensions could not be prepared")]
    ExtensionsUnavailable,
    #[error("runtime snapshot store is unavailable")]
    StoreUnavailable,
    #[error("runtime configuration changed while the snapshot was loading")]
    RevisionChanged,
    #[error("runtime snapshot contains invalid frozen data")]
    InvalidData,
    #[error("extension model aliases conflict with the provider catalog or model mappings")]
    InvalidExtensionModels,
    #[error("provider model catalog changed while the snapshot was compiling")]
    CatalogChanged,
}

/// Store 一致性事实与 Provider 实时目录的唯一快照编译器。
#[derive(Clone)]
pub struct RuntimeSnapshotCompiler {
    store: Arc<dyn SnapshotStorePort>,
    catalogs: Arc<dyn ProviderCatalogPort>,
    extensions: Option<Arc<dyn crate::runtime::extensions::ExtensionPreparationPort>>,
}

impl RuntimeSnapshotCompiler {
    #[must_use]
    pub const fn new(
        store: Arc<dyn SnapshotStorePort>,
        catalogs: Arc<dyn ProviderCatalogPort>,
    ) -> Self {
        Self {
            store,
            catalogs,
            extensions: None,
        }
    }

    #[must_use]
    pub fn with_extensions(
        mut self,
        extensions: Arc<dyn crate::runtime::extensions::ExtensionPreparationPort>,
    ) -> Self {
        self.extensions = Some(extensions);
        self
    }

    pub(crate) fn store(&self) -> Arc<dyn SnapshotStorePort> {
        Arc::clone(&self.store)
    }

    pub(crate) fn provider_catalog_generations(
        &self,
    ) -> BTreeMap<ProviderKind, ProviderCatalogGeneration> {
        self.catalogs.catalog_generations()
    }

    /// 读取一个 revision，并为已注册 Provider 查询实时模型目录。
    pub async fn compile(&self) -> Result<RuntimeSnapshot, RuntimeSnapshotCompileError> {
        self.compile_inner(None).await
    }

    /// 配置提交复用同一扩展集合的已发布目录，目录留待对账刷新。
    pub(crate) async fn compile_with_cached_catalog(
        &self,
        previous: &RuntimeSnapshot,
    ) -> Result<RuntimeSnapshot, RuntimeSnapshotCompileError> {
        self.compile_inner(Some(previous)).await
    }

    async fn compile_inner(
        &self,
        previous: Option<&RuntimeSnapshot>,
    ) -> Result<RuntimeSnapshot, RuntimeSnapshotCompileError> {
        // 同一配置 revision 的重试复用扩展候选，保留插件策略的发布一致性。
        let mut prepared_extensions: Option<(
            ConfigRevision,
            crate::runtime::extensions::ExtensionSetReference,
        )> = None;
        for _ in 0..MAXIMUM_CATALOG_STABILITY_ATTEMPTS {
            let facts = self
                .store
                .load_snapshot_facts()
                .await
                .map_err(|_| RuntimeSnapshotCompileError::StoreUnavailable)?;
            if facts.config_revision != facts.observed_current_revision {
                return Err(RuntimeSnapshotCompileError::RevisionChanged);
            }
            let revision = facts.config_revision;
            let extensions = match &self.extensions {
                Some(preparer) => {
                    let must_prepare =
                        prepared_extensions
                            .as_ref()
                            .is_none_or(|(prepared_revision, prepared)| {
                                *prepared_revision != revision || !prepared.is_ready()
                            });
                    if must_prepare {
                        let prepared = preparer
                            .prepare(revision)
                            .await
                            .map_err(|_| RuntimeSnapshotCompileError::ExtensionsUnavailable)?;
                        prepared_extensions = Some((revision, prepared));
                    }
                    prepared_extensions
                        .as_ref()
                        .map(|(_, prepared)| prepared.clone())
                }
                None => None,
            };
            let catalogs = &self.catalogs;
            let catalog_generations = catalogs.catalog_generations();
            let provider_kinds = catalog_generations.keys().cloned().collect();
            let cached = previous.filter(|previous| {
                previous.extensions().map(|set| set.id()) == extensions.as_ref().map(|set| set.id())
            });
            let snapshot =
                compile_runtime_snapshot(facts, catalogs.as_ref(), provider_kinds, cached).await?;
            let observed_generations = catalogs.catalog_generations();
            if catalog_generations == observed_generations {
                if extensions.is_some()
                    && self
                        .store
                        .current_config_revision()
                        .await
                        .map_err(|_| RuntimeSnapshotCompileError::StoreUnavailable)?
                        != revision
                {
                    return Err(RuntimeSnapshotCompileError::RevisionChanged);
                }
                let snapshot = snapshot
                    .with_provider_catalog_generations(if cached.is_some() {
                        BTreeMap::new()
                    } else {
                        observed_generations
                    })
                    .with_extensions(extensions);
                snapshot.validate_extension_models()?;
                return Ok(snapshot);
            }
        }
        Err(RuntimeSnapshotCompileError::CatalogChanged)
    }
}

async fn compile_runtime_snapshot(
    facts: SnapshotFacts,
    catalogs: &dyn ProviderCatalogPort,
    provider_kinds: Vec<ProviderKind>,
    previous: Option<&RuntimeSnapshot>,
) -> Result<RuntimeSnapshot, RuntimeSnapshotCompileError> {
    // 只有成功取得的完整目录能证明模型缺项；发现型目录和查询失败均交由上游验证。
    let mut provider_models = Vec::new();
    let mut exhaustive_provider_catalogs = BTreeSet::new();
    for provider in &provider_kinds {
        if let Some(previous) = previous {
            if previous.exhaustive_provider_catalogs.contains(provider) {
                exhaustive_provider_catalogs.insert(provider.clone());
            }
            if let Some(models) = previous.provider_models.get(provider) {
                provider_models.extend(models.iter().map(|(model, capabilities)| {
                    let compiled =
                        ProviderModel::new(provider.clone(), model.clone(), capabilities.clone());
                    match previous
                        .provider_model_presentations
                        .get(provider)
                        .and_then(|presentations| presentations.get(model))
                    {
                        Some(presentation) => compiled.with_presentation(presentation.clone()),
                        None => compiled,
                    }
                }));
            }
            continue;
        }
        let Ok(models) = catalogs.query_model_capabilities(provider).await else {
            continue;
        };
        if catalogs.model_catalog_is_exhaustive(provider) {
            exhaustive_provider_catalogs.insert(provider.clone());
        }
        provider_models.extend(models.into_iter().map(|model| {
            let compiled = ProviderModel::new(
                provider.clone(),
                model.upstream_model().clone(),
                model.capabilities().clone(),
            );
            match model.presentation().cloned() {
                Some(presentation) => compiled.with_presentation(presentation),
                None => compiled,
            }
        }));
    }

    let mut groups = BTreeMap::new();
    for group in facts.account_groups {
        if group.name.trim() != group.name
            || group.name.is_empty()
            || group.name.chars().count() > 100
            || group.name.chars().any(char::is_control)
            || groups.insert(group.id.clone(), group).is_some()
        {
            return Err(RuntimeSnapshotCompileError::InvalidData);
        }
    }

    let mut account_groups = BTreeMap::<ProviderAccountId, BTreeSet<AccountGroupId>>::new();
    let mut accounts = BTreeMap::new();
    for account in facts.provider_accounts {
        let provider_kind = ProviderKind::new(account.provider_kind)
            .map_err(|_| RuntimeSnapshotCompileError::InvalidData)?;
        // 账号归属独立于当前执行器集合；Provider 撤下后仍保留账号和分组，路由只选已注册执行器。
        if accounts
            .insert(
                account.account_id.clone(),
                RuntimeAccount::new(provider_kind, BTreeSet::new())
                    .with_model_access(account.model_access),
            )
            .is_some()
        {
            return Err(RuntimeSnapshotCompileError::InvalidData);
        }
        account_groups.insert(account.account_id, BTreeSet::new());
    }
    let mut memberships = BTreeSet::new();
    for membership in facts.group_memberships {
        if !groups.contains_key(&membership.group_id)
            || !accounts.contains_key(&membership.account_id)
            || !memberships.insert((membership.group_id.clone(), membership.account_id.clone()))
        {
            return Err(RuntimeSnapshotCompileError::InvalidData);
        }
        account_groups
            .get_mut(&membership.account_id)
            .ok_or(RuntimeSnapshotCompileError::InvalidData)?
            .insert(membership.group_id);
    }
    for (account_id, group_ids) in account_groups {
        let account = accounts
            .get_mut(&account_id)
            .ok_or(RuntimeSnapshotCompileError::InvalidData)?;
        *account = RuntimeAccount::new(account.provider_kind().clone(), group_ids)
            .with_model_access(account.model_access().clone());
    }
    let account_directory = Arc::new(RuntimeAccountDirectory::new(accounts));

    if facts.settings.max_waiting_per_key > 1_000
        || facts.settings.max_waiting_per_account > 1_000
        || !(1..=120).contains(&facts.settings.concurrency_wait_timeout_seconds)
    {
        return Err(RuntimeSnapshotCompileError::InvalidData);
    }
    let decompressed_body_limit =
        isize::try_from(facts.settings.responses_max_decompressed_body_bytes)
            .ok()
            .and_then(|bytes| usize::try_from(bytes).ok())
            .and_then(std::num::NonZeroUsize::new)
            .ok_or(RuntimeSnapshotCompileError::InvalidData)?;
    let queue_timeout =
        Duration::from_secs(u64::from(facts.settings.concurrency_wait_timeout_seconds));
    let client_queue_policy = ConcurrencyQueuePolicy {
        max_waiting: facts.settings.max_waiting_per_key,
        timeout: queue_timeout,
    };
    let model_mappings = facts.settings.model_mappings;
    let min_client_versions = CodexClientMinVersions::new(
        facts
            .settings
            .min_codex_desktop_version
            .as_deref()
            .map(CodexClientVersion::parse)
            .transpose()
            .map_err(|_| RuntimeSnapshotCompileError::InvalidData)?,
        facts
            .settings
            .min_codex_cli_version
            .as_deref()
            .map(CodexClientVersion::parse)
            .transpose()
            .map_err(|_| RuntimeSnapshotCompileError::InvalidData)?,
    );
    let rotation_strategy = RotationStrategy::parse(facts.settings.rotation_strategy.as_str())
        .ok_or(RuntimeSnapshotCompileError::InvalidData)?;
    let selection_policy = AccountSelectionPolicy::new(
        rotation_strategy,
        AccountConcurrency::new(facts.settings.max_concurrent_per_account),
        Duration::from_millis(facts.settings.request_interval_ms),
    )
    .with_queue(ConcurrencyQueuePolicy {
        max_waiting: facts.settings.max_waiting_per_account,
        timeout: queue_timeout,
    });
    let mut client_policies = Vec::with_capacity(facts.client_policies.len());
    for policy in facts.client_policies {
        let mut disable_fast = false;
        let account_scope = if policy.group_ids.is_empty() {
            FrozenAccountScope::new(
                Arc::clone(&account_directory),
                ClientRoutingScope::all_accounts(),
            )
        } else {
            let mut seen = BTreeSet::new();
            let mut bound_groups = Vec::with_capacity(policy.group_ids.len());
            let mut enabled_group_ids = BTreeSet::new();
            for group_id in policy.group_ids {
                if !seen.insert(group_id.clone()) {
                    return Err(RuntimeSnapshotCompileError::InvalidData);
                }
                let group = groups
                    .get(&group_id)
                    .ok_or(RuntimeSnapshotCompileError::InvalidData)?;
                // 禁用分组仅影响选号；Key 仍绑定其 Fast 限制。
                disable_fast |= group.disable_fast;
                bound_groups.push(RoutingGroupSnapshot::new(
                    group.id.clone(),
                    group.name.clone(),
                ));
                if group.enabled {
                    enabled_group_ids.insert(group_id);
                }
            }
            bound_groups.sort_by(|left, right| left.id().cmp(right.id()));
            let provider_kinds = account_directory.providers_for_groups(&enabled_group_ids);
            FrozenAccountScope::new(
                Arc::clone(&account_directory),
                ClientRoutingScope::restricted(bound_groups, enabled_group_ids, provider_kinds)
                    .map_err(|_| RuntimeSnapshotCompileError::InvalidData)?,
            )
        };
        let mut request_profiles = facts.settings.request_profiles.clone();
        request_profiles.extend(policy.request_profiles);
        client_policies.push(ClientPolicy::new(
            policy.key_id,
            policy.plaintext_key,
            Arc::new(
                account_scope
                    .with_disable_fast(disable_fast)
                    .with_request_profiles(request_profiles),
            ),
            true,
            policy.limits,
        ));
    }

    // 关闭自定义时保留持久化值，但不生成全局覆盖；请求继续使用客户端字段。
    let request_location = if facts.settings.request_location_enabled {
        Some(
            facts
                .settings
                .request_location
                .normalized()
                .map_err(|_| RuntimeSnapshotCompileError::InvalidData)?,
        )
    } else {
        None
    };
    RuntimeSnapshot::new(
        facts.config_revision,
        selection_policy,
        provider_kinds,
        provider_models,
        client_policies,
    )
    .map_err(|_| RuntimeSnapshotCompileError::InvalidData)
    .map(|snapshot| {
        snapshot
            .with_pricing(facts.settings.pricing)
            .with_request_location(request_location)
            .with_responses_max_decompressed_body_bytes(decompressed_body_limit)
            .with_client_queue_policy(client_queue_policy)
            .with_model_mappings(model_mappings)
            .with_account_directory(account_directory)
            .with_exhaustive_provider_catalogs(exhaustive_provider_catalogs)
            .with_min_codex_client_versions(min_client_versions)
    })
}

/// 数据面使用的不可变配置快照。
#[derive(Debug, Clone)]
pub struct RuntimeSnapshot {
    extensions: Option<crate::runtime::extensions::ExtensionSetReference>,
    pricing: Arc<crate::metering::PricingOverrides>,
    responses_max_decompressed_body_bytes: std::num::NonZeroUsize,
    request_location: Option<crate::account::RequestLocation>,
    revision: ConfigRevision,
    client_queue_policy: ConcurrencyQueuePolicy,
    account_selection_policy: AccountSelectionPolicy,
    providers: Arc<BTreeSet<ProviderKind>>,
    provider_models: Arc<BTreeMap<ProviderKind, BTreeMap<UpstreamModelId, ModelCapabilities>>>,
    provider_model_presentations:
        Arc<BTreeMap<ProviderKind, BTreeMap<UpstreamModelId, super::ModelPresentation>>>,
    model_mappings: Arc<BTreeMap<String, String>>,
    provider_catalog_generations: Arc<BTreeMap<ProviderKind, ProviderCatalogGeneration>>,
    exhaustive_provider_catalogs: Arc<BTreeSet<ProviderKind>>,
    account_directory: Arc<RuntimeAccountDirectory>,
    client_policies: Arc<BTreeMap<ClientApiKeyId, ClientPolicy>>,
    min_codex_client_versions: CodexClientMinVersions,
}

impl RuntimeSnapshot {
    #[must_use]
    pub fn with_extensions(
        mut self,
        extensions: Option<crate::runtime::extensions::ExtensionSetReference>,
    ) -> Self {
        self.extensions = extensions;
        self
    }

    #[must_use]
    pub const fn extensions(&self) -> Option<&crate::runtime::extensions::ExtensionSetReference> {
        self.extensions.as_ref()
    }

    fn model_aliases(&self) -> &[super::ContributedModelAlias] {
        self.extensions
            .as_ref()
            .map_or(&[], |set| set.model_aliases())
    }

    fn model_alias(&self, model: &str) -> Option<&super::ContributedModelAlias> {
        self.model_aliases()
            .iter()
            .find(|alias| alias.id.as_str() == model)
    }

    fn validate_extension_models(&self) -> Result<(), RuntimeSnapshotCompileError> {
        let mut ids = BTreeSet::new();
        for alias in self.model_aliases() {
            let valid = ids.insert(&alias.id)
                && self.providers.contains(&alias.provider)
                && !self.model_mappings.contains_key(alias.id.as_str())
                && !self.model_mappings.contains_key(alias.target.as_str())
                && !self
                    .model_mappings
                    .values()
                    .any(|target| target == alias.id.as_str())
                && self.model_alias(alias.target.as_str()).is_none()
                && !self.provider_models.values().any(|models| {
                    models
                        .keys()
                        .any(|model| model.as_str() == alias.id.as_str())
                })
                && self
                    .provider_models
                    .get(&alias.provider)
                    .is_some_and(|models| models.contains_key(&alias.target));
            if !valid {
                tracing::warn!(owner = %alias.owner, model = %alias.id, "插件模型别名与宿主目录或映射冲突，拒绝发布候选快照");
                return Err(RuntimeSnapshotCompileError::InvalidExtensionModels);
            }
        }
        Ok(())
    }
    #[must_use]
    pub fn with_pricing(mut self, pricing: Arc<crate::metering::PricingOverrides>) -> Self {
        self.pricing = pricing;
        self
    }

    #[must_use]
    pub const fn responses_max_decompressed_body_bytes(&self) -> usize {
        self.responses_max_decompressed_body_bytes.get()
    }

    #[must_use]
    pub const fn with_responses_max_decompressed_body_bytes(
        mut self,
        bytes: std::num::NonZeroUsize,
    ) -> Self {
        self.responses_max_decompressed_body_bytes = bytes;
        self
    }

    #[must_use]
    pub fn with_request_location(
        mut self,
        location: Option<crate::account::RequestLocation>,
    ) -> Self {
        self.request_location = location;
        self
    }

    #[must_use]
    pub const fn client_queue_policy(&self) -> ConcurrencyQueuePolicy {
        self.client_queue_policy
    }

    #[must_use]
    pub const fn with_client_queue_policy(mut self, policy: ConcurrencyQueuePolicy) -> Self {
        self.client_queue_policy = policy;
        self
    }

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

        let mut exhaustive_provider_catalogs = BTreeSet::new();
        let mut model_map =
            BTreeMap::<ProviderKind, BTreeMap<UpstreamModelId, ModelCapabilities>>::new();
        let mut presentation_map =
            BTreeMap::<ProviderKind, BTreeMap<UpstreamModelId, super::ModelPresentation>>::new();
        for model in provider_models {
            let ProviderModel {
                provider,
                upstream_model,
                capabilities,
                presentation,
            } = model;
            exhaustive_provider_catalogs.insert(provider.clone());
            if !provider_set.contains(&provider) {
                return Err(RoutingError::NotFound {
                    entity: "provider",
                    id: provider.to_string(),
                });
            }
            let models = model_map.entry(provider.clone()).or_default();
            if models
                .insert(upstream_model.clone(), capabilities)
                .is_some()
            {
                return Err(RoutingError::DuplicateEntity {
                    entity: "provider model",
                    id: upstream_model.to_string(),
                });
            }
            if let Some(presentation) = presentation {
                presentation_map
                    .entry(provider)
                    .or_default()
                    .insert(upstream_model, presentation);
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
            extensions: None,
            responses_max_decompressed_body_bytes: std::num::NonZeroUsize::new(64 * 1024 * 1024)
                .expect("positive default limit"),
            pricing: Arc::default(),
            request_location: None,
            revision,
            account_selection_policy,
            client_queue_policy: ConcurrencyQueuePolicy::default(),
            providers: Arc::new(provider_set),
            provider_models: Arc::new(model_map),
            provider_model_presentations: Arc::new(presentation_map),
            model_mappings: Arc::new(BTreeMap::new()),
            provider_catalog_generations: Arc::new(BTreeMap::new()),
            exhaustive_provider_catalogs: Arc::new(exhaustive_provider_catalogs),
            account_directory: Arc::new(RuntimeAccountDirectory::default()),
            client_policies: Arc::new(client_policy_map),
            min_codex_client_versions: CodexClientMinVersions::default(),
        })
    }

    #[must_use]
    pub fn with_model_mappings(mut self, mappings: BTreeMap<String, String>) -> Self {
        self.model_mappings = Arc::new(mappings);
        self
    }

    #[must_use]
    pub fn with_account_directory(mut self, directory: Arc<RuntimeAccountDirectory>) -> Self {
        self.account_directory = directory;
        self
    }

    #[must_use]
    pub fn with_min_codex_client_versions(mut self, versions: CodexClientMinVersions) -> Self {
        self.min_codex_client_versions = versions;
        self
    }

    #[must_use]
    fn with_exhaustive_provider_catalogs(mut self, providers: BTreeSet<ProviderKind>) -> Self {
        self.exhaustive_provider_catalogs = Arc::new(providers);
        self
    }

    #[must_use]
    pub fn all_account_scope(&self) -> Arc<FrozenAccountScope> {
        Arc::new(FrozenAccountScope::new(
            Arc::clone(&self.account_directory),
            ClientRoutingScope::all_accounts(),
        ))
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
        if !self.providers.contains(provider) {
            return Vec::new();
        }
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
        models.extend(
            self.model_aliases()
                .iter()
                .filter(|alias| &alias.provider == provider)
                .map(|alias| alias.id.clone()),
        );
        models.into_iter().collect()
    }

    /// 返回 Provider 已明确声明画像的公开模型；没有画像时不猜测 Provider 语义。
    #[must_use]
    pub fn public_model_profiles_for_provider(
        &self,
        provider: &ProviderKind,
    ) -> Vec<super::PublicModelProfile> {
        let Some(presentations) = self.provider_model_presentations.get(provider) else {
            return Vec::new();
        };
        let mut profiles = BTreeMap::new();
        for (model, presentation) in presentations {
            if let Ok(public_model) = PublicModelId::new(model.as_str().to_owned()) {
                profiles.insert(public_model, presentation.clone());
            }
        }
        for alias in self.model_mappings.keys() {
            let target = self.mapped_model(alias);
            let Some(presentation) = presentations.iter().find_map(|(model, presentation)| {
                (model.as_str() == target).then_some(presentation)
            }) else {
                continue;
            };
            if let Ok(public_model) = PublicModelId::new(alias.clone()) {
                profiles.insert(public_model, presentation.clone());
            }
        }
        for alias in self
            .model_aliases()
            .iter()
            .filter(|alias| &alias.provider == provider)
        {
            if let Some(presentation) = presentations.get(&alias.target) {
                profiles.insert(alias.id.clone(), presentation.clone());
            }
        }
        profiles
            .into_iter()
            .map(|(model, presentation)| super::PublicModelProfile::new(model, presentation))
            .collect()
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

    /// 合并冻结账号范围内实际存在 Provider 的公开模型。
    #[must_use]
    pub fn public_models_for_scope(&self, scope: &FrozenAccountScope) -> Vec<PublicModelId> {
        scope
            .provider_kinds()
            .iter()
            .flat_map(|provider| {
                self.public_models_for_provider(provider)
                    .into_iter()
                    .filter(|model| {
                        scope.allows_provider_model(provider, &self.mapped_model(model.as_str()))
                    })
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// 合并冻结账号范围内实际存在 Provider 的公开模型画像。
    #[must_use]
    pub fn public_model_profiles_for_scope(
        &self,
        scope: &FrozenAccountScope,
    ) -> Vec<super::PublicModelProfile> {
        let mut profiles = BTreeMap::new();
        for provider in scope.provider_kinds() {
            for profile in self.public_model_profiles_for_provider(provider) {
                if !scope
                    .allows_provider_model(provider, &self.mapped_model(profile.model().as_str()))
                {
                    continue;
                }
                profiles
                    .entry(profile.model().clone())
                    .or_insert_with(|| profile.presentation().clone());
            }
        }
        profiles
            .into_iter()
            .map(|(model, presentation)| super::PublicModelProfile::new(model, presentation))
            .collect()
    }

    /// 完整目录按映射后的模型判定；发现型或不可用目录不作为能力白名单。
    #[must_use]
    pub fn contains_public_model_for_provider(
        &self,
        public_model: &PublicModelId,
        provider: &ProviderKind,
    ) -> bool {
        if !self.providers.contains(provider) {
            return false;
        }
        if self
            .model_alias(public_model.as_str())
            .is_some_and(|alias| &alias.provider != provider)
        {
            return false;
        }
        if !self.exhaustive_provider_catalogs.contains(provider) {
            return true;
        }
        let upstream_model = self.mapped_model(public_model.as_str());
        self.provider_models
            .get(provider)
            .is_some_and(|models| models.keys().any(|model| model.as_str() == upstream_model))
    }

    #[must_use]
    pub fn contains_public_model_for_scope(
        &self,
        public_model: &PublicModelId,
        scope: &FrozenAccountScope,
    ) -> bool {
        scope.provider_kinds().iter().any(|provider| {
            self.contains_public_model_for_provider(public_model, provider)
                && scope.allows_provider_model(provider, &self.mapped_model(public_model.as_str()))
        })
    }

    #[must_use]
    pub fn mapped_model(&self, requested: &str) -> String {
        if let Some(alias) = self.model_alias(requested) {
            return alias.target.as_str().to_owned();
        }
        let original = requested;
        let mut current = original.to_owned();
        let mut seen = BTreeSet::new();
        for _ in 0..20 {
            let Some(target) = self.model_mappings.get(&current).map(String::as_str) else {
                return current;
            };
            if !seen.insert(current.clone()) || seen.contains(target) {
                return original.to_owned();
            }
            current = target.to_owned();
        }
        original.to_owned()
    }

    pub fn client_policies(&self) -> impl Iterator<Item = &ClientPolicy> {
        self.client_policies.values()
    }

    #[must_use]
    pub fn client_policy(&self, id: &ClientApiKeyId) -> Option<&ClientPolicy> {
        self.client_policies.get(id)
    }

    /// 返回当前 Key 范围、本代次注册表与请求路由限制共同允许的 Provider。
    #[must_use]
    pub fn available_providers(
        &self,
        account_scope: &FrozenAccountScope,
        context: &RoutingContext,
    ) -> BTreeSet<ProviderKind> {
        account_scope
            .provider_kinds()
            .iter()
            .filter(|provider| {
                self.providers.contains(*provider)
                    && context
                        .required_provider
                        .as_ref()
                        .is_none_or(|required| required == *provider)
                    && !context.blocked_providers.contains(*provider)
            })
            .cloned()
            .collect()
    }

    #[must_use]
    pub const fn min_codex_client_versions(&self) -> &CodexClientMinVersions {
        &self.min_codex_client_versions
    }

    pub fn plan(
        &self,
        public_model: &PublicModelId,
        operation: &Operation,
        account_scope: Arc<FrozenAccountScope>,
        context: &RoutingContext,
    ) -> Result<RoutingPlan, RoutingError> {
        self.plan_inner(public_model, operation, account_scope, context, true)
    }

    /// 管理探测已固定目标账号，不使用数据面 Key 的账号模型政策裁决探测资格。
    pub(crate) fn plan_diagnostic(
        &self,
        public_model: &PublicModelId,
        operation: &Operation,
        context: &RoutingContext,
    ) -> Result<RoutingPlan, RoutingError> {
        self.plan_inner(
            public_model,
            operation,
            self.all_account_scope(),
            context,
            false,
        )
    }

    fn plan_inner(
        &self,
        public_model: &PublicModelId,
        operation: &Operation,
        account_scope: Arc<FrozenAccountScope>,
        context: &RoutingContext,
        enforce_account_model_policy: bool,
    ) -> Result<RoutingPlan, RoutingError> {
        let requirements = operation.capability_requirements();
        let mut candidates = Vec::new();

        if context.required_provider.is_none() && account_scope.provider_kinds().is_empty() {
            return Err(RoutingError::EmptyAccountScope);
        }

        let providers = context.required_provider.as_ref().map_or_else(
            || account_scope.provider_kinds().clone(),
            |provider| BTreeSet::from([provider.clone()]),
        );
        for provider in &providers {
            if !self.providers.contains(provider) {
                continue;
            }
            if self
                .model_alias(public_model.as_str())
                .is_some_and(|alias| &alias.provider != provider)
            {
                continue;
            }
            if context
                .required_provider
                .as_ref()
                .is_some_and(|expected| expected != provider)
                || context.blocked_providers.contains(provider)
            {
                continue;
            }
            let requested_model = public_model.as_str();
            let mapped_model = self.mapped_model(requested_model);
            let upstream_model = if self.model_mappings.contains_key(requested_model)
                || self.model_alias(requested_model).is_some()
            {
                UpstreamModelId::new(mapped_model)
            } else {
                UpstreamModelId::from_client_wire(mapped_model)
            }
            .map_err(|_| RoutingError::InvalidIdentifier)?;
            // 强制 Provider 路由也不能扩大 Key 授权；管理诊断的目标账号由探测入口固定。
            if enforce_account_model_policy
                && !account_scope.allows_provider_model(provider, upstream_model.as_str())
            {
                continue;
            }
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
                None if self.exhaustive_provider_catalogs.contains(provider) => continue,
                None => BTreeSet::new(),
            };
            candidates.push(ProviderCandidate {
                provider: provider.clone(),
                upstream_model: Some(upstream_model),
                emulated_features,
                account_scope: Arc::clone(&account_scope),
            });
        }

        if candidates.is_empty() {
            // 模型存在性必须包含被请求路由限制排除的 Provider；目录未知时不能断言模型不存在。
            let mut scoped_providers = providers.intersection(&self.providers).peekable();
            if scoped_providers.peek().is_some()
                && scoped_providers.all(|provider| {
                    self.exhaustive_provider_catalogs.contains(provider)
                        && !self.contains_public_model_for_provider(public_model, provider)
                })
            {
                return Err(RoutingError::ModelNotFound {
                    model: public_model.as_str().to_owned(),
                    mapped_model: self.mapped_model(public_model.as_str()),
                });
            }
            return Err(RoutingError::NoCapableProvider {
                model: public_model.as_str().to_owned(),
            });
        }

        Ok(RoutingPlan {
            config_revision: self.revision,
            pricing: Arc::clone(&self.pricing),
            request_location: self.request_location.clone(),
            account_selection_policy: self.account_selection_policy,
            operation: operation.kind(),
            max_attempts: NonZeroU32::new(super::MAX_REQUEST_ATTEMPTS)
                .expect("constant request attempt limit is non-zero"),
            account_scope,
            candidates: Arc::from(candidates),
        })
    }

    /// 为 Provider 自有端点冻结请求计划。
    ///
    /// 端点 adapter 已经确定 Provider。非模型 HTTP 端点不读取文本模型目录；Token
    /// 计数端点仍必须匹配冻结账号范围及该模型声明的计数能力。
    pub fn plan_provider_endpoint(
        &self,
        provider: &ProviderKind,
        upstream_model: Option<&UpstreamModelId>,
        operation: &Operation,
        account_scope: Arc<FrozenAccountScope>,
        context: &RoutingContext,
    ) -> Result<RoutingPlan, RoutingError> {
        let available = !account_scope.provider_kinds().is_empty()
            && account_scope.provider_kinds().contains(provider)
            && self.providers.contains(provider)
            && context
                .required_provider
                .as_ref()
                .is_none_or(|required| required == provider)
            && !context.blocked_providers.contains(provider);
        if !available
            || upstream_model
                .is_some_and(|model| !account_scope.allows_provider_model(provider, model.as_str()))
        {
            return Err(RoutingError::NoCapableProviderEndpoint {
                provider: provider.as_str().to_owned(),
            });
        }
        let model_binding_valid =
            matches!(operation.kind(), OperationKind::CountTokens) == upstream_model.is_some();
        let model_capable = upstream_model.is_none_or(|model| {
            match self
                .provider_models
                .get(provider)
                .and_then(|models| models.get(model))
            {
                Some(capabilities) => capabilities
                    .match_requirements(&operation.capability_requirements())
                    .is_some(),
                None => !self.exhaustive_provider_catalogs.contains(provider),
            }
        });
        if !model_binding_valid || !model_capable {
            return Err(RoutingError::UnsupportedProviderEndpoint {
                provider: provider.as_str().to_owned(),
                operation: operation.kind().as_str(),
            });
        }
        let candidate = ProviderCandidate {
            provider: provider.clone(),
            upstream_model: upstream_model.cloned(),
            emulated_features: BTreeSet::new(),
            account_scope: Arc::clone(&account_scope),
        };
        Ok(RoutingPlan {
            config_revision: self.revision,
            pricing: Arc::clone(&self.pricing),
            request_location: self.request_location.clone(),
            account_selection_policy: self.account_selection_policy,
            operation: operation.kind(),
            max_attempts: NonZeroU32::new(super::MAX_REQUEST_ATTEMPTS)
                .expect("constant request attempt limit is non-zero"),
            account_scope,
            candidates: Arc::from([candidate]),
        })
    }
}
