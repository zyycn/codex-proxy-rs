//! Provider 账号、明文 credential port 与同一 target 内的账号选择。

use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::num::{NonZeroU16, NonZeroU32, NonZeroU64};
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

/// Per-account scheduling concurrency override.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccountConcurrencyLimit(NonZeroU32);

impl AccountConcurrencyLimit {
    #[must_use]
    pub const fn new(value: u32) -> Option<Self> {
        match NonZeroU32::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }

    #[must_use]
    pub const fn into_non_zero(self) -> NonZeroU32 {
        self.0
    }
}

/// Relative scheduling priority for an account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccountWeight(NonZeroU16);

impl AccountWeight {
    pub const DEFAULT: Self = Self(NonZeroU16::MIN);
    pub const MAX: u16 = 100;

    #[must_use]
    pub const fn new(value: u16) -> Option<Self> {
        match NonZeroU16::new(value) {
            Some(value) if value.get() <= Self::MAX => Some(Self(value)),
            _ => None,
        }
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

impl Default for AccountWeight {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl fmt::Display for ProviderAccountId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// credential 轮换时可选替换的上游账号身份。
#[derive(Clone, PartialEq, Eq)]
pub struct ProviderAccountIdentity {
    upstream_user_id: String,
    upstream_account_id: Option<String>,
}

impl fmt::Debug for ProviderAccountIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderAccountIdentity")
            .field("upstream_user_id", &"<redacted>")
            .field(
                "upstream_account_id",
                &self.upstream_account_id.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

impl ProviderAccountIdentity {
    #[must_use]
    pub const fn new(upstream_user_id: String, upstream_account_id: Option<String>) -> Self {
        Self {
            upstream_user_id,
            upstream_account_id,
        }
    }

    #[must_use]
    pub fn upstream_user_id(&self) -> &str {
        &self.upstream_user_id
    }

    #[must_use]
    pub fn upstream_account_id(&self) -> Option<&str> {
        self.upstream_account_id.as_deref()
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

/// 已持久化的凭据/账号身份状态。
///
/// 这里只保存凭据与账号身份事实。额度耗尽属于 [`QuotaState`]，临时 429 属于
/// 运行时冷却；二者都不得写入此枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CredentialState {
    Unknown,
    Ready,
    Expired,
    Banned,
    Invalid,
}

impl CredentialState {
    /// 返回数据库稳定值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Ready => "ready",
            Self::Expired => "expired",
            Self::Banned => "banned",
            Self::Invalid => "invalid",
        }
    }

    /// 返回该凭据状态对应的稳定错误原因。
    #[must_use]
    pub const fn error_reason(self) -> Option<AccountErrorReason> {
        match self {
            Self::Unknown => Some(AccountErrorReason::AccountUnverified),
            Self::Expired => Some(AccountErrorReason::CredentialExpired),
            Self::Banned => Some(AccountErrorReason::AccountBanned),
            Self::Invalid => Some(AccountErrorReason::CredentialInvalid),
            Self::Ready => None,
        }
    }

    /// 解析数据库稳定值。
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "unknown" => Some(Self::Unknown),
            "ready" => Some(Self::Ready),
            "expired" => Some(Self::Expired),
            "banned" => Some(Self::Banned),
            "invalid" => Some(Self::Invalid),
            _ => None,
        }
    }
}

/// 额度访问结论；百分比和余额等展示值不进入此枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuotaAccessState {
    Unknown,
    Allowed,
    Exhausted,
}

impl QuotaAccessState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Allowed => "allowed",
            Self::Exhausted => "exhausted",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "unknown" => Some(Self::Unknown),
            "allowed" => Some(Self::Allowed),
            "exhausted" => Some(Self::Exhausted),
            _ => None,
        }
    }
}

/// 权威额度耗尽证据。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuotaEvidence {
    ProviderDenied,
    AccountLimitReached,
    UsageLimitReached,
    PaymentRequired,
}

impl QuotaEvidence {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProviderDenied => "provider_denied",
            Self::AccountLimitReached => "account_limit_reached",
            Self::UsageLimitReached => "usage_limit_reached",
            Self::PaymentRequired => "payment_required",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "provider_denied" => Some(Self::ProviderDenied),
            "account_limit_reached" => Some(Self::AccountLimitReached),
            "usage_limit_reached" => Some(Self::UsageLimitReached),
            "payment_required" => Some(Self::PaymentRequired),
            _ => None,
        }
    }
}

