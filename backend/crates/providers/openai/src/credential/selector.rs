//! AttemptContext 驱动的 Codex 账号选择与 Redis lease port。

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use gateway_core::engine::credential::{
    AccountAvailability, AccountAvailabilityPolicy, AccountCandidate, AccountFeedbackStats,
    AccountRuntimeSignals, AccountSchedulingBlocker, AccountSelectionContext, AccountSelector,
    PreferredAccountSelection, ProviderAccount, ProviderAccountId,
};
use gateway_core::engine::{AttemptContext, ContinuationAttempt};
use gateway_core::provider_ports::{
    ProviderLeaseAcquisition, ProviderLeaseGuard, ProviderLeasePort, ProviderLeaseRequest,
    ProviderSchedulingLeaseRequest, ProviderSessionAffinityKey, ProviderSessionAffinityPort,
    ProviderSessionExclusionPort, ProviderSessionExclusions, ProviderStoreError,
};
use gateway_core::routing::ProviderKind;
use secrecy::ExposeSecret;
use thiserror::Error;
use url::Url;

use super::agent_identity::CodexAgentIdentityTaskService;
use super::catalog::CodexCredentialCatalogService;
use super::cookie::CodexCookiePolicy;
use super::quota::CodexCredentialQuotaService;
use super::repository::{CodexCredentialRepository, CredentialRepositoryError};
use super::security::CodexRuntimeAuthentication;
use super::types::{
    CODEX_AUTHENTICATION_KIND_AGENT_IDENTITY, CodexCookie, CodexCookieCaptureOutcome,
    RuntimeCodexCookie,
};

const DEFAULT_RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(60);
const CLOUDFLARE_PATH_BLOCK_COOLDOWN: Duration = Duration::from_secs(30);
const CLOUDFLARE_RECOVERY_STALE_AFTER: Duration = Duration::from_secs(60 * 60);
const CLOUDFLARE_CHALLENGE_BACKOFF: [Duration; 4] = [
    Duration::from_secs(10),
    Duration::from_secs(30),
    Duration::from_secs(90),
    Duration::from_secs(120),
];
const CLOUDFLARE_PATH_BLOCK_THRESHOLD: u32 = 3;
const SESSION_AFFINITY_TTL: Duration = Duration::from_secs(4 * 60 * 60);
const SESSION_AFFINITY_TIMEOUT: Duration = Duration::from_millis(100);
const CYBER_POLICY_SESSION_TTL: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexAccountFailure {
    /// Access token 已被上游明确判定为过期或失效。
    CredentialExpired,
    /// 账号需要完成身份验证后才能继续使用。
    IdentityVerificationRequired,
    /// 账号、workspace 或 organization 已被封禁或停用。
    Banned,
    /// 账号信用额度已耗尽。
    QuotaExhausted,
    /// 当前用量窗口已耗尽；到重置时间后可自动恢复。
    UsageLimitExhausted {
        /// 上游返回的窗口重置时长。
        retry_after: Option<Duration>,
    },
    /// 账号触发临时用量限制。
    RateLimited {
        /// 上游明确返回的最短冷却时长。
        retry_after: Option<Duration>,
    },
    /// Cloudflare challenge 要求账号进入递增冷却。
    CloudflareChallenge {
        /// 上游明确返回的最短冷却时长。
        retry_after: Option<Duration>,
    },
    /// Cloudflare 对当前上游路径返回空 404。
    CloudflarePathBlocked,
}

#[derive(Debug, Clone, Copy)]
struct RiskRecoveryState {
    challenge_count: u32,
    path_block_count: u32,
    observed_at: SystemTime,
}

#[derive(Debug, Clone, Copy)]
enum CookieRecovery {
    None,
    ExpireAt(SystemTime),
    Clear,
}

pub struct SelectCodexCredential<'a> {
    pub upstream_model: &'a str,
    pub request_url: &'a Url,
    pub attempt: &'a AttemptContext,
    pub session_affinity_key: Option<&'a ProviderSessionAffinityKey>,
}

#[derive(Clone)]
pub(crate) struct CodexCyberPolicyScope {
    key: ProviderSessionAffinityKey,
    state: Option<ProviderSessionExclusions>,
}

pub struct CodexCredentialSelector {
    provider_kind: ProviderKind,
    repository: CodexCredentialRepository,
    leases: Arc<dyn ProviderLeasePort>,
    session_affinity: Arc<dyn ProviderSessionAffinityPort>,
    session_exclusions: Arc<dyn ProviderSessionExclusionPort>,
    catalog: Arc<CodexCredentialCatalogService>,
    quota: Arc<CodexCredentialQuotaService>,
    agent_identity: Arc<CodexAgentIdentityTaskService>,
    cookie_policy: CodexCookiePolicy,
    risk_recovery: Mutex<HashMap<String, RiskRecoveryState>>,
    account_feedback: Arc<AccountFeedbackStats>,
    skip_exhausted: bool,
}

