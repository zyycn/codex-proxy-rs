//! Provider 运行时所需的中立存储能力。

use std::collections::BTreeMap;
use std::fmt;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use futures::future::BoxFuture;

use crate::engine::credential::{
    AccountAvailability, AccountFeedbackStats, AccountRuntimeSignals, CredentialRevision,
    OpaqueProviderData, ProviderAccountId, ProviderAccountStore,
};
use crate::routing::ProviderKind;

const MAX_PENDING_FLOW_TTL: Duration = Duration::from_secs(30 * 60);

/// Provider 可据此决定是否重试，但看不到 SQL、Redis 或秘密原文。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderStoreErrorKind {
    Unavailable,
    InvalidData,
    Conflict,
}

/// Provider 存储端口的脱敏错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("provider store {operation} failed: {kind:?}")]
pub struct ProviderStoreError {
    kind: ProviderStoreErrorKind,
    operation: &'static str,
}

impl ProviderStoreError {
    #[must_use]
    pub const fn new(kind: ProviderStoreErrorKind, operation: &'static str) -> Self {
        Self { kind, operation }
    }

    #[must_use]
    pub const fn kind(&self) -> ProviderStoreErrorKind {
        self.kind
    }
}

/// 一个 Provider 的完整可重建调度状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSchedulingState {
    signals: BTreeMap<ProviderAccountId, AccountRuntimeSignals>,
    round_robin_cursor: u64,
}

impl ProviderSchedulingState {
    #[must_use]
    pub const fn new(
        signals: BTreeMap<ProviderAccountId, AccountRuntimeSignals>,
        round_robin_cursor: u64,
    ) -> Self {
        Self {
            signals,
            round_robin_cursor,
        }
    }

    #[must_use]
    pub const fn signals(&self) -> &BTreeMap<ProviderAccountId, AccountRuntimeSignals> {
        &self.signals
    }

    #[must_use]
    pub const fn round_robin_cursor(&self) -> u64 {
        self.round_robin_cursor
    }
}

/// 请求级账号 lease 的全部中立事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSchedulingLeaseRequest {
    provider_kind: ProviderKind,
    account_id: ProviderAccountId,
    credential_revision: CredentialRevision,
    max_concurrent: NonZeroU32,
    request_interval: Duration,
    deadline: SystemTime,
}

impl ProviderSchedulingLeaseRequest {
    #[must_use]
    pub const fn new(
        provider_kind: ProviderKind,
        account_id: ProviderAccountId,
        credential_revision: CredentialRevision,
        max_concurrent: NonZeroU32,
        request_interval: Duration,
        deadline: SystemTime,
    ) -> Self {
        Self {
            provider_kind,
            account_id,
            credential_revision,
            max_concurrent,
            request_interval,
            deadline,
        }
    }

    #[must_use]
    pub const fn provider_kind(&self) -> &ProviderKind {
        &self.provider_kind
    }

    #[must_use]
    pub const fn account_id(&self) -> &ProviderAccountId {
        &self.account_id
    }

    #[must_use]
    pub const fn credential_revision(&self) -> CredentialRevision {
        self.credential_revision
    }

    #[must_use]
    pub const fn max_concurrent(&self) -> NonZeroU32 {
        self.max_concurrent
    }

    #[must_use]
    pub const fn request_interval(&self) -> Duration {
        self.request_interval
    }

    #[must_use]
    pub const fn deadline(&self) -> SystemTime {
        self.deadline
    }
}

/// Lease 生命周期由具体 Store guard 管理，Provider 只能持有。
pub trait ProviderLeaseGuard: Send + Sync + 'static {}

impl<T> ProviderLeaseGuard for T where T: Send + Sync + 'static {}

pub enum ProviderLeaseAcquisition {
    Acquired(Box<dyn ProviderLeaseGuard>),
    Busy { retry_after: Option<Duration> },
}

impl fmt::Debug for ProviderLeaseAcquisition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Acquired(_) => formatter.write_str("Acquired([LEASE])"),
            Self::Busy { retry_after } => formatter
                .debug_struct("Busy")
                .field("retry_after", retry_after)
                .finish(),
        }
    }
}

