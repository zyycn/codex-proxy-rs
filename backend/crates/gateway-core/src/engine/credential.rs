//! Provider 账号、明文 credential port 与同一 target 内的账号选择。

use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::num::{NonZeroU32, NonZeroU64};
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use serde_json::{Map, Value};
use thiserror::Error;

use crate::error::{IdentifierError, StoreError, validate_text};
use crate::routing::ProviderKind;

/// `provider_accounts.id` 的核心值对象。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderAccountId(String);

impl ProviderAccountId {
    /// 校验并创建账号 ID。
    ///
    /// # Errors
    ///
    /// ID 缺少 `acct_` 前缀或不满足通用文本约束时返回错误。
    pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = value.into();
        validate_text(&value, 128, false, Some("acct_"))?;
        Ok(Self(value))
    }

    /// 返回数据库 ID 文本。
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProviderAccountId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// `provider_accounts.credential_revision` 的正数 CAS revision。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CredentialRevision(NonZeroU64);

impl CredentialRevision {
    /// 创建正数 revision。
    ///
    /// # Errors
    ///
    /// `value` 为零时返回错误。
    pub fn new(value: u64) -> Result<Self, CredentialError> {
        NonZeroU64::new(value)
            .map(Self)
            .ok_or(CredentialError::InvalidRevision)
    }

    /// 返回 revision 数值。
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }

    /// 返回下一个 revision；溢出时返回错误。
    ///
    /// # Errors
    ///
    /// 当前 revision 已是 `u64::MAX` 时返回错误。
    pub fn next(self) -> Result<Self, CredentialError> {
        self.get()
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .map(Self)
            .ok_or(CredentialError::RevisionOverflow)
    }
}

/// Provider-owned 的明文 credential JSON。
///
/// Core 只保证顶层是 object，绝不读取其中的 AT、RT、Cookie 或 Provider key。
#[derive(Clone, PartialEq)]
pub struct PlaintextCredential(Map<String, Value>);

impl PlaintextCredential {
    /// 接受由具体 Provider 完整校验后的 JSON object。
    #[must_use]
    pub const fn new(value: Map<String, Value>) -> Self {
        Self(value)
    }

    /// 将明文 object 借给对应 Provider adapter。
    #[must_use]
    pub const fn expose_to_provider(&self) -> &Map<String, Value> {
        &self.0
    }

    /// 将明文 object 交给 Store adapter 持久化。
    #[must_use]
    pub fn into_inner(self) -> Map<String, Value> {
        self.0
    }
}

impl fmt::Debug for PlaintextCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaintextCredential")
            .field("keys", &self.0.keys().collect::<Vec<_>>())
            .field("values", &"<redacted>")
            .finish()
    }
}

/// Provider-owned 的任意 JSON object；公共层只搬运、不读取内部 key。
#[derive(Clone, PartialEq)]
pub struct OpaqueProviderData(Map<String, Value>);

impl OpaqueProviderData {
    #[must_use]
    pub const fn new(value: Map<String, Value>) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn expose_to_provider(&self) -> &Map<String, Value> {
        &self.0
    }

    #[must_use]
    pub fn into_inner(self) -> Map<String, Value> {
        self.0
    }
}

impl fmt::Debug for OpaqueProviderData {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpaqueProviderData")
            .field("keys", &self.0.keys().collect::<Vec<_>>())
            .field("values", &"<provider-owned>")
            .finish()
    }
}

/// 数据库中固定的账号运行状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountAvailability {
    Unknown,
    Ready,
    Cooldown,
    QuotaExhausted,
    Expired,
    Banned,
    Invalid,
}