enum SessionAffinityLookup {
    Missing,
    Bound(ProviderAccountId),
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AffinityEscapeReason {
    HardUnavailable,
    QuotaExhausted,
    Cooldown,
    LeaseSaturated,
    PinnedAccount,
    SelectionInvariant,
}

impl AffinityEscapeReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::HardUnavailable => "hard_unavailable",
            Self::QuotaExhausted => "quota_exhausted",
            Self::Cooldown => "cooldown",
            Self::LeaseSaturated => "lease_saturated",
            Self::PinnedAccount => "pinned_account",
            Self::SelectionInvariant => "selection_invariant",
        }
    }
}

#[derive(Debug, Default)]
struct AffinitySelection {
    bound_account: Option<ProviderAccountId>,
    preferred_account: Option<ProviderAccountId>,
    escape_reason: Option<AffinityEscapeReason>,
}

impl AffinitySelection {
    fn preferred(account_id: ProviderAccountId) -> Self {
        Self {
            bound_account: Some(account_id.clone()),
            preferred_account: Some(account_id),
            escape_reason: None,
        }
    }

    fn escaped(account_id: ProviderAccountId, reason: AffinityEscapeReason) -> Self {
        Self {
            bound_account: Some(account_id),
            preferred_account: None,
            escape_reason: Some(reason),
        }
    }

    fn bound_account(&self) -> Option<&ProviderAccountId> {
        self.bound_account.as_ref()
    }

    fn preferred_account(&self) -> Option<&ProviderAccountId> {
        self.preferred_account.as_ref()
    }

    fn escape(&mut self, reason: AffinityEscapeReason) {
        if self.bound_account.is_some() && self.escape_reason.is_none() {
            self.escape_reason = Some(reason);
            self.preferred_account = None;
        }
    }

    fn observe_preferred_selection(&mut self, selection: PreferredAccountSelection) {
        if self.bound_account.is_none() || self.escape_reason.is_some() {
            return;
        }
        match selection {
            PreferredAccountSelection::Hit => {}
            PreferredAccountSelection::Blocked(AccountSchedulingBlocker::ConcurrencyLimit) => {
                self.escape(AffinityEscapeReason::LeaseSaturated);
            }
            PreferredAccountSelection::Blocked(AccountSchedulingBlocker::RequestInterval) => {
                self.escape(AffinityEscapeReason::Cooldown);
            }
            PreferredAccountSelection::Blocked(
                AccountSchedulingBlocker::LocalAvailability | AccountSchedulingBlocker::Excluded,
            )
            | PreferredAccountSelection::Missing => {
                self.escape(AffinityEscapeReason::HardUnavailable);
            }
            PreferredAccountSelection::NotRequested => {
                self.escape(AffinityEscapeReason::SelectionInvariant);
            }
        }
    }

    fn observe_lease_busy(&mut self, account_id: &ProviderAccountId) {
        if self.bound_account.as_ref() == Some(account_id) {
            self.escape(AffinityEscapeReason::LeaseSaturated);
        }
    }