/// 可审计的额度访问事实。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuotaState {
    access: QuotaAccessState,
    evidence: Option<QuotaEvidence>,
    observed_at: Option<SystemTime>,
    reset_at: Option<SystemTime>,
}

impl Default for QuotaState {
    fn default() -> Self {
        Self::unknown()
    }
}

impl QuotaState {
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            access: QuotaAccessState::Unknown,
            evidence: None,
            observed_at: None,
            reset_at: None,
        }
    }

    #[must_use]
    pub const fn observed_unknown(observed_at: SystemTime) -> Self {
        Self {
            access: QuotaAccessState::Unknown,
            evidence: None,
            observed_at: Some(observed_at),
            reset_at: None,
        }
    }

    #[must_use]
    pub const fn allowed(observed_at: SystemTime) -> Self {
        Self {
            access: QuotaAccessState::Allowed,
            evidence: None,
            observed_at: Some(observed_at),
            reset_at: None,
        }
    }

    #[must_use]
    pub const fn exhausted(
        evidence: QuotaEvidence,
        observed_at: SystemTime,
        reset_at: Option<SystemTime>,
    ) -> Self {
        Self {
            access: QuotaAccessState::Exhausted,
            evidence: Some(evidence),
            observed_at: Some(observed_at),
            reset_at,
        }
    }

    /// 从持久化列恢复额度状态；非法组合返回 `None`。
    #[must_use]
    pub const fn from_persisted(
        access: QuotaAccessState,
        evidence: Option<QuotaEvidence>,
        observed_at: Option<SystemTime>,
        reset_at: Option<SystemTime>,
    ) -> Option<Self> {
        match access {
            QuotaAccessState::Unknown if evidence.is_none() && reset_at.is_none() => Some(Self {
                access,
                evidence,
                observed_at,
                reset_at,
            }),
            QuotaAccessState::Allowed
                if evidence.is_none() && observed_at.is_some() && reset_at.is_none() =>
            {
                Some(Self {
                    access,
                    evidence,
                    observed_at,
                    reset_at,
                })
            }
            QuotaAccessState::Exhausted if evidence.is_some() && observed_at.is_some() => {
                Some(Self {
                    access,
                    evidence,
                    observed_at,
                    reset_at,
                })
            }
            _ => None,
        }
    }

    #[must_use]
    pub const fn access(self) -> QuotaAccessState {
        self.access
    }

    #[must_use]
    pub const fn evidence(self) -> Option<QuotaEvidence> {
        self.evidence
    }

    #[must_use]
    pub const fn observed_at(self) -> Option<SystemTime> {
        self.observed_at
    }

    #[must_use]
    pub const fn reset_at(self) -> Option<SystemTime> {
        self.reset_at
    }

    /// 返回当前已确认的额度耗尽结论。
    ///
    /// `reset_at` 只表示何时应重新向 Provider 求证；时间到期本身不是额度恢复证据。
    #[must_use]
    pub fn is_exhausted(self) -> bool {
        self.access == QuotaAccessState::Exhausted
    }

    /// 判断已耗尽额度是否到达 Provider 的下一次复核时间。
    ///
    /// 有明确 `reset_at` 时严格等待该时刻；没有时由 Provider 传入自己的保守复核周期。
    #[must_use]
    pub fn exhaustion_refresh_due(self, now: SystemTime, fallback_interval: Duration) -> bool {
        if !self.is_exhausted() {
            return false;
        }
        let due_at = self.reset_at.or_else(|| {
            self.observed_at
                .and_then(|observed_at| observed_at.checked_add(fallback_interval))
        });
        due_at.is_none_or(|due_at| due_at <= now)
    }

    /// 合并一次额度访问观察；`Unknown` 不能擦除已经确认的访问结论。
    #[must_use]
    pub fn merge_observation(self, observation: Self) -> Self {
        if observation.access == QuotaAccessState::Unknown
            && self.access != QuotaAccessState::Unknown
        {
            self
        } else {
            observation
        }
    }
}

/// 对外唯一的五态账号状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountStatus {
    Normal,
    QuotaExhausted,
    RateLimited,
    Disabled,
    Error,
}