impl AccountAvailability {
    /// 返回数据库稳定值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Ready => "ready",
            Self::Cooldown => "cooldown",
            Self::QuotaExhausted => "quota_exhausted",
            Self::Expired => "expired",
            Self::Banned => "banned",
            Self::Invalid => "invalid",
        }
    }

    /// 解析数据库稳定值。
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "unknown" => Some(Self::Unknown),
            "ready" => Some(Self::Ready),
            "cooldown" => Some(Self::Cooldown),
            "quota_exhausted" => Some(Self::QuotaExhausted),
            "expired" => Some(Self::Expired),
            "banned" => Some(Self::Banned),
            "invalid" => Some(Self::Invalid),
            _ => None,
        }
    }
}

/// 不含 secret 的账号持久事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAccount {
    id: ProviderAccountId,
    provider: ProviderKind,
    name: String,
    email: Option<String>,
    upstream_user_id: String,
    upstream_account_id: Option<String>,
    plan_type: Option<String>,
    authentication_kind: String,
    revision: CredentialRevision,
    enabled: bool,
    availability: AccountAvailability,
    cooldown_until: Option<SystemTime>,
    access_token_expires_at: Option<SystemTime>,
    next_refresh_at: Option<SystemTime>,
    has_refresh_token: bool,
}

impl ProviderAccount {
    /// 创建账号快照。
    #[must_use]
    pub const fn new(
        id: ProviderAccountId,
        provider: ProviderKind,
        name: String,
        upstream_user_id: String,
        authentication_kind: String,
        revision: CredentialRevision,
        access_token_expires_at: Option<SystemTime>,
    ) -> Self {
        Self {
            id,
            provider,
            name,
            email: None,
            upstream_user_id,
            upstream_account_id: None,
            plan_type: None,
            authentication_kind,
            revision,
            enabled: true,
            availability: AccountAvailability::Unknown,
            cooldown_until: None,
            access_token_expires_at,
            next_refresh_at: None,
            has_refresh_token: false,
        }
    }

    #[must_use]
    pub fn with_profile(
        mut self,
        email: Option<String>,
        upstream_account_id: Option<String>,
        plan_type: Option<String>,
    ) -> Self {
        self.email = email;
        self.upstream_account_id = upstream_account_id;
        self.plan_type = plan_type;
        self
    }

    #[must_use]
    pub const fn with_runtime_state(
        mut self,
        enabled: bool,
        availability: AccountAvailability,
        cooldown_until: Option<SystemTime>,
    ) -> Self {
        self.enabled = enabled;
        self.availability = availability;
        self.cooldown_until = cooldown_until;
        self
    }

    #[must_use]
    pub const fn with_refresh_schedule(
        mut self,
        has_refresh_token: bool,
        next_refresh_at: Option<SystemTime>,
    ) -> Self {
        self.has_refresh_token = has_refresh_token;
        self.next_refresh_at = next_refresh_at;
        self
    }

    #[must_use]
    pub const fn id(&self) -> &ProviderAccountId {
        &self.id
    }

    #[must_use]
    pub const fn provider(&self) -> &ProviderKind {
        &self.provider
    }