    fn telemetry(&self, selected_account: &ProviderAccountId) -> AffinityTelemetry {
        AffinityTelemetry {
            affinity_hit: self.bound_account.as_ref() == Some(selected_account)
                && self.escape_reason.is_none(),
            escape_reason: self.escape_reason,
            account_switch: self
                .bound_account
                .as_ref()
                .is_some_and(|bound| bound != selected_account),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct AffinityTelemetry {
    affinity_hit: bool,
    escape_reason: Option<AffinityEscapeReason>,
    account_switch: bool,
}

impl CodexCredentialSelector {
    #[must_use]
    // 选择器显式持有各能力边界，避免把 Provider 私有服务重新包装成通用容器。
    #[expect(clippy::too_many_arguments)]
    pub fn new(
        provider_kind: ProviderKind,
        repository: CodexCredentialRepository,
        leases: Arc<dyn ProviderLeasePort>,
        session_affinity: Arc<dyn ProviderSessionAffinityPort>,
        session_exclusions: Arc<dyn ProviderSessionExclusionPort>,
        catalog: Arc<CodexCredentialCatalogService>,
        quota: Arc<CodexCredentialQuotaService>,
        agent_identity: Arc<CodexAgentIdentityTaskService>,
        account_feedback: Arc<AccountFeedbackStats>,
        cookie_policy: CodexCookiePolicy,
        skip_exhausted: bool,
    ) -> Self {
        Self {
            provider_kind,
            repository,
            leases,
            session_affinity,
            session_exclusions,
            catalog,
            quota,
            agent_identity,
            cookie_policy,
            risk_recovery: Mutex::new(HashMap::new()),
            account_feedback,
            skip_exhausted,
        }
    }

    pub async fn select(
        &self,
        request: &SelectCodexCredential<'_>,
    ) -> Result<CodexCredentialLease, CredentialSelectionError> {
        self.select_with_cyber_policy(request, None).await
    }

    pub(crate) async fn select_with_cyber_policy(
        &self,
        request: &SelectCodexCredential<'_>,
        cyber_policy_session_key: Option<&ProviderSessionAffinityKey>,
    ) -> Result<CodexCredentialLease, CredentialSelectionError> {
        let diagnostic = request.attempt.is_diagnostic_required_account();
        let accounts = self.repository.list_for_provider().await?;
        let accounts = accounts
            .into_iter()
            .filter(|account| {
                account.provider() == &self.provider_kind
                    && (diagnostic || {
                        let observed_support = self
                            .catalog
                            .observed_model_support(account, request.upstream_model);
                        matches!(observed_support, Ok(None | Some(true)))
                    })
            })
            .collect::<Vec<_>>();
        if !diagnostic {
            self.quota.prepare_scheduling(&accounts).await;
        }
        let account_ids = accounts
            .iter()
            .map(|account| account.id().clone())
            .collect::<Vec<_>>();
        let scheduling = self
            .leases
            .load_state(&self.provider_kind, &account_ids)
            .await?;
        let round_robin_cursor = scheduling.round_robin_cursor();
        let candidates = accounts
            .into_iter()
            .map(|account| {
                let health = self
                    .account_feedback
                    .scheduling_signals(&self.provider_kind, account.id());
                let signals = scheduling
                    .signals()
                    .get(account.id())
                    .cloned()
                    .unwrap_or(AccountRuntimeSignals {
                        in_flight: 0,
                        last_started_at: None,
                        quota_reset_at: None,
                        quota_remaining_rank: None,
                        failure_rate_basis_points: None,
                        first_output_latency_ms: None,
                    })
                    .with_provider_quota(self.quota.scheduling_signals(&account))
                    .with_runtime_health(health.0, health.1);
                AccountCandidate { account, signals }
            })
            .collect::<Vec<_>>();
        // 旧版开关只影响“候选资格”。保留原始账号供 revision-fenced 凭据加载，
        // 用影子候选承载放宽后的资格，避免伪造已持久化账号的运行状态。
        let quota_exhausted_candidates = (!self.skip_exhausted).then(|| {
            candidates
                .iter()
                .cloned()
                .map(|mut candidate| {
                    candidate.account = quota_exhausted_scheduling_projection(&candidate.account);
                    candidate
                })
                .collect::<Vec<_>>()
        });
        let scheduling_candidates = quota_exhausted_candidates.as_deref().unwrap_or(&candidates);
        let mut affinity = if diagnostic {
            AffinitySelection::default()
        } else {
            self.resolve_session_affinity(
                request.session_affinity_key,
                &candidates,
                SystemTime::now(),
            )
            .await
        };
        let cyber_policy_scope = self
            .prepare_cyber_policy_scope(cyber_policy_session_key)
            .await;
        let mut excluded = request.attempt.excluded_accounts().clone();
        if let Some(state) = cyber_policy_scope
            .as_ref()
            .and_then(|scope| scope.state.as_ref())
        {
            excluded.extend(state.excluded_accounts().iter().cloned());
        }
        let continuation_account = match request.attempt.continuation_attempt() {
            ContinuationAttempt::Native => request
                .attempt
                .continuation()
                .and_then(gateway_core::engine::continuation::ContinuationBinding::pinned)
                .map(|continuation| continuation.account().clone()),
            ContinuationAttempt::ReplayOwner => request
                .attempt
                .account_state_owner()
                .filter(|owner| owner.provider() == &self.provider_kind)
                .map(|owner| owner.account().clone()),
            ContinuationAttempt::None | ContinuationAttempt::ReplayAny => None,
        };
        let required_account = request.attempt.required_account().cloned();
        if required_account
            .as_ref()
            .zip(continuation_account.as_ref())
            .is_some_and(|(required, continuation)| required != continuation)
        {
            return Err(CredentialSelectionError::NoEligibleCredential);
        }
        let pinned_account = required_account.or(continuation_account);
        if let Some(required) = pinned_account.as_ref() {
            for candidate in &candidates {
                if candidate.account.id() != required {
                    excluded.insert(candidate.account.id().clone());
                }
            }
        }
        if pinned_account
            .as_ref()
            .zip(affinity.bound_account())
            .is_some_and(|(pinned, bound)| pinned != bound)
        {
            affinity.escape(AffinityEscapeReason::PinnedAccount);
        }
        let preferred = pinned_account
            .clone()
            .or_else(|| affinity.preferred_account().cloned());
        let mut shortest_retry = None;

        loop {
            let context = AccountSelectionContext {
                policy: request.attempt.account_selection_policy(),
                now: SystemTime::now(),
                excluded_accounts: excluded.clone(),
                preferred_account: preferred.clone(),
                round_robin_cursor,
                availability: if diagnostic {
                    AccountAvailabilityPolicy::BypassForDiagnostic
                } else {
                    AccountAvailabilityPolicy::Enforce
                },
            };
            let Some(selection) = AccountSelector.select(scheduling_candidates, &context) else {
                return match shortest_retry {
                    Some(retry_after) => Err(CredentialSelectionError::CapacityUnavailable {
                        retry_after: Some(retry_after),
                    }),
                    None => Err(CredentialSelectionError::NoEligibleCredential),
                };
            };
            affinity.observe_preferred_selection(selection.preferred());
            let selected = selection.candidate();
            let account = candidates
                .iter()
                .find(|candidate| candidate.account.id() == selected.account.id())
                .map(|candidate| candidate.account.clone())
                .ok_or(CredentialSelectionError::InvalidCredential)?;
            let allows_account_state_mutation = !diagnostic || account.enabled();
            let policy = request.attempt.account_selection_policy();
            match self
                .leases
                .try_acquire(ProviderLeaseRequest::Scheduling(
                    ProviderSchedulingLeaseRequest::new(
                        self.provider_kind.clone(),
                        account.id().clone(),
                        account.revision(),
                        policy.max_concurrent_per_account(),
                        policy.request_interval(),
                        request.attempt.deadline(),
                    ),
                ))
                .await?
            {
                ProviderLeaseAcquisition::Busy { retry_after } => {
                    affinity.observe_lease_busy(account.id());
                    shortest_retry = minimum_duration(shortest_retry, retry_after);
                    excluded.insert(account.id().clone());
                }
                ProviderLeaseAcquisition::Acquired(guard) => {
                    let affinity_telemetry = affinity.telemetry(account.id());
                    tracing::info!(
                        request_id = %request.attempt.request_id(),
                        attempt_index = request.attempt.attempt_index().get(),
                        rotation_strategy = policy.strategy().as_str(),
                        account_id = %account.id(),
                        affinity_hit = affinity_telemetry.affinity_hit,
                        escape_reason = affinity_telemetry
                            .escape_reason
                            .map_or("", AffinityEscapeReason::as_str),
                        account_switch = affinity_telemetry.account_switch,
                        "OpenAI account selected"
                    );
                    let (account, runtime) = if account.authentication_kind()
                        == CODEX_AUTHENTICATION_KIND_AGENT_IDENTITY
                    {
                        let prepared = if allows_account_state_mutation {
                            self.agent_identity.prepare(&account).await
                        } else {
                            self.agent_identity.load_current(account.id()).await
                        }
                        .map_err(|_| CredentialSelectionError::InvalidCredential)?;
                        (prepared.account, prepared.credential)
                    } else {
                        let runtime = self.repository.load_runtime_credential(&account).await?;
                        (account, runtime)
                    };
                    let cookies = runtime
                        .cookies
                        .into_iter()
                        .filter(|cookie| {
                            cookie
                                .expires_at
                                .is_none_or(|expires| expires > chrono::Utc::now())
                                && self.cookie_policy.may_replay(
                                    request.request_url,
                                    &cookie.domain,
                                    &cookie.path,
                                    cookie.host_only,
                                    cookie.secure,
                                )
                        })
                        .collect();
                    return Ok(CodexCredentialLease {
                        installation_id: runtime.installation_id,
                        account,
                        authentication: runtime.authentication,
                        cookies,
                        cyber_policy_scope,
                        allows_account_state_mutation,
                        affinity_telemetry,
                        _guard: guard,
                    });
                }
            }
        }
    }

    async fn resolve_session_affinity(
        &self,
        key: Option<&ProviderSessionAffinityKey>,
        candidates: &[AccountCandidate],
        now: SystemTime,
    ) -> AffinitySelection {
        let Some(key) = key else {
            return AffinitySelection::default();
        };
        let SessionAffinityLookup::Bound(account_id) = self.lookup_session_affinity(key).await
        else {
            return AffinitySelection::default();
        };
        let Some(candidate) = candidates
            .iter()
            .find(|candidate| candidate.account.id() == &account_id)
        else {
            self.clear_session_affinity(key).await;
            return AffinitySelection::escaped(account_id, AffinityEscapeReason::HardUnavailable);
        };
        if candidate.account.is_schedulable(now) {
            return AffinitySelection::preferred(account_id);
        }
        let reason = affinity_unavailable_reason(&candidate.account, now);
        self.clear_session_affinity(key).await;
        AffinitySelection::escaped(account_id, reason)
    }

    async fn lookup_session_affinity(
        &self,
        key: &ProviderSessionAffinityKey,
    ) -> SessionAffinityLookup {
        match tokio::time::timeout(
            SESSION_AFFINITY_TIMEOUT,
            self.session_affinity.load(&self.provider_kind, key),
        )
        .await
        {
            Ok(Ok(Some(account_id))) => SessionAffinityLookup::Bound(account_id),
            Ok(Ok(None)) => SessionAffinityLookup::Missing,
            Ok(Err(error)) => {
                tracing::warn!(error = %error, "OpenAI session affinity read failed open");
                SessionAffinityLookup::Unavailable
            }
            Err(_) => {
                tracing::warn!(
                    timeout_ms = SESSION_AFFINITY_TIMEOUT.as_millis(),
                    "OpenAI session affinity read timed out"
                );
                SessionAffinityLookup::Unavailable
            }
        }
    }

    async fn clear_session_affinity(&self, key: &ProviderSessionAffinityKey) {
        match tokio::time::timeout(
            SESSION_AFFINITY_TIMEOUT,
            self.session_affinity.clear(&self.provider_kind, key),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                tracing::warn!(error = %error, "OpenAI stale session affinity clear failed open");
            }
            Err(_) => {
                tracing::warn!(
                    timeout_ms = SESSION_AFFINITY_TIMEOUT.as_millis(),
                    "OpenAI stale session affinity clear timed out"
                );
            }
        }
    }

    async fn prepare_cyber_policy_scope(
        &self,
        key: Option<&ProviderSessionAffinityKey>,
    ) -> Option<CodexCyberPolicyScope> {
        let key = key?.clone();
        let state = match tokio::time::timeout(
            SESSION_AFFINITY_TIMEOUT,
            self.session_exclusions.load(&self.provider_kind, &key),
        )
        .await
        {
            Ok(Ok(state)) => state,
            Ok(Err(error)) => {
                tracing::warn!(error = %error, "OpenAI cyber policy state read failed open");
                None
            }
            Err(_) => {
                tracing::warn!(
                    timeout_ms = SESSION_AFFINITY_TIMEOUT.as_millis(),
                    "OpenAI cyber policy state read timed out"
                );
                None
            }
        };
        Some(CodexCyberPolicyScope { key, state })
    }

    pub(crate) async fn record_cyber_policy_failure(
        &self,
        scope: Option<&CodexCyberPolicyScope>,
        account: &ProviderAccount,
    ) {
        let Some(scope) = scope else {
            return;
        };
        match tokio::time::timeout(
            SESSION_AFFINITY_TIMEOUT,
            self.session_exclusions.record_failure(
                &self.provider_kind,
                &scope.key,
                account.id(),
                CYBER_POLICY_SESSION_TTL,
            ),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                tracing::warn!(
                    account_id = %account.id(),
                    error = %error,
                    "OpenAI cyber policy exclusion write failed open"
                );
            }
            Err(_) => {
                tracing::warn!(
                    account_id = %account.id(),
                    timeout_ms = SESSION_AFFINITY_TIMEOUT.as_millis(),
                    "OpenAI cyber policy exclusion write timed out"
                );
            }
        }
    }

    pub(crate) async fn observe_cyber_policy_success(&self, scope: Option<&CodexCyberPolicyScope>) {
        let Some(scope) = scope.filter(|scope| scope.state.is_some()) else {
            return;
        };
        let Some(state) = scope.state.as_ref() else {
            return;
        };
        match tokio::time::timeout(
            SESSION_AFFINITY_TIMEOUT,
            self.session_exclusions
                .clear(&self.provider_kind, &scope.key, state.revision()),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                tracing::warn!(error = %error, "OpenAI cyber policy exclusion clear failed open");
            }
            Err(_) => {
                tracing::warn!(
                    timeout_ms = SESSION_AFFINITY_TIMEOUT.as_millis(),
                    "OpenAI cyber policy exclusion clear timed out"
                );
            }
        }
    }