/// Provider 运行时会持有的三类 lease；刷新必须同时持有全局容量与账号互斥 lease。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderLeaseRequest {
    Scheduling(ProviderSchedulingLeaseRequest),
    RefreshCapacity(ProviderRefreshCapacityRequest),
    Refresh(ProviderRefreshLeaseRequest),
}

pub trait ProviderLeasePort: Send + Sync {
    fn load_state<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        accounts: &'a [ProviderAccountId],
    ) -> BoxFuture<'a, Result<ProviderSchedulingState, ProviderStoreError>>;

    fn try_acquire(
        &self,
        request: ProviderLeaseRequest,
    ) -> BoxFuture<'_, Result<ProviderLeaseAcquisition, ProviderStoreError>>;
}

/// Provider 从原始会话锚点派生的不可逆亲和键。
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderSessionAffinityKey(String);

impl ProviderSessionAffinityKey {
    pub fn try_new(value: impl Into<String>) -> Result<Self, ProviderStoreError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 128
            || !value.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
            })
        {
            return Err(ProviderStoreError::new(
                ProviderStoreErrorKind::InvalidData,
                "validate provider session affinity key",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn expose_to_store(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ProviderSessionAffinityKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProviderSessionAffinityKey([OPAQUE])")
    }
}

/// 可丢失的会话到账号偏好；Provider 负责先把原始会话标识哈希为不透明键。
pub trait ProviderSessionAffinityPort: Send + Sync {
    fn load<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        key: &'a ProviderSessionAffinityKey,
    ) -> BoxFuture<'a, Result<Option<ProviderAccountId>, ProviderStoreError>>;

    fn bind<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        key: &'a ProviderSessionAffinityKey,
        account_id: &'a ProviderAccountId,
        ttl: Duration,
    ) -> BoxFuture<'a, Result<(), ProviderStoreError>>;

    fn clear<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        key: &'a ProviderSessionAffinityKey,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>>;
}

/// 所有 Provider 共享的 OAuth refresh 并发容量。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderRefreshCapacityRequest {
    max_concurrent: NonZeroU32,
}

impl ProviderRefreshCapacityRequest {
    #[must_use]
    pub const fn new(max_concurrent: NonZeroU32) -> Self {
        Self { max_concurrent }
    }

    #[must_use]
    pub const fn max_concurrent(self) -> NonZeroU32 {
        self.max_concurrent
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRefreshLeaseRequest {
    account_id: ProviderAccountId,
    credential_revision: CredentialRevision,
}

impl ProviderRefreshLeaseRequest {
    #[must_use]
    pub const fn new(
        account_id: ProviderAccountId,
        credential_revision: CredentialRevision,
    ) -> Self {
        Self {
            account_id,
            credential_revision,
        }
    }

    #[must_use]
    pub const fn account_id(&self) -> &ProviderAccountId {
        &self.account_id
    }

    #[must_use]
    pub const fn credential_revision(&self) -> CredentialRevision {
        self.credential_revision
    }
}

/// Opaque catalog cache 的平台、账号与 credential revision 作用域。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCatalogCacheKey {
    provider_kind: ProviderKind,
    account_id: ProviderAccountId,
    credential_revision: CredentialRevision,
}

impl ProviderCatalogCacheKey {
    #[must_use]
    pub const fn new(
        provider_kind: ProviderKind,
        account_id: ProviderAccountId,
        credential_revision: CredentialRevision,
    ) -> Self {
        Self {
            provider_kind,
            account_id,
            credential_revision,
        }
    }

    #[must_use]
    pub const fn provider_kind(&self) -> &ProviderKind {
        &self.provider_kind
    }

    #[must_use]
    pub const fn account_id(&self) -> &ProviderAccountId {
        &self.account_id
    }

    #[must_use]
    pub const fn credential_revision(&self) -> CredentialRevision {
        self.credential_revision
    }
}

pub trait ProviderCatalogCachePort: Send + Sync {
    fn replace<'a>(
        &'a self,
        key: &'a ProviderCatalogCacheKey,
        catalog: &'a OpaqueProviderData,
        ttl: Duration,
    ) -> BoxFuture<'a, Result<(), ProviderStoreError>>;

    fn read<'a>(
        &'a self,
        key: &'a ProviderCatalogCacheKey,
    ) -> BoxFuture<'a, Result<Option<OpaqueProviderData>, ProviderStoreError>>;
}