    #[must_use]
    pub const fn revision(&self) -> CredentialRevision {
        self.revision
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn email(&self) -> Option<&str> {
        self.email.as_deref()
    }

    #[must_use]
    pub fn upstream_user_id(&self) -> &str {
        &self.upstream_user_id
    }

    #[must_use]
    pub fn upstream_account_id(&self) -> Option<&str> {
        self.upstream_account_id.as_deref()
    }

    #[must_use]
    pub fn plan_type(&self) -> Option<&str> {
        self.plan_type.as_deref()
    }

    #[must_use]
    pub fn authentication_kind(&self) -> &str {
        &self.authentication_kind
    }

    #[must_use]
    pub const fn availability(&self) -> AccountAvailability {
        self.availability
    }

    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    #[must_use]
    pub const fn cooldown_until(&self) -> Option<SystemTime> {
        self.cooldown_until
    }

    #[must_use]
    pub const fn access_token_expires_at(&self) -> Option<SystemTime> {
        self.access_token_expires_at
    }

    #[must_use]
    pub const fn next_refresh_at(&self) -> Option<SystemTime> {
        self.next_refresh_at
    }

    #[must_use]
    pub const fn has_refresh_token(&self) -> bool {
        self.has_refresh_token
    }

    /// 判断账号当前能否进入同 target 的候选池。
    #[must_use]
    pub fn is_schedulable(&self, now: SystemTime) -> bool {
        let available = match self.availability {
            AccountAvailability::Ready => true,
            AccountAvailability::Cooldown => self.cooldown_until.is_some_and(|until| until <= now),
            AccountAvailability::Unknown
            | AccountAvailability::QuotaExhausted
            | AccountAvailability::Expired
            | AccountAvailability::Banned
            | AccountAvailability::Invalid => false,
        };
        self.enabled
            && available
            && self
                .access_token_expires_at
                .is_none_or(|expires_at| expires_at > now)
    }
}

/// Store 读出的账号与 Provider-owned 明文 credential。
#[derive(Clone, PartialEq)]
pub struct LoadedCredential {
    pub account: ProviderAccount,
    pub credential: PlaintextCredential,
}

/// Admin/Provider import 创建账号时的一次性明文输入。
#[derive(Clone, PartialEq)]
pub struct NewProviderAccount {
    pub account: ProviderAccount,
    pub credential: PlaintextCredential,
}

impl fmt::Debug for NewProviderAccount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NewProviderAccount")
            .field("account", &self.account)
            .field("credential", &self.credential)
            .finish()
    }
}

/// 不改 credential revision 的管理字段更新。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAccountUpdate {
    pub account_id: ProviderAccountId,
    pub name: String,
    pub email: Option<String>,
    pub plan_type: Option<String>,
}

impl fmt::Debug for LoadedCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoadedCredential")
            .field("account", &self.account)
            .field("credential", &self.credential)
            .finish()
    }
}

/// 刷新成功后的完整 CAS 写回。
#[derive(Clone, PartialEq)]
pub struct CredentialCasUpdate {
    account_id: ProviderAccountId,
    expected_revision: CredentialRevision,
    profile: ProviderAccountUpdate,
    credential: PlaintextCredential,
    has_refresh_token: bool,
    access_token_expires_at: Option<SystemTime>,
    next_refresh_at: Option<SystemTime>,
}

impl fmt::Debug for CredentialCasUpdate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialCasUpdate")
            .field("account_id", &self.account_id)
            .field("expected_revision", &self.expected_revision)
            .field("profile", &self.profile)
            .field("credential", &self.credential)
            .field("has_refresh_token", &self.has_refresh_token)
            .field("access_token_expires_at", &self.access_token_expires_at)
            .field("next_refresh_at", &self.next_refresh_at)
            .finish()
    }
}

impl CredentialCasUpdate {
    /// 创建同一账号 revision fence 下的完整 credential + 普通投影写回。
    ///
    /// # Errors
    ///
    /// profile 与 credential 指向不同账号，或无 RT 却声明下次刷新时间时失败。
    pub fn new(
        account_id: ProviderAccountId,
        expected_revision: CredentialRevision,
        profile: ProviderAccountUpdate,
        credential: PlaintextCredential,
        has_refresh_token: bool,
        access_token_expires_at: Option<SystemTime>,
        next_refresh_at: Option<SystemTime>,
    ) -> Result<Self, CredentialError> {
        if profile.account_id != account_id {
            return Err(CredentialError::ProfileAccountMismatch);
        }
        if !has_refresh_token && next_refresh_at.is_some() {
            return Err(CredentialError::InvalidRefreshSchedule);
        }
        Ok(Self {
            account_id,
            expected_revision,
            profile,
            credential,
            has_refresh_token,
            access_token_expires_at,
            next_refresh_at,
        })
    }

    #[must_use]
    pub const fn account_id(&self) -> &ProviderAccountId {
        &self.account_id
    }