    pub async fn record_failure(
        &self,
        account: &ProviderAccount,
        failure: CodexAccountFailure,
    ) -> Result<(), CredentialSelectionError> {
        let now = SystemTime::now();
        let (availability, reason, cooldown_until, cookie_recovery) = match failure {
            CodexAccountFailure::CredentialExpired => (
                AccountAvailability::Expired,
                Some("credential_expired".to_owned()),
                None,
                CookieRecovery::None,
            ),
            CodexAccountFailure::IdentityVerificationRequired => (
                AccountAvailability::Invalid,
                Some("identity_verification_required".to_owned()),
                None,
                CookieRecovery::None,
            ),
            CodexAccountFailure::Banned => (
                AccountAvailability::Banned,
                Some("account_banned".to_owned()),
                None,
                CookieRecovery::None,
            ),
            CodexAccountFailure::QuotaExhausted => (
                AccountAvailability::QuotaExhausted,
                Some("quota_exhausted".to_owned()),
                None,
                CookieRecovery::None,
            ),
            CodexAccountFailure::UsageLimitExhausted { retry_after } => {
                return self
                    .record_resettable_limit_failure(
                        account,
                        retry_after,
                        "usage_limit_exhausted",
                        now,
                    )
                    .await;
            }
            CodexAccountFailure::RateLimited { retry_after } => {
                return self
                    .record_resettable_limit_failure(account, retry_after, "rate_limited", now)
                    .await;
            }
            CodexAccountFailure::CloudflareChallenge { retry_after } => {
                let delay = self.cloudflare_challenge_delay(account.id(), now, retry_after);
                let cooldown_until = now.checked_add(delay);
                (
                    AccountAvailability::Cooldown,
                    Some("cloudflare_challenge".to_owned()),
                    cooldown_until,
                    cooldown_until.map_or(CookieRecovery::Clear, CookieRecovery::ExpireAt),
                )
            }
            CodexAccountFailure::CloudflarePathBlocked => {
                let blocked = self.record_cloudflare_path_block(account.id(), now);
                if blocked >= CLOUDFLARE_PATH_BLOCK_THRESHOLD {
                    (
                        AccountAvailability::Invalid,
                        Some("cloudflare_path_blocked".to_owned()),
                        None,
                        CookieRecovery::Clear,
                    )
                } else {
                    (
                        AccountAvailability::Cooldown,
                        Some("cloudflare_path_blocked".to_owned()),
                        now.checked_add(CLOUDFLARE_PATH_BLOCK_COOLDOWN),
                        CookieRecovery::Clear,
                    )
                }
            }
        };
        self.repository
            .apply_state(account, availability, reason, cooldown_until, now)
            .await?;
        self.apply_cookie_recovery(account, cookie_recovery).await?;
        Ok(())
    }