impl AccountStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::QuotaExhausted => "quota_exhausted",
            Self::RateLimited => "rate_limited",
            Self::Disabled => "disabled",
            Self::Error => "error",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "normal" => Some(Self::Normal),
            "quota_exhausted" => Some(Self::QuotaExhausted),
            "rate_limited" => Some(Self::RateLimited),
            "disabled" => Some(Self::Disabled),
            "error" => Some(Self::Error),
            _ => None,
        }
    }

    /// 管理页稳定业务排序：正常、限流、耗尽、错误、停用。
    #[must_use]
    pub const fn sort_rank(self) -> u8 {
        match self {
            Self::Normal => 0,
            Self::RateLimited => 1,
            Self::QuotaExhausted => 2,
            Self::Error => 3,
            Self::Disabled => 4,
        }
    }
}

/// `error` 状态下的稳定原因码。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountErrorReason {
    AccountUnverified,
    AccessTokenExpired,
    CredentialExpired,
    CredentialInvalid,
    AccountBanned,
}

impl AccountErrorReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AccountUnverified => "account_unverified",
            Self::AccessTokenExpired => "access_token_expired",
            Self::CredentialExpired => "credential_expired",
            Self::CredentialInvalid => "credential_invalid",
            Self::AccountBanned => "account_banned",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "account_unverified" => Some(Self::AccountUnverified),
            "access_token_expired" => Some(Self::AccessTokenExpired),
            "credential_expired" => Some(Self::CredentialExpired),
            "credential_invalid" => Some(Self::CredentialInvalid),
            "account_banned" => Some(Self::AccountBanned),
            _ => None,
        }
    }
}

/// 唯一状态解析器的完整输入事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountStatusFacts {
    pub enabled: bool,
    pub credential_state: CredentialState,
    pub access_token_expires_at: Option<SystemTime>,
    pub quota: QuotaState,
    pub rate_limited_until: Option<SystemTime>,
    pub last_error_reason: Option<AccountErrorReason>,
    pub last_error_message: Option<String>,
}

/// 唯一状态解析器的输出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountStatusProjection {
    pub status: AccountStatus,
    pub error_reason: Option<AccountErrorReason>,
    pub error_message: Option<String>,
    /// 仅 `rate_limited` 状态携带仍有效的运行时冷却截止时间。
    pub rate_limited_until: Option<SystemTime>,
}

/// 从独立事实派生唯一、互斥的对外状态。
#[must_use]
pub fn resolve_account_status(
    facts: &AccountStatusFacts,
    now: SystemTime,
) -> AccountStatusProjection {
    if !facts.enabled {
        return status_projection(AccountStatus::Disabled);
    }
    let credential_error = facts.credential_state.error_reason().or_else(|| {
        facts
            .access_token_expires_at
            .is_some_and(|expires_at| expires_at <= now)
            .then_some(AccountErrorReason::AccessTokenExpired)
    });
    if let Some(default_reason) = credential_error {
        return AccountStatusProjection {
            status: AccountStatus::Error,
            error_reason: Some(facts.last_error_reason.unwrap_or(default_reason)),
            error_message: facts.last_error_message.clone(),
            rate_limited_until: None,
        };
    }
    if facts.quota.is_exhausted() {
        return status_projection(AccountStatus::QuotaExhausted);
    }
    if facts
        .rate_limited_until
        .is_some_and(|rate_limited_until| rate_limited_until > now)
    {
        return AccountStatusProjection {
            status: AccountStatus::RateLimited,
            error_reason: None,
            error_message: None,
            rate_limited_until: facts.rate_limited_until,
        };
    }
    status_projection(AccountStatus::Normal)
}

const fn status_projection(status: AccountStatus) -> AccountStatusProjection {
    AccountStatusProjection {
        status,
        error_reason: None,
        error_message: None,
        rate_limited_until: None,
    }
}