    #[must_use]
    pub const fn expected_revision(&self) -> CredentialRevision {
        self.expected_revision
    }

    #[must_use]
    pub const fn profile(&self) -> &ProviderAccountUpdate {
        &self.profile
    }

    #[must_use]
    pub const fn credential(&self) -> &PlaintextCredential {
        &self.credential
    }

    #[must_use]
    pub const fn has_refresh_token(&self) -> bool {
        self.has_refresh_token
    }

    #[must_use]
    pub const fn access_token_expires_at(&self) -> Option<SystemTime> {
        self.access_token_expires_at
    }

    #[must_use]
    pub const fn next_refresh_at(&self) -> Option<SystemTime> {
        self.next_refresh_at
    }

    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        ProviderAccountId,
        CredentialRevision,
        ProviderAccountUpdate,
        PlaintextCredential,
        bool,
        Option<SystemTime>,
        Option<SystemTime>,
    ) {
        (
            self.account_id,
            self.expected_revision,
            self.profile,
            self.credential,
            self.has_refresh_token,
            self.access_token_expires_at,
            self.next_refresh_at,
        )
    }
}

/// CAS 写回结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialCasOutcome {
    Updated(CredentialRevision),
    Conflict,
}

/// Provider quota 的一次完整观察结果。
#[derive(Clone, PartialEq)]
pub struct QuotaObservation {
    pub account_id: ProviderAccountId,
    pub expected_revision: CredentialRevision,
    pub quota: Option<OpaqueProviderData>,
    pub observed_at: Option<SystemTime>,
}

impl fmt::Debug for QuotaObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QuotaObservation")
            .field("account_id", &self.account_id)
            .field("expected_revision", &self.expected_revision)
            .field("quota", &self.quota)
            .field("observed_at", &self.observed_at)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaWriteOutcome {
    Updated,
    Conflict,
}

/// 账号状态的 revision-fenced 写入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountStateChange {
    pub account_id: ProviderAccountId,
    pub expected_revision: CredentialRevision,
    pub availability: AccountAvailability,
    pub reason: Option<String>,
    pub cooldown_until: Option<SystemTime>,
    pub observed_at: SystemTime,
}

/// `provider_accounts` 的数据库中立端口。
#[async_trait]
pub trait ProviderAccountStore: Send + Sync {
    async fn create_account(&self, account: NewProviderAccount) -> Result<(), StoreError>;

    async fn get_account(
        &self,
        account: &ProviderAccountId,
    ) -> Result<Option<ProviderAccount>, StoreError>;

    async fn list_accounts(&self) -> Result<Vec<ProviderAccount>, StoreError>;

    async fn list_for_provider(
        &self,
        provider: &ProviderKind,
    ) -> Result<Vec<ProviderAccount>, StoreError>;

    async fn load_credential(
        &self,
        account: &ProviderAccountId,
        expected_revision: CredentialRevision,
    ) -> Result<LoadedCredential, StoreError>;

    async fn compare_and_swap_credential(
        &self,
        update: CredentialCasUpdate,
    ) -> Result<CredentialCasOutcome, StoreError>;

    async fn get_quotas(
        &self,
        accounts: &[ProviderAccountId],
    ) -> Result<Vec<QuotaObservation>, StoreError>;

    async fn compare_and_swap_quota(
        &self,
        observation: QuotaObservation,
    ) -> Result<QuotaWriteOutcome, StoreError>;

    async fn apply_state_change(&self, change: AccountStateChange) -> Result<(), StoreError>;

    async fn update_account(&self, update: ProviderAccountUpdate) -> Result<(), StoreError>;

    async fn set_enabled(
        &self,
        account: &ProviderAccountId,
        enabled: bool,
    ) -> Result<(), StoreError>;

    async fn delete_account(&self, account: &ProviderAccountId) -> Result<(), StoreError>;
}