    async fn record_resettable_limit_failure(
        &self,
        account: &ProviderAccount,
        retry_after: Option<Duration>,
        reason: &'static str,
        observed_at: SystemTime,
    ) -> Result<(), CredentialSelectionError> {
        let current = self.current_account(account.id()).await?;
        if current.revision() != account.revision() {
            return Err(CredentialRepositoryError::RevisionConflict.into());
        }
        let cooldown_until =
            observed_at.checked_add(retry_after.unwrap_or(DEFAULT_RATE_LIMIT_COOLDOWN));
        let cooldown_until = match (current.availability(), current.cooldown_until()) {
            (AccountAvailability::QuotaExhausted, Some(existing)) => {
                cooldown_until.map_or(Some(existing), |until| Some(existing.max(until)))
            }
            _ => cooldown_until,
        };
        let reason = if current.availability() == AccountAvailability::QuotaExhausted
            && current.cooldown_until().is_none()
        {
            Some("quota_exhausted".to_owned())
        } else {
            Some(reason.to_owned())
        };
        // 一次真实 429 证明上游已经接受当前凭据，可纠正旧的认证/封禁终态。
        // resettable 用量限制沿用 v2 的 quota lock 语义，并保留/延长复核时间。
        self.repository
            .apply_state(
                &current,
                AccountAvailability::QuotaExhausted,
                reason,
                cooldown_until,
                observed_at,
            )
            .await?;
        Ok(())
    }