/// 不含 secret 的账号持久事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAccount {
    id: ProviderAccountId,
    provider: ProviderKind,
    name: String,
    email: Option<String>,
    upstream_user_id: Option<String>,
    upstream_account_id: Option<String>,
    plan_type: Option<String>,
    authentication_kind: String,
    revision: CredentialRevision,
    enabled: bool,
    concurrency_limit: Option<AccountConcurrencyLimit>,
    weight: AccountWeight,
    credential_state: CredentialState,
    quota: QuotaState,
    last_error_reason: Option<AccountErrorReason>,
    last_error_message: Option<String>,
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
        upstream_user_id: Option<String>,
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
            concurrency_limit: None,
            weight: AccountWeight::DEFAULT,
            credential_state: CredentialState::Unknown,
            quota: QuotaState::unknown(),
            last_error_reason: None,
            last_error_message: None,
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
    pub fn with_account_facts(
        mut self,
        enabled: bool,
        credential_state: CredentialState,
        quota: QuotaState,
        last_error_reason: Option<AccountErrorReason>,
        last_error_message: Option<String>,
    ) -> Self {
        self.enabled = enabled;
        self.credential_state = if self.upstream_user_id.is_some() {
            credential_state
        } else {
            CredentialState::Unknown
        };
        self.quota = quota;
        self.last_error_reason = last_error_reason;
        self.last_error_message = last_error_message;
        self
    }

    #[must_use]
    pub const fn with_scheduling(
        mut self,
        concurrency_limit: Option<AccountConcurrencyLimit>,
        weight: AccountWeight,
    ) -> Self {
        self.concurrency_limit = concurrency_limit;
        self.weight = weight;
        self
    }

    /// 设置 RT 存在性与失败后的最早重试时刻。
    ///
    /// 正常 OAuth 预刷新由 worker 使用 AT 原始过期时间与当前运行时策略动态判断，
    /// 不应把提前量物化到 `next_refresh_at`。
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
    pub fn upstream_user_id(&self) -> Option<&str> {
        self.upstream_user_id.as_deref()
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
    pub const fn credential_state(&self) -> CredentialState {
        self.credential_state
    }

    #[must_use]
    pub const fn quota(&self) -> QuotaState {
        self.quota
    }

    #[must_use]
    pub const fn last_error_reason(&self) -> Option<AccountErrorReason> {
        self.last_error_reason
    }

    #[must_use]
    pub fn last_error_message(&self) -> Option<&str> {
        self.last_error_message.as_deref()
    }

    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    #[must_use]
    pub const fn concurrency_limit(&self) -> Option<AccountConcurrencyLimit> {
        self.concurrency_limit
    }

    #[must_use]
    pub const fn weight(&self) -> AccountWeight {
        self.weight
    }

    #[must_use]
    pub const fn effective_concurrency(&self, default: NonZeroU32) -> NonZeroU32 {
        match self.concurrency_limit {
            Some(limit) => limit.into_non_zero(),
            None => default,
        }
    }

    #[must_use]
    pub const fn access_token_expires_at(&self) -> Option<SystemTime> {
        self.access_token_expires_at
    }

    /// 返回瞬态 OAuth 刷新失败后的最早重试时刻；正常账号为 `None`。
    #[must_use]
    pub const fn next_refresh_at(&self) -> Option<SystemTime> {
        self.next_refresh_at
    }

    #[must_use]
    pub const fn has_refresh_token(&self) -> bool {
        self.has_refresh_token
    }

    /// 组合持久事实与请求级冷却，交给唯一解析器派生状态。
    #[must_use]
    pub fn status_projection(
        &self,
        now: SystemTime,
        rate_limited_until: Option<SystemTime>,
    ) -> AccountStatusProjection {
        resolve_account_status(
            &AccountStatusFacts {
                enabled: self.enabled,
                credential_state: self.credential_state,
                access_token_expires_at: self.access_token_expires_at,
                quota: self.quota,
                rate_limited_until,
                last_error_reason: self.last_error_reason,
                last_error_message: self.last_error_message.clone(),
            },
            now,
        )
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

/// 与 credential revision CAS 同事务提交的账号错误事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialStateWrite {
    pub credential_state: CredentialState,
    pub observed_at: SystemTime,
    pub error_reason: Option<AccountErrorReason>,
    pub message: Option<String>,
}

/// [`CredentialCasUpdate`] 跨 store 边界的命名字段。
pub struct CredentialCasUpdateParts {
    pub account_id: ProviderAccountId,
    pub expected_revision: CredentialRevision,
    pub profile: ProviderAccountUpdate,
    pub credential: PlaintextCredential,
    pub has_refresh_token: bool,
    pub access_token_expires_at: Option<SystemTime>,
    pub next_refresh_at: Option<SystemTime>,
    pub account_state: Option<Box<CredentialStateWrite>>,
}

/// 刷新后的完整 CAS 写回。
#[derive(Clone, PartialEq)]
pub struct CredentialCasUpdate {
    account_id: ProviderAccountId,
    expected_revision: CredentialRevision,
    profile: ProviderAccountUpdate,
    credential: PlaintextCredential,
    has_refresh_token: bool,
    access_token_expires_at: Option<SystemTime>,
    next_refresh_at: Option<SystemTime>,
    account_state: Option<Box<CredentialStateWrite>>,
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
            .field("account_state", &self.account_state)
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
            account_state: None,
        })
    }

    /// 将刷新调度与账号错误事实放入同一个 revision CAS。
    #[must_use]
    pub fn with_account_state(
        mut self,
        credential_state: CredentialState,
        observed_at: SystemTime,
        error_reason: Option<AccountErrorReason>,
        message: Option<String>,
    ) -> Self {
        let message = message.filter(|value| !value.trim().is_empty());
        let error_reason = if credential_state == CredentialState::Ready {
            message.as_ref().and(error_reason)
        } else {
            error_reason.or_else(|| credential_state.error_reason())
        };
        self.account_state = Some(Box::new(CredentialStateWrite {
            credential_state,
            observed_at,
            error_reason,
            message,
        }));
        self
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
    pub fn into_parts(self) -> CredentialCasUpdateParts {
        CredentialCasUpdateParts {
            account_id: self.account_id,
            expected_revision: self.expected_revision,
            profile: self.profile,
            credential: self.credential,
            has_refresh_token: self.has_refresh_token,
            access_token_expires_at: self.access_token_expires_at,
            next_refresh_at: self.next_refresh_at,
            account_state: self.account_state,
        }
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
    pub quota: OpaqueProviderData,
    /// Provider 原始 quota document 的观察时间；不代表访问结论发生变化。
    pub observed_at: SystemTime,
    /// Provider 已从私有 JSON 归一化出的额度访问事实。
    pub state: QuotaState,
}

impl fmt::Debug for QuotaObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QuotaObservation")
            .field("account_id", &self.account_id)
            .field("expected_revision", &self.expected_revision)
            .field("quota", &self.quota)
            .field("observed_at", &self.observed_at)
            .field("state", &self.state)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaWriteOutcome {
    Updated,
    Conflict,
}

/// 只推进 Provider quota 文档的最后成功查询时间。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaObservationTouch {
    pub account_id: ProviderAccountId,
    pub expected_revision: CredentialRevision,
    pub observed_at: SystemTime,
}