/// Redis 中可重建的账号状态投影。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCredentialState {
    account_id: ProviderAccountId,
    credential_revision: CredentialRevision,
    enabled: bool,
    availability: AccountAvailability,
    observed_at: SystemTime,
}

impl ProviderCredentialState {
    #[must_use]
    pub const fn new(
        account_id: ProviderAccountId,
        credential_revision: CredentialRevision,
        enabled: bool,
        availability: AccountAvailability,
        observed_at: SystemTime,
    ) -> Self {
        Self {
            account_id,
            credential_revision,
            enabled,
            availability,
            observed_at,
        }
    }

    #[must_use]
    pub const fn account_id(&self) -> &ProviderAccountId {
        &self.account_id
    }

    #[must_use]
    pub const fn credential_revision(&self) -> CredentialRevision {
        self.credential_revision
    }

    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    #[must_use]
    pub const fn availability(&self) -> AccountAvailability {
        self.availability
    }

    #[must_use]
    pub const fn observed_at(&self) -> SystemTime {
        self.observed_at
    }
}

pub trait ProviderCredentialStatePort: Send + Sync {
    fn replace(
        &self,
        state: ProviderCredentialState,
    ) -> BoxFuture<'_, Result<(), ProviderStoreError>>;

    fn read<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<Option<ProviderCredentialState>, ProviderStoreError>>;

    fn clear<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>>;
}

/// 临时 cooldown 只保存调度截止时间；原因事实仍由 PostgreSQL 账号状态持有。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCooldown {
    account_id: ProviderAccountId,
    credential_revision: CredentialRevision,
    until: SystemTime,
}

impl ProviderCooldown {
    #[must_use]
    pub const fn new(
        account_id: ProviderAccountId,
        credential_revision: CredentialRevision,
        until: SystemTime,
    ) -> Self {
        Self {
            account_id,
            credential_revision,
            until,
        }
    }

    #[must_use]
    pub const fn account_id(&self) -> &ProviderAccountId {
        &self.account_id
    }

    #[must_use]
    pub const fn credential_revision(&self) -> CredentialRevision {
        self.credential_revision
    }

    #[must_use]
    pub const fn until(&self) -> SystemTime {
        self.until
    }
}

pub trait ProviderCooldownPort: Send + Sync {
    fn put_if_later(
        &self,
        cooldown: ProviderCooldown,
    ) -> BoxFuture<'_, Result<bool, ProviderStoreError>>;

    fn read<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<Option<ProviderCooldown>, ProviderStoreError>>;

    fn clear<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
        through_revision: CredentialRevision,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderRefreshPolicy {
    margin: Duration,
    concurrency: NonZeroU32,
}

impl ProviderRefreshPolicy {
    pub fn try_new(margin: Duration, concurrency: NonZeroU32) -> Result<Self, ProviderStoreError> {
        if margin.is_zero() {
            return Err(ProviderStoreError::new(
                ProviderStoreErrorKind::InvalidData,
                "validate refresh policy",
            ));
        }
        Ok(Self {
            margin,
            concurrency,
        })
    }

    #[must_use]
    pub const fn margin(self) -> Duration {
        self.margin
    }

    #[must_use]
    pub const fn concurrency(self) -> NonZeroU32 {
        self.concurrency
    }