    pub async fn record_success(
        &self,
        account: &ProviderAccount,
        session_affinity_key: Option<&ProviderSessionAffinityKey>,
    ) {
        self.restore_recoverable_account_state(account).await;
        self.risk_recovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(account.id().as_str());
        let Some(key) = session_affinity_key else {
            return;
        };
        if matches!(
            self.lookup_session_affinity(key).await,
            SessionAffinityLookup::Unavailable
        ) {
            return;
        }
        match tokio::time::timeout(
            SESSION_AFFINITY_TIMEOUT,
            self.session_affinity.bind(
                &self.provider_kind,
                key,
                account.id(),
                SESSION_AFFINITY_TTL,
            ),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(
                    account_id = %account.id(),
                    error = %error,
                    "OpenAI session affinity write failed open"
                );
            }
            Err(_) => {
                tracing::warn!(
                    account_id = %account.id(),
                    timeout_ms = SESSION_AFFINITY_TIMEOUT.as_millis(),
                    "OpenAI session affinity write timed out"
                );
            }
        }
    }

    async fn restore_recoverable_account_state(&self, account: &ProviderAccount) {
        let Ok(current) = self.current_account(account.id()).await else {
            return;
        };
        // QuotaExhausted 只能由结构化额度事实解除；一次成功响应本身不能证明额度已经恢复。
        if current.provider() != &self.provider_kind
            || current.revision() != account.revision()
            || !current.enabled()
            || !matches!(
                current.availability(),
                AccountAvailability::Unknown
                    | AccountAvailability::Cooldown
                    | AccountAvailability::Expired
                    | AccountAvailability::Invalid
                    | AccountAvailability::Banned
            )
        {
            return;
        }
        if let Err(error) = self
            .repository
            .apply_state(
                &current,
                AccountAvailability::Ready,
                None,
                None,
                SystemTime::now(),
            )
            .await
        {
            tracing::warn!(
                account_id = %account.id(),
                error = %error,
                "OpenAI account state recovery after successful upstream response failed"
            );
        }
    }

    pub async fn current_account(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<ProviderAccount, CredentialSelectionError> {
        self.repository
            .store()
            .get_account(account_id)
            .await
            .map_err(|_| CredentialSelectionError::Store)?
            .ok_or(CredentialSelectionError::InvalidCredential)
    }

    pub async fn capture_response_cookies(
        &self,
        account: &ProviderAccount,
        response_origin: &Url,
        headers: &[String],
    ) -> Result<CodexCookieCaptureOutcome, CredentialSelectionError> {
        let parsed = self.cookie_policy.parse_response_headers(
            account.id().as_str(),
            account.revision().get(),
            response_origin,
            headers,
            chrono::Utc::now(),
        );
        if parsed.inputs.is_empty() {
            return Ok(CodexCookieCaptureOutcome {
                credential_revision: None,
                rejected: parsed.rejected,
            });
        }
        let mut data = self.repository.load_complete_data(account).await?;
        let cookies = data.cookies_mut();
        for input in parsed.inputs {
            let scope = self.cookie_policy.validate_capture(
                &input.response_origin,
                input.domain_attribute.as_deref(),
                &input.name,
                &input.path,
            )?;
            cookies.retain(|cookie| {
                !(cookie.name == input.name
                    && cookie.domain == scope.domain
                    && cookie.path == input.path)
            });
            if !input.delete {
                cookies.push(CodexCookie {
                    name: input.name,
                    value: input.value.expose_secret().to_owned(),
                    domain: scope.domain,
                    path: input.path,
                    host_only: scope.host_only,
                    secure: input.secure,
                    expires_at: input.expires_at,
                });
            }
        }
        let revision = self.repository.compare_and_swap_data(account, data).await?;
        Ok(CodexCookieCaptureOutcome {
            credential_revision: Some(revision.get()),
            rejected: parsed.rejected,
        })
    }

    fn cloudflare_challenge_delay(
        &self,
        account_id: &ProviderAccountId,
        now: SystemTime,
        retry_after: Option<Duration>,
    ) -> Duration {
        let mut recovery = self
            .risk_recovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = active_risk_recovery(&mut recovery, account_id.as_str(), now);
        state.challenge_count = state.challenge_count.saturating_add(1);
        state.observed_at = now;
        let index = usize::try_from(state.challenge_count.saturating_sub(1))
            .unwrap_or(usize::MAX)
            .min(CLOUDFLARE_CHALLENGE_BACKOFF.len() - 1);
        retry_after
            .unwrap_or_default()
            .max(CLOUDFLARE_CHALLENGE_BACKOFF[index])
    }

    fn record_cloudflare_path_block(&self, account_id: &ProviderAccountId, now: SystemTime) -> u32 {
        let mut recovery = self
            .risk_recovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = active_risk_recovery(&mut recovery, account_id.as_str(), now);
        state.path_block_count = state.path_block_count.saturating_add(1);
        state.observed_at = now;
        state.path_block_count
    }

    async fn apply_cookie_recovery(
        &self,
        account: &ProviderAccount,
        recovery: CookieRecovery,
    ) -> Result<(), CredentialSelectionError> {
        if matches!(recovery, CookieRecovery::None) {
            return Ok(());
        }
        let mut data = self.repository.load_complete_data(account).await?;
        if data.cookies().is_empty() {
            return Ok(());
        }
        match recovery {
            CookieRecovery::None => return Ok(()),
            CookieRecovery::ExpireAt(expires_at) => {
                let expires_at = chrono::DateTime::<chrono::Utc>::from(expires_at);
                for cookie in data.cookies_mut() {
                    cookie.expires_at = Some(
                        cookie
                            .expires_at
                            .map_or(expires_at, |current| current.min(expires_at)),
                    );
                }
            }
            CookieRecovery::Clear => data.cookies_mut().clear(),
        }
        self.repository.compare_and_swap_data(account, data).await?;
        Ok(())
    }
}