/// `runtime_settings.rotation_strategy` 的稳定值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RotationStrategy {
    Smart,
    QuotaResetPriority,
    RoundRobin,
    Sticky,
}

impl RotationStrategy {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Smart => "smart",
            Self::QuotaResetPriority => "quota_reset_priority",
            Self::RoundRobin => "round_robin",
            Self::Sticky => "sticky",
        }
    }
}

/// 从 `runtime_settings` 冻结到一次请求计划的账号调度策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountSelectionPolicy {
    strategy: RotationStrategy,
    max_concurrent_per_account: NonZeroU32,
    request_interval: Duration,
}

impl AccountSelectionPolicy {
    #[must_use]
    pub const fn new(
        strategy: RotationStrategy,
        max_concurrent_per_account: NonZeroU32,
        request_interval: Duration,
    ) -> Self {
        Self {
            strategy,
            max_concurrent_per_account,
            request_interval,
        }
    }

    #[must_use]
    pub const fn strategy(self) -> RotationStrategy {
        self.strategy
    }

    #[must_use]
    pub const fn max_concurrent_per_account(self) -> NonZeroU32 {
        self.max_concurrent_per_account
    }

    #[must_use]
    pub const fn request_interval(self) -> Duration {
        self.request_interval
    }
}

/// Store 提供并发事实，Provider 叠加自己解释的额度事实；全部信号均可重建。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRuntimeSignals {
    pub in_flight: u32,
    pub last_started_at: Option<SystemTime>,
    pub quota_reset_at: Option<SystemTime>,
    pub quota_remaining_rank: Option<u64>,
    pub failure_rate_basis_points: Option<u16>,
    pub first_output_latency_ms: Option<u64>,
}

const ACCOUNT_FEEDBACK_EWMA_ALPHA: f64 = 0.2;
const EMPTY_FEEDBACK_SAMPLE: u64 = f64::NAN.to_bits();

/// 一次真实上游 attempt 对账号级 Smart 调度产生的中立反馈。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountAttemptFeedback {
    Succeeded { first_output_ms: Option<u64> },
    Failed { first_output_ms: Option<u64> },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AccountFeedbackKey {
    provider_kind: ProviderKind,
    account_id: ProviderAccountId,
}

#[derive(Debug)]
struct AccountFeedback {
    failure_rate: AtomicU64,
    first_output_ms: AtomicU64,
}

impl Default for AccountFeedback {
    fn default() -> Self {
        Self {
            // 新账号从健康基线开始，首个失败样本按 EWMA 平滑，而不是直接判死。
            failure_rate: AtomicU64::new(0.0_f64.to_bits()),
            first_output_ms: AtomicU64::new(EMPTY_FEEDBACK_SAMPLE),
        }
    }
}

impl AccountFeedback {
    fn report(&self, feedback: AccountAttemptFeedback) {
        let (failure, first_output_ms) = match feedback {
            AccountAttemptFeedback::Succeeded { first_output_ms } => (0.0, first_output_ms),
            AccountAttemptFeedback::Failed { first_output_ms } => (1.0, first_output_ms),
        };
        update_feedback_ewma(&self.failure_rate, failure);
        if let Some(first_output_ms) = first_output_ms.filter(|value| *value > 0) {
            update_feedback_ewma(&self.first_output_ms, first_output_ms as f64);
        }
    }

    fn scheduling_signals(&self) -> (Option<u16>, Option<u64>) {
        let failure_rate = load_feedback_ewma(&self.failure_rate).map(|value| {
            let basis_points = (value.clamp(0.0, 1.0) * 10_000.0).round();
            basis_points as u16
        });
        let first_output_ms = load_feedback_ewma(&self.first_output_ms)
            .filter(|value| value.is_finite() && *value > 0.0)
            .map(|value| value.round() as u64);
        (failure_rate, first_output_ms)
    }
}