/// 不改 Provider 原始 quota JSON 的额度访问事实写入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaAccessChange {
    pub account_id: ProviderAccountId,
    pub expected_revision: CredentialRevision,
    pub state: QuotaState,
}

/// 账号状态的 revision-fenced 写入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountStateChange {
    pub account_id: ProviderAccountId,
    pub expected_revision: CredentialRevision,
    pub credential_state: CredentialState,
    pub observed_at: SystemTime,
    /// 受控错误原因；无失败事实的 Ready 写入必须清空。
    ///
    /// Ready 凭据可以在 AT 过期后继续 RT 恢复，此时允许保留最近一次
    /// 刷新失败原因，供错误状态投影展示。
    pub error_reason: Option<AccountErrorReason>,
    /// 供管理端展示的错误消息；结构化上游失败应保留原始 message，不能写入整个正文。
    /// 刷新成功或其他无失败事实的 Ready 写入必须清空。
    pub message: Option<String>,
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

    /// 读取账号当前 credential 及其 revision，不做任何版本比对。
    ///
    /// 管理写入必须在临近 CAS 时用它取 fence，而不是携带调用方持有的旧 revision：
    /// 后台刷新随时会推进 revision，用陈旧快照会把正常的恢复操作误判为冲突。
    async fn load_current_credential(
        &self,
        account: &ProviderAccountId,
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

    async fn touch_quota_observation(
        &self,
        touch: QuotaObservationTouch,
    ) -> Result<QuotaWriteOutcome, StoreError>;

    async fn apply_quota_access(
        &self,
        change: QuotaAccessChange,
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

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "smart" => Some(Self::Smart),
            "quota_reset_priority" => Some(Self::QuotaResetPriority),
            "round_robin" => Some(Self::RoundRobin),
            "sticky" => Some(Self::Sticky),
            _ => None,
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
    pub rate_limited_until: Option<SystemTime>,
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
    pub const fn with_rate_limit(mut self, rate_limited_until: Option<SystemTime>) -> Self {
        self.rate_limited_until = rate_limited_until;
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
    pub eligibility: AccountEligibilityPolicy,
    pub account_scope: Option<std::sync::Arc<crate::routing::FrozenAccountScope>>,
}

/// 选择账号时是否执行本地调度资格投影。
///
/// 管理端对指定账号执行诊断时，会直接向上游确认实际状态；该模式跳过
/// `enabled`、可用性、冷却和 token 到期等本地投影，仍保留租约、并发和
/// 请求间隔约束，且只能与固定账号约束组合使用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AccountEligibilityPolicy {
    #[default]
    Enforce,
    BypassForDiagnostic,
}

impl AccountEligibilityPolicy {
    #[must_use]
    pub const fn bypasses_local_eligibility(self) -> bool {
        matches!(self, Self::BypassForDiagnostic)
    }
}

/// 候选账号未进入调度池的约束。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountSchedulingBlocker {
    OutsideClientScope,
    LocalAvailability,
    Excluded,
    ConcurrencyLimit,
    RequestInterval,
    LowerWeight,
}