fn quota_exhausted_scheduling_projection(account: &ProviderAccount) -> ProviderAccount {
    if account.availability() == AccountAvailability::QuotaExhausted {
        return account.clone().with_runtime_state(
            account.enabled(),
            AccountAvailability::Ready,
            account.cooldown_until(),
        );
    }
    account.clone()
}

fn affinity_unavailable_reason(account: &ProviderAccount, now: SystemTime) -> AffinityEscapeReason {
    if !account.enabled()
        || account
            .access_token_expires_at()
            .is_some_and(|expires_at| expires_at <= now)
    {
        return AffinityEscapeReason::HardUnavailable;
    }
    match account.availability() {
        AccountAvailability::QuotaExhausted => AffinityEscapeReason::QuotaExhausted,
        AccountAvailability::Cooldown => AffinityEscapeReason::Cooldown,
        AccountAvailability::Unknown
        | AccountAvailability::Ready
        | AccountAvailability::Expired
        | AccountAvailability::Banned
        | AccountAvailability::Invalid => AffinityEscapeReason::HardUnavailable,
    }
}

fn active_risk_recovery<'a>(
    recovery: &'a mut HashMap<String, RiskRecoveryState>,
    account_id: &str,
    now: SystemTime,
) -> &'a mut RiskRecoveryState {
    recovery.retain(|_, state| match now.duration_since(state.observed_at) {
        Ok(elapsed) => elapsed <= CLOUDFLARE_RECOVERY_STALE_AFTER,
        Err(_) => true,
    });
    recovery
        .entry(account_id.to_owned())
        .or_insert(RiskRecoveryState {
            challenge_count: 0,
            path_block_count: 0,
            observed_at: now,
        })
}