    /// 按账号稳定扰动刷新提前量；相同账号与过期时间跨进程得到同一执行时刻。
    pub fn next_attempt_at(
        self,
        account_id: &ProviderAccountId,
        access_token_expires_at: SystemTime,
        observed_at: SystemTime,
    ) -> Result<SystemTime, ProviderStoreError> {
        let remaining = access_token_expires_at
            .duration_since(observed_at)
            .map_err(|_| invalid_refresh_policy("schedule expired access token"))?;
        let base_margin_seconds = self.margin.as_secs();
        if base_margin_seconds == 0 {
            return Ok(observed_at);
        }

        let factor = stable_factor(account_id.as_str(), "normal", 850, 1_150);
        let jittered_seconds = base_margin_seconds
            .saturating_mul(u64::from(factor))
            .saturating_add(500)
            / 1_000;
        if jittered_seconds >= remaining.as_secs() {
            return Ok(observed_at);
        }
        let lead = jittered_seconds.max(1);
        access_token_expires_at
            .checked_sub(Duration::from_secs(lead))
            .ok_or_else(|| invalid_refresh_policy("schedule access token refresh"))
    }
}

/// 临时失败后的持久重试时刻；稳定扰动避免多实例同频重试。
pub fn provider_refresh_retry_at(
    account_id: &ProviderAccountId,
    observed_at: SystemTime,
    base_delay: Duration,
    reason: &'static str,
) -> Result<SystemTime, ProviderStoreError> {
    if base_delay.is_zero() {
        return Err(invalid_refresh_policy("schedule refresh retry"));
    }
    let factor = stable_factor(account_id.as_str(), reason, 800, 1_200);
    let millis = u64::try_from(base_delay.as_millis())
        .unwrap_or(u64::MAX)
        .saturating_mul(u64::from(factor))
        .saturating_add(500)
        / 1_000;
    observed_at
        .checked_add(Duration::from_millis(millis.max(1)))
        .ok_or_else(|| invalid_refresh_policy("schedule refresh retry"))
}

fn stable_factor(value: &str, salt: &str, minimum: u32, maximum: u32) -> u32 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in value.bytes().chain([0]).chain(salt.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let width = u64::from(maximum - minimum) + 1;
    minimum + u32::try_from(hash % width).unwrap_or_default()
}

fn invalid_refresh_policy(operation: &'static str) -> ProviderStoreError {
    ProviderStoreError::new(ProviderStoreErrorKind::InvalidData, operation)
}

pub trait ProviderRuntimePolicyPort: Send + Sync {
    fn load_refresh_policy(
        &self,
    ) -> BoxFuture<'_, Result<ProviderRefreshPolicy, ProviderStoreError>>;
}

/// OAuth pending flow 的原始绑定只在 Provider 与 Store 边界内短暂存在。
#[derive(Clone, PartialEq, Eq)]
pub struct OAuthPendingBinding(String);

impl OAuthPendingBinding {
    pub fn try_new(value: impl Into<String>) -> Result<Self, ProviderStoreError> {
        let value = value.into();
        if value.is_empty() || value.len() > 512 {
            return Err(ProviderStoreError::new(
                ProviderStoreErrorKind::InvalidData,
                "validate OAuth pending binding",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn expose_to_store(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for OAuthPendingBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OAuthPendingBinding([REDACTED])")
    }
}

#[derive(Clone, PartialEq)]
pub struct NewOAuthPendingFlow {
    provider_kind: ProviderKind,
    flow: OAuthPendingBinding,
    owner: OAuthPendingBinding,
    ttl: Duration,
    payload: OpaqueProviderData,
}

impl NewOAuthPendingFlow {
    pub fn try_new(
        provider_kind: ProviderKind,
        flow: OAuthPendingBinding,
        owner: OAuthPendingBinding,
        ttl: Duration,
        payload: OpaqueProviderData,
    ) -> Result<Self, ProviderStoreError> {
        if ttl.is_zero() || ttl > MAX_PENDING_FLOW_TTL {
            return Err(ProviderStoreError::new(
                ProviderStoreErrorKind::InvalidData,
                "validate OAuth pending TTL",
            ));
        }
        Ok(Self {
            provider_kind,
            flow,
            owner,
            ttl,
            payload,
        })
    }

    #[must_use]
    pub const fn provider_kind(&self) -> &ProviderKind {
        &self.provider_kind
    }

    #[must_use]
    pub const fn flow(&self) -> &OAuthPendingBinding {
        &self.flow
    }

    #[must_use]
    pub const fn owner(&self) -> &OAuthPendingBinding {
        &self.owner
    }

    #[must_use]
    pub const fn ttl(&self) -> Duration {
        self.ttl
    }

    #[must_use]
    pub const fn payload(&self) -> &OpaqueProviderData {
        &self.payload
    }
}

impl fmt::Debug for NewOAuthPendingFlow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NewOAuthPendingFlow")
            .field("provider_kind", &self.provider_kind)
            .field("flow", &self.flow)
            .field("owner", &self.owner)
            .field("ttl", &self.ttl)
            .field("payload", &"[PROVIDER-OWNED]")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthPendingPutOutcome {
    Stored,
    AlreadyExists,
}

#[derive(Clone, PartialEq)]
pub enum OAuthPendingTakeOutcome {
    Taken(OpaqueProviderData),
    NotFound,
    OwnerMismatch,
}

impl fmt::Debug for OAuthPendingTakeOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Taken(_) => formatter.write_str("Taken([PROVIDER-OWNED])"),
            Self::NotFound => formatter.write_str("NotFound"),
            Self::OwnerMismatch => formatter.write_str("OwnerMismatch"),
        }
    }
}

pub trait OAuthPendingFlowPort: Send + Sync {
    fn put_if_absent(
        &self,
        flow: NewOAuthPendingFlow,
    ) -> BoxFuture<'_, Result<OAuthPendingPutOutcome, ProviderStoreError>>;