/// 优先账号在本次选择中的处理结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreferredAccountSelection {
    NotRequested,
    Hit,
    Missing,
    Blocked(AccountSchedulingBlocker),
}

/// 一次完整账号选择；同时保留优先账号决策，供 Provider 记录调度遥测。
#[derive(Debug, Clone, Copy)]
pub struct AccountSelection<'a> {
    candidate: &'a AccountCandidate,
    preferred: PreferredAccountSelection,
}

impl<'a> AccountSelection<'a> {
    #[must_use]
    pub const fn candidate(self) -> &'a AccountCandidate {
        self.candidate
    }

    #[must_use]
    pub const fn preferred(self) -> PreferredAccountSelection {
        self.preferred
    }
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
    ) -> Option<AccountSelection<'a>> {
        let mut eligible = candidates
            .iter()
            .filter(|candidate| self.scheduling_blocker(candidate, context).is_none())
            .collect::<Vec<_>>();
        if eligible.is_empty() {
            return None;
        }
        let highest_weight = eligible
            .iter()
            .map(|candidate| candidate.account.weight())
            .max()?;
        let preferred = if let Some(preferred) = context.preferred_account.as_ref() {
            match candidates
                .iter()
                .find(|candidate| candidate.account.id() == preferred)
            {
                Some(candidate) => match self.scheduling_blocker(candidate, context) {
                    Some(blocker) => PreferredAccountSelection::Blocked(blocker),
                    None if candidate.account.weight() < highest_weight => {
                        PreferredAccountSelection::Blocked(AccountSchedulingBlocker::LowerWeight)
                    }
                    None => {
                        return Some(AccountSelection {
                            candidate,
                            preferred: PreferredAccountSelection::Hit,
                        });
                    }
                },
                None => PreferredAccountSelection::Missing,
            }
        } else {
            PreferredAccountSelection::NotRequested
        };
        eligible.retain(|candidate| candidate.account.weight() == highest_weight);

        let candidate = match context.policy.strategy() {
            RotationStrategy::QuotaResetPriority => {
                let default_concurrency = context.policy.max_concurrent_per_account();
                eligible.sort_by(|left, right| {
                    (
                        left.signals.quota_reset_at.is_none(),
                        left.signals.quota_reset_at,
                    )
                        .cmp(&(
                            right.signals.quota_reset_at.is_none(),
                            right.signals.quota_reset_at,
                        ))
                        .then_with(|| {
                            capacity_utilization(left, default_concurrency)
                                .total_cmp(&capacity_utilization(right, default_concurrency))
                        })
                        .then_with(|| {
                            left.signals
                                .last_started_at
                                .cmp(&right.signals.last_started_at)
                        })
                        .then_with(|| left.account.id().cmp(right.account.id()))
                });
                eligible.first().copied()?
            }
            RotationStrategy::RoundRobin => {
                eligible.sort_by_key(|candidate| candidate.account.id().clone());
                let index = context.round_robin_cursor as usize % eligible.len();
                eligible.get(index).copied()?
            }
            RotationStrategy::Smart => select_smart_candidate(
                &eligible,
                context.policy.max_concurrent_per_account(),
                context.round_robin_cursor,
            )?,
            RotationStrategy::Sticky => {
                eligible.sort_by_key(|candidate| {
                    (
                        Reverse(candidate.signals.last_started_at),
                        candidate.account.id().clone(),
                    )
                });
                eligible.first().copied()?
            }
        };
        Some(AccountSelection {
            candidate,
            preferred,
        })
    }

    fn scheduling_blocker(
        &self,
        candidate: &AccountCandidate,
        context: &AccountSelectionContext,
    ) -> Option<AccountSchedulingBlocker> {
        if context
            .account_scope
            .as_ref()
            .is_some_and(|scope| !scope.allows(candidate.account.id()))
        {
            return Some(AccountSchedulingBlocker::OutsideClientScope);
        }
        if !context.eligibility.bypasses_local_eligibility() {
            let status = candidate
                .account
                .status_projection(context.now, candidate.signals.rate_limited_until)
                .status;
            if status != AccountStatus::Normal {
                return Some(AccountSchedulingBlocker::LocalAvailability);
            }
        }
        if context.excluded_accounts.contains(candidate.account.id()) {
            return Some(AccountSchedulingBlocker::Excluded);
        }
        if candidate.signals.in_flight
            >= candidate
                .account
                .effective_concurrency(context.policy.max_concurrent_per_account())
                .get()
        {
            return Some(AccountSchedulingBlocker::ConcurrencyLimit);
        }
        if candidate
            .signals
            .last_started_at
            .is_some_and(|last_started| {
                !context
                    .now
                    .duration_since(last_started)
                    .is_ok_and(|elapsed| elapsed >= context.policy.request_interval())
            })
        {
            return Some(AccountSchedulingBlocker::RequestInterval);
        }
        None
    }
}