/// 所有 Provider 共享、按 Provider 和账号隔离的进程内 Smart 健康反馈。
#[derive(Debug, Default)]
pub struct AccountFeedbackStats {
    accounts: RwLock<HashMap<AccountFeedbackKey, AccountFeedback>>,
}

impl AccountFeedbackStats {
    /// 读取账号当前的错误率与首个有效输出延迟 EWMA。
    #[must_use]
    pub fn scheduling_signals(
        &self,
        provider_kind: &ProviderKind,
        account_id: &ProviderAccountId,
    ) -> (Option<u16>, Option<u64>) {
        let key = AccountFeedbackKey {
            provider_kind: provider_kind.clone(),
            account_id: account_id.clone(),
        };
        self.accounts
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
            .map(AccountFeedback::scheduling_signals)
            .unwrap_or_default()
    }

    /// 回灌一次已经真实发送的上游 attempt。
    pub fn report(
        &self,
        provider_kind: &ProviderKind,
        account_id: &ProviderAccountId,
        feedback: AccountAttemptFeedback,
    ) {
        let key = AccountFeedbackKey {
            provider_kind: provider_kind.clone(),
            account_id: account_id.clone(),
        };
        if let Some(account) = self
            .accounts
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
        {
            account.report(feedback);
            return;
        }
        self.accounts
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(key)
            .or_default()
            .report(feedback);
    }
}

fn load_feedback_ewma(value: &AtomicU64) -> Option<f64> {
    let value = f64::from_bits(value.load(Ordering::Relaxed));
    (!value.is_nan()).then_some(value)
}

fn update_feedback_ewma(target: &AtomicU64, sample: f64) {
    let mut current = target.load(Ordering::Relaxed);
    loop {
        let previous = f64::from_bits(current);
        let next = if previous.is_nan() {
            sample
        } else {
            ACCOUNT_FEEDBACK_EWMA_ALPHA * sample + (1.0 - ACCOUNT_FEEDBACK_EWMA_ALPHA) * previous
        };
        match target.compare_exchange_weak(
            current,
            next.to_bits(),
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return,
            Err(observed) => current = observed,
        }
    }
}

impl AccountRuntimeSignals {
    #[must_use]
    pub fn with_provider_quota(mut self, quota: Option<AccountQuotaSignals>) -> Self {
        if let Some(quota) = quota {
            self.quota_reset_at = quota.reset_at;
            self.quota_remaining_rank = quota.remaining_rank;
        }
        self
    }

    #[must_use]
    pub const fn with_runtime_health(
        mut self,
        failure_rate_basis_points: Option<u16>,
        first_output_latency_ms: Option<u64>,
    ) -> Self {
        self.failure_rate_basis_points = failure_rate_basis_points;
        self.first_output_latency_ms = first_output_latency_ms;
        self
    }
}

/// Provider 从私有 quota JSON 投影出的中立调度事实。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountQuotaSignals {
    reset_at: Option<SystemTime>,
    remaining_rank: Option<u64>,
}

impl AccountQuotaSignals {
    #[must_use]
    pub const fn new(reset_at: Option<SystemTime>, remaining_rank: Option<u64>) -> Self {
        Self {
            reset_at,
            remaining_rank,
        }
    }

    #[must_use]
    pub const fn reset_at(self) -> Option<SystemTime> {
        self.reset_at
    }

    #[must_use]
    pub const fn remaining_rank(self) -> Option<u64> {
        self.remaining_rank
    }
}

/// 账号持久事实与可重建运行信号的请求级组合。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountCandidate {
    pub account: ProviderAccount,
    pub signals: AccountRuntimeSignals,
}

/// 一次账号选择使用的全局策略快照。
#[derive(Debug, Clone)]
pub struct AccountSelectionContext {
    pub policy: AccountSelectionPolicy,
    pub now: SystemTime,
    pub excluded_accounts: BTreeSet<ProviderAccountId>,
    pub preferred_account: Option<ProviderAccountId>,
    pub round_robin_cursor: u64,
}