impl fmt::Debug for CodexCredentialSelector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexCredentialSelector")
            .field("repository", &"ProviderAccountStore")
            .field("leases", &"ProviderLeasePort")
            .field("catalog", &"CodexCredentialCatalogService")
            .field("quota", &"CodexCredentialQuotaService")
            .field("cookie_policy", &self.cookie_policy)
            .finish()
    }
}

pub struct CodexCredentialLease {
    account: ProviderAccount,
    authentication: CodexRuntimeAuthentication,
    cookies: Vec<RuntimeCodexCookie>,
    installation_id: String,
    cyber_policy_scope: Option<CodexCyberPolicyScope>,
    allows_account_state_mutation: bool,
    affinity_telemetry: AffinityTelemetry,
    _guard: Box<dyn ProviderLeaseGuard>,
}

impl CodexCredentialLease {
    #[must_use]
    pub const fn account(&self) -> &ProviderAccount {
        &self.account
    }

    #[must_use]
    pub const fn account_id(&self) -> &ProviderAccountId {
        self.account.id()
    }

    #[must_use]
    pub const fn authentication(&self) -> &CodexRuntimeAuthentication {
        &self.authentication
    }

    #[must_use]
    pub fn cookies(&self) -> &[RuntimeCodexCookie] {
        &self.cookies
    }

    #[must_use]
    pub fn installation_id(&self) -> &str {
        &self.installation_id
    }

    #[must_use]
    pub(crate) const fn cyber_policy_scope(&self) -> Option<&CodexCyberPolicyScope> {
        self.cyber_policy_scope.as_ref()
    }

    /// 禁用账号的管理端诊断必须只返回真实上游结果，不能回写账号侧状态。
    #[must_use]
    pub(crate) const fn allows_account_state_mutation(&self) -> bool {
        self.allows_account_state_mutation
    }

    #[must_use]
    pub const fn affinity_hit(&self) -> bool {
        self.affinity_telemetry.affinity_hit
    }

    #[must_use]
    pub const fn escape_reason(&self) -> Option<&'static str> {
        match self.affinity_telemetry.escape_reason {
            Some(reason) => Some(reason.as_str()),
            None => None,
        }
    }

    #[must_use]
    pub const fn account_switch(&self) -> bool {
        self.affinity_telemetry.account_switch
    }
}

impl fmt::Debug for CodexCredentialLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexCredentialLease")
            .field("account", &self.account)
            .field("authentication", &"<redacted>")
            .field("cookies", &self.cookies)
            .field("installation_id", &"<pseudonymous>")
            .finish()
    }
}

#[derive(Debug, Error)]
pub enum CredentialSelectionError {
    #[error("no eligible Codex account")]
    NoEligibleCredential,
    #[error("Codex account capacity is unavailable")]
    CapacityUnavailable { retry_after: Option<Duration> },
    #[error("Codex account data is invalid")]
    InvalidCredential,
    #[error("Codex account store is unavailable")]
    Store,
    #[error("Codex account lease runtime is unavailable")]
    Coordinator,
    #[error("Codex Cookie policy rejected the value")]
    CookiePolicy,
}

impl From<CredentialRepositoryError> for CredentialSelectionError {
    fn from(error: CredentialRepositoryError) -> Self {
        match error {
            CredentialRepositoryError::InvalidInput(_)
            | CredentialRepositoryError::InvalidCredentialData
            | CredentialRepositoryError::IdentityMismatch => Self::InvalidCredential,
            CredentialRepositoryError::RevisionConflict | CredentialRepositoryError::Store => {
                Self::Store
            }
        }
    }
}

impl From<ProviderStoreError> for CredentialSelectionError {
    fn from(_: ProviderStoreError) -> Self {
        Self::Coordinator
    }
}

impl From<super::cookie::CookiePolicyError> for CredentialSelectionError {
    fn from(_: super::cookie::CookiePolicyError) -> Self {
        Self::CookiePolicy
    }
}

fn minimum_duration(current: Option<Duration>, candidate: Option<Duration>) -> Option<Duration> {
    match (current, candidate) {
        (Some(current), Some(candidate)) => Some(current.min(candidate)),
        (Some(current), None) => Some(current),
        (None, candidate) => candidate,
    }
}