const SMART_LOAD_WEIGHT: f64 = 1.0;
const SMART_QUOTA_WEIGHT: f64 = 0.8;
const SMART_FAILURE_WEIGHT: f64 = 1.0;
const SMART_LATENCY_WEIGHT: f64 = 0.5;

fn capacity_utilization(candidate: &AccountCandidate, default_concurrency: NonZeroU32) -> f64 {
    f64::from(candidate.signals.in_flight)
        / f64::from(
            candidate
                .account
                .effective_concurrency(default_concurrency)
                .get(),
        )
}

struct SmartNormalization {
    max_capacity_utilization: f64,
    min_quota: Option<u64>,
    max_quota: Option<u64>,
    min_latency_ms: Option<u64>,
    max_latency_ms: Option<u64>,
}

impl SmartNormalization {
    fn from_candidates(candidates: &[&AccountCandidate], default_concurrency: NonZeroU32) -> Self {
        let max_capacity_utilization = candidates
            .iter()
            .map(|candidate| capacity_utilization(candidate, default_concurrency))
            .reduce(f64::max)
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
            max_capacity_utilization,
            min_quota,
            max_quota,
            min_latency_ms,
            max_latency_ms,
        }
    }
}

fn select_smart_candidate<'a>(
    candidates: &[&'a AccountCandidate],
    default_concurrency: NonZeroU32,
    cursor: u64,
) -> Option<&'a AccountCandidate> {
    let normalization = SmartNormalization::from_candidates(candidates, default_concurrency);
    let mut ranked = candidates
        .iter()
        .map(|candidate| {
            (
                *candidate,
                smart_score(candidate, default_concurrency, &normalization),
            )
        })
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

fn smart_score(
    candidate: &AccountCandidate,
    default_concurrency: NonZeroU32,
    normalization: &SmartNormalization,
) -> f64 {
    let load = lower_ratio_is_better(
        capacity_utilization(candidate, default_concurrency),
        normalization.max_capacity_utilization,
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

fn lower_ratio_is_better(value: f64, maximum: f64) -> f64 {
    if maximum <= 0.0 {
        return 1.0;
    }
    1.0 - (value / maximum).clamp(0.0, 1.0)
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