/// 同一 target 内唯一的账号排序器。
#[derive(Debug, Default, Clone, Copy)]
pub struct AccountSelector;

impl AccountSelector {
    /// 从可调度账号中确定一个候选；这里只消费 Provider 已解析的额度投影。
    #[must_use]
    pub fn select<'a>(
        &self,
        candidates: &'a [AccountCandidate],
        context: &AccountSelectionContext,
    ) -> Option<&'a AccountCandidate> {
        let mut eligible = candidates
            .iter()
            .filter(|candidate| eligible(candidate, context))
            .collect::<Vec<_>>();
        if eligible.is_empty() {
            return None;
        }

        if let Some(preferred) = context.preferred_account.as_ref()
            && let Some(candidate) = eligible
                .iter()
                .find(|candidate| candidate.account.id() == preferred)
            && (context.policy.strategy() == RotationStrategy::Sticky
                || (context.policy.strategy() == RotationStrategy::Smart
                    && !smart_preference_should_escape(&candidate.signals)))
        {
            return Some(candidate);
        }

        match context.policy.strategy() {
            RotationStrategy::QuotaResetPriority => eligible.sort_by_key(|candidate| {
                (
                    candidate.signals.quota_reset_at.is_none(),
                    candidate.signals.quota_reset_at,
                    candidate.signals.in_flight,
                    candidate.signals.last_started_at,
                    candidate.account.id().clone(),
                )
            }),
            RotationStrategy::RoundRobin => {
                eligible.sort_by_key(|candidate| candidate.account.id().clone());
                let index = context.round_robin_cursor as usize % eligible.len();
                return Some(eligible[index]);
            }
            RotationStrategy::Smart => {
                return select_smart_candidate(&eligible, context.round_robin_cursor);
            }
            RotationStrategy::Sticky => {
                eligible.sort_by_key(|candidate| {
                    (
                        Reverse(candidate.signals.last_started_at),
                        candidate.account.id().clone(),
                    )
                });
            }
        }
        eligible.into_iter().next()
    }
}

const SMART_LOAD_WEIGHT: f64 = 1.0;
const SMART_QUOTA_WEIGHT: f64 = 0.8;
const SMART_FAILURE_WEIGHT: f64 = 1.0;
const SMART_LATENCY_WEIGHT: f64 = 0.5;
const SMART_PREFERENCE_FAILURE_ESCAPE_BASIS_POINTS: u16 = 5_000;
const SMART_PREFERENCE_LATENCY_ESCAPE_MS: u64 = 15_000;

struct SmartNormalization {
    max_in_flight: u32,
    min_quota: Option<u64>,
    max_quota: Option<u64>,
    min_latency_ms: Option<u64>,
    max_latency_ms: Option<u64>,
}

impl SmartNormalization {
    fn from_candidates(candidates: &[&AccountCandidate]) -> Self {
        let max_in_flight = candidates
            .iter()
            .map(|candidate| candidate.signals.in_flight)
            .max()
            .unwrap_or_default();
        let min_quota = candidates
            .iter()
            .filter_map(|candidate| candidate.signals.quota_remaining_rank)
            .min();
        let max_quota = candidates
            .iter()
            .filter_map(|candidate| candidate.signals.quota_remaining_rank)
            .max();
        let min_latency_ms = candidates
            .iter()
            .filter_map(|candidate| candidate.signals.first_output_latency_ms)
            .filter(|latency| *latency > 0)
            .min();
        let max_latency_ms = candidates
            .iter()
            .filter_map(|candidate| candidate.signals.first_output_latency_ms)
            .filter(|latency| *latency > 0)
            .max();
        Self {
            max_in_flight,
            min_quota,
            max_quota,
            min_latency_ms,
            max_latency_ms,
        }
    }
}