    fn take_if_owner<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        flow: &'a OAuthPendingBinding,
        owner: &'a OAuthPendingBinding,
    ) -> BoxFuture<'a, Result<OAuthPendingTakeOutcome, ProviderStoreError>>;
}

/// Provider 只能按能力取用端口，无法取得 Redis client 或 repository 集合。
#[derive(Clone)]
pub struct ProviderStorePorts {
    accounts: Arc<dyn ProviderAccountStore>,
    leases: Arc<dyn ProviderLeasePort>,
    session_affinity: Arc<dyn ProviderSessionAffinityPort>,
    account_feedback: Arc<AccountFeedbackStats>,
    catalog_cache: Arc<dyn ProviderCatalogCachePort>,
    credential_state: Arc<dyn ProviderCredentialStatePort>,
    cooldowns: Arc<dyn ProviderCooldownPort>,
    runtime_policy: Arc<dyn ProviderRuntimePolicyPort>,
    oauth_pending: Arc<dyn OAuthPendingFlowPort>,
}

impl ProviderStorePorts {
    #[must_use]
    // 每个参数代表独立能力端口；合并为单一配置对象会隐藏 Provider 能力边界。
    #[expect(clippy::too_many_arguments)]
    pub fn new(
        accounts: Arc<dyn ProviderAccountStore>,
        leases: Arc<dyn ProviderLeasePort>,
        session_affinity: Arc<dyn ProviderSessionAffinityPort>,
        catalog_cache: Arc<dyn ProviderCatalogCachePort>,
        credential_state: Arc<dyn ProviderCredentialStatePort>,
        cooldowns: Arc<dyn ProviderCooldownPort>,
        runtime_policy: Arc<dyn ProviderRuntimePolicyPort>,
        oauth_pending: Arc<dyn OAuthPendingFlowPort>,
    ) -> Self {
        Self {
            accounts,
            leases,
            session_affinity,
            account_feedback: Arc::new(AccountFeedbackStats::default()),
            catalog_cache,
            credential_state,
            cooldowns,
            runtime_policy,
            oauth_pending,
        }
    }

    #[must_use]
    pub fn accounts(&self) -> Arc<dyn ProviderAccountStore> {
        Arc::clone(&self.accounts)
    }

    #[must_use]
    pub fn leases(&self) -> Arc<dyn ProviderLeasePort> {
        Arc::clone(&self.leases)
    }

    #[must_use]
    pub fn session_affinity(&self) -> Arc<dyn ProviderSessionAffinityPort> {
        Arc::clone(&self.session_affinity)
    }

    #[must_use]
    pub fn account_feedback(&self) -> Arc<AccountFeedbackStats> {
        Arc::clone(&self.account_feedback)
    }

    #[must_use]
    pub fn catalog_cache(&self) -> Arc<dyn ProviderCatalogCachePort> {
        Arc::clone(&self.catalog_cache)
    }

    #[must_use]
    pub fn credential_state(&self) -> Arc<dyn ProviderCredentialStatePort> {
        Arc::clone(&self.credential_state)
    }

    #[must_use]
    pub fn cooldowns(&self) -> Arc<dyn ProviderCooldownPort> {
        Arc::clone(&self.cooldowns)
    }

    #[must_use]
    pub fn runtime_policy(&self) -> Arc<dyn ProviderRuntimePolicyPort> {
        Arc::clone(&self.runtime_policy)
    }

    #[must_use]
    pub fn oauth_pending(&self) -> Arc<dyn OAuthPendingFlowPort> {
        Arc::clone(&self.oauth_pending)
    }
}

impl fmt::Debug for ProviderStorePorts {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProviderStorePorts([CAPABILITIES])")
    }
}
