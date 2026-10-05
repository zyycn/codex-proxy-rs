//! 请求认证时冻结的账号范围与目录，不依赖路由选择器

use super::{FastMode, ProviderAccountId};
use crate::identity::ProviderKind;
use crate::validation::{IdentifierError, RoutingError};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::Arc,
};

/// `account_groups.id` 的核心值对象
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccountGroupId(String);

impl AccountGroupId {
    /// 校验并创建账号分组 ID
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

/// 快照中一个账号的 Provider 与分组归属
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeAccount {
    provider_kind: ProviderKind,
    group_ids: Arc<BTreeSet<AccountGroupId>>,
    model_access: super::AccountModelAccess,
}

impl RuntimeAccount {
    #[must_use]
    pub fn new(provider_kind: ProviderKind, group_ids: BTreeSet<AccountGroupId>) -> Self {
        Self {
            provider_kind,
            group_ids: Arc::new(group_ids),
            model_access: super::AccountModelAccess::all(),
        }
    }

    #[must_use]
    pub fn with_model_access(mut self, model_access: super::AccountModelAccess) -> Self {
        self.model_access = model_access;
        self
    }

    #[must_use]
    pub const fn model_access(&self) -> &super::AccountModelAccess {
        &self.model_access
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

/// 全快照共享的账号、Provider 与分组反向索引
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

/// 历史请求保存的账号范围种类
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

/// 请求开始时冻结的分组 ID 与名称
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

/// 请求历史所需的完整、稳定账号范围快照
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

/// Key 持久 binding 编译出的账号权限
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

/// 一次认证随 RuntimeSnapshot 冻结的账号目录与 Key 权限
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenAccountScope {
    fast_mode: FastMode,
    request_profiles: BTreeMap<ProviderKind, super::OpaqueProviderData>,
    directory: Arc<RuntimeAccountDirectory>,
    client_scope: ClientRoutingScope,
    provider_kinds: Arc<BTreeSet<ProviderKind>>,
    allowed_account_ids: Option<Arc<BTreeSet<ProviderAccountId>>>,
    excluded_account_ids: Arc<BTreeSet<ProviderAccountId>>,
}

impl FrozenAccountScope {
    /// 与 Key 授权范围一同冻结；具体字段只由对应 Provider 解释
    #[must_use]
    pub fn with_request_profiles(
        mut self,
        profiles: BTreeMap<ProviderKind, super::OpaqueProviderData>,
    ) -> Self {
        self.request_profiles = profiles;
        self
    }

    #[must_use]
    pub fn request_profile(&self, provider: &ProviderKind) -> Option<&super::OpaqueProviderData> {
        self.request_profiles.get(provider)
    }

    #[must_use]
    pub fn request_profiles(&self) -> &BTreeMap<ProviderKind, super::OpaqueProviderData> {
        &self.request_profiles
    }

    /// Key 绑定分组的冻结 Fast 策略，与账号成员资格无关
    #[must_use]
    pub const fn with_fast_mode(mut self, fast_mode: FastMode) -> Self {
        self.fast_mode = fast_mode;
        self
    }

    #[must_use]
    pub const fn fast_mode(&self) -> FastMode {
        self.fast_mode
    }

    #[must_use]
    pub fn new(directory: Arc<RuntimeAccountDirectory>, client_scope: ClientRoutingScope) -> Self {
        let provider_kinds = match &client_scope {
            ClientRoutingScope::AllAccounts => Arc::new(directory.providers_with_accounts.clone()),
            ClientRoutingScope::Restricted { provider_kinds, .. } => Arc::clone(provider_kinds),
        };
        Self {
            fast_mode: FastMode::Default,
            request_profiles: BTreeMap::new(),
            directory,
            client_scope,
            provider_kinds,
            allowed_account_ids: None,
            excluded_account_ids: Arc::default(),
        }
    }

    /// 在已经冻结的 Key 范围内施加一次请求局部的 Provider/账号交集
    ///
    /// 空集合表示该维度不额外收窄；非空集合即使没有有效交集也保持为空，
    /// 不能回退为父请求的完整范围
    #[must_use]
    pub fn restricted_to(
        &self,
        providers: &BTreeSet<ProviderKind>,
        accounts: &BTreeSet<ProviderAccountId>,
    ) -> Self {
        let mut restricted = self.clone();
        let mut provider_kinds = self.provider_kinds.as_ref().clone();
        if !providers.is_empty() {
            provider_kinds.retain(|provider| providers.contains(provider));
        }
        if !accounts.is_empty() {
            let allowed_account_ids = accounts
                .iter()
                .filter(|account| self.allows(account))
                .cloned()
                .collect::<BTreeSet<_>>();
            provider_kinds.retain(|provider| {
                allowed_account_ids.iter().any(|account| {
                    self.directory
                        .account(account)
                        .is_some_and(|entry| entry.provider_kind() == provider)
                })
            });
            restricted.allowed_account_ids = Some(Arc::new(allowed_account_ids));
        }
        restricted.provider_kinds = Arc::new(provider_kinds);
        restricted
    }

    /// 排除父 attempt 当前已经持有的账号，避免嵌套调用形成账号 lease 自等待
    #[must_use]
    pub fn excluding_account(&self, account: ProviderAccountId) -> Self {
        let mut restricted = self.clone();
        let mut excluded = self.excluded_account_ids.as_ref().clone();
        excluded.insert(account);
        restricted.excluded_account_ids = Arc::new(excluded);
        restricted
    }

    #[must_use]
    pub fn allows(&self, account_id: &ProviderAccountId) -> bool {
        let Some(account) = self.directory.account(account_id) else {
            return false;
        };
        if self.excluded_account_ids.contains(account_id)
            || !self.provider_kinds.contains(account.provider_kind())
            || self
                .allowed_account_ids
                .as_ref()
                .is_some_and(|accounts| !accounts.contains(account_id))
        {
            return false;
        }
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
    pub fn allows_model(&self, account_id: &ProviderAccountId, upstream_model: &str) -> bool {
        self.allows(account_id)
            && self
                .directory
                .account(account_id)
                .is_some_and(|account| account.model_access.allows(upstream_model))
    }

    /// 目录按整个授权账号池过滤，不能只看用于获取元数据的账号政策
    #[must_use]
    pub fn allows_provider_model(&self, provider: &ProviderKind, upstream_model: &str) -> bool {
        self.provider_kinds.contains(provider)
            && self.directory.accounts.iter().any(|(id, account)| {
                account.provider_kind() == provider && self.allows_model(id, upstream_model)
            })
    }

    #[must_use]
    pub fn provider_kinds(&self) -> &BTreeSet<ProviderKind> {
        &self.provider_kinds
    }

    /// 返回冻结目录中账号所属 Provider；调用方仍须另行检查 [`Self::allows`]
    #[must_use]
    pub fn account_provider(&self, account_id: &ProviderAccountId) -> Option<&ProviderKind> {
        self.directory
            .account(account_id)
            .map(RuntimeAccount::provider_kind)
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