fn select_smart_candidate<'a>(
    candidates: &[&'a AccountCandidate],
    cursor: u64,
) -> Option<&'a AccountCandidate> {
    let normalization = SmartNormalization::from_candidates(candidates);
    let mut ranked = candidates
        .iter()
        .map(|candidate| (*candidate, smart_score(candidate, &normalization)))
        .collect::<Vec<_>>();
    ranked.sort_by(|(left, left_score), (right, right_score)| {
        right_score
            .total_cmp(left_score)
            .then_with(|| left.account.id().cmp(right.account.id()))
    });
    let best_score = ranked.first()?.1.to_bits();
    let tied = ranked
        .iter()
        .take_while(|(_, score)| score.to_bits() == best_score)
        .count();
    let index = cursor as usize % tied;
    Some(ranked[index].0)
}

fn smart_score(candidate: &AccountCandidate, normalization: &SmartNormalization) -> f64 {
    let load = lower_is_better(
        u64::from(candidate.signals.in_flight),
        0,
        u64::from(normalization.max_in_flight),
    );
    let quota = candidate.signals.quota_remaining_rank.map_or(0.5, |quota| {
        higher_is_better(
            quota,
            normalization.min_quota.unwrap_or(quota),
            normalization.max_quota.unwrap_or(quota),
        )
    });
    let failure = 1.0
        - f64::from(
            candidate
                .signals
                .failure_rate_basis_points
                .unwrap_or_default()
                .min(10_000),
        ) / 10_000.0;
    let latency = candidate
        .signals
        .first_output_latency_ms
        .filter(|latency| *latency > 0)
        .map_or(1.0, |latency| {
            lower_is_better(
                latency,
                normalization.min_latency_ms.unwrap_or(latency),
                normalization.max_latency_ms.unwrap_or(latency),
            )
        });

    SMART_LOAD_WEIGHT * load
        + SMART_QUOTA_WEIGHT * quota
        + SMART_FAILURE_WEIGHT * failure
        + SMART_LATENCY_WEIGHT * latency
}

fn lower_is_better(value: u64, minimum: u64, maximum: u64) -> f64 {
    if maximum <= minimum {
        return 1.0;
    }
    1.0 - (value.saturating_sub(minimum) as f64 / (maximum - minimum) as f64).clamp(0.0, 1.0)
}

fn higher_is_better(value: u64, minimum: u64, maximum: u64) -> f64 {
    if maximum <= minimum {
        return 1.0;
    }
    (value.saturating_sub(minimum) as f64 / (maximum - minimum) as f64).clamp(0.0, 1.0)
}

fn smart_preference_should_escape(signals: &AccountRuntimeSignals) -> bool {
    signals
        .failure_rate_basis_points
        .is_some_and(|rate| rate > SMART_PREFERENCE_FAILURE_ESCAPE_BASIS_POINTS)
        || signals
            .first_output_latency_ms
            .is_some_and(|latency| latency > SMART_PREFERENCE_LATENCY_ESCAPE_MS)
}

fn eligible(candidate: &AccountCandidate, context: &AccountSelectionContext) -> bool {
    if !candidate.account.is_schedulable(context.now)
        || context.excluded_accounts.contains(candidate.account.id())
        || candidate.signals.in_flight >= context.policy.max_concurrent_per_account().get()
    {
        return false;
    }

    candidate
        .signals
        .last_started_at
        .is_none_or(|last_started| {
            context
                .now
                .duration_since(last_started)
                .is_ok_and(|elapsed| elapsed >= context.policy.request_interval())
        })
}

/// Credential 值对象构造错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CredentialError {
    #[error("credential revision must be greater than zero")]
    InvalidRevision,
    #[error("credential revision overflow")]
    RevisionOverflow,
    #[error("credential CAS profile belongs to a different account")]
    ProfileAccountMismatch,
    #[error("credential refresh schedule requires a refresh token")]
    InvalidRefreshSchedule,
}
