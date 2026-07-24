//! AttemptContext 驱动的 Codex 账号选择与 Redis lease port。

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use gateway_core::engine::credential::{
    AccountAvailability, AccountCandidate, AccountFeedbackStats, AccountRuntimeSignals,
    AccountSelectionContext, AccountSelector, ProviderAccount, ProviderAccountId,
};
use gateway_core::engine::{AttemptContext, ContinuationAttempt};
use gateway_core::provider_ports::{
    ProviderLeaseAcquisition, ProviderLeaseGuard, ProviderLeasePort, ProviderLeaseRequest,
    ProviderSchedulingLeaseRequest, ProviderSessionAffinityKey, ProviderSessionAffinityPort,
    ProviderStoreError,
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
const SESSION_AFFINITY_TTL: Duration = Duration::from_secs(60 * 60);
const SESSION_AFFINITY_TIMEOUT: Duration = Duration::from_millis(100);

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

pub struct CodexCredentialSelector {
    provider_kind: ProviderKind,
    repository: CodexCredentialRepository,
    leases: Arc<dyn ProviderLeasePort>,
    session_affinity: Arc<dyn ProviderSessionAffinityPort>,
    catalog: Arc<CodexCredentialCatalogService>,
    quota: Arc<CodexCredentialQuotaService>,
    agent_identity: Arc<CodexAgentIdentityTaskService>,
    cookie_policy: CodexCookiePolicy,
    risk_recovery: Mutex<HashMap<String, RiskRecoveryState>>,
    account_feedback: Arc<AccountFeedbackStats>,
}

enum SessionAffinityLookup {
    Missing,
    Bound(ProviderAccountId),
    Unavailable,
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
        catalog: Arc<CodexCredentialCatalogService>,
        quota: Arc<CodexCredentialQuotaService>,
        agent_identity: Arc<CodexAgentIdentityTaskService>,
        account_feedback: Arc<AccountFeedbackStats>,
        cookie_policy: CodexCookiePolicy,
    ) -> Self {
        Self {
            provider_kind,
            repository,
            leases,
            session_affinity,
            catalog,
            quota,
            agent_identity,
            cookie_policy,
            risk_recovery: Mutex::new(HashMap::new()),
            account_feedback,
        }
    }

    pub async fn select(
        &self,
        request: &SelectCodexCredential<'_>,
    ) -> Result<CodexCredentialLease, CredentialSelectionError> {
        let accounts = self.repository.list_for_provider().await?;
        let accounts = accounts
            .into_iter()
            .filter(|account| {
                account.provider() == &self.provider_kind
                    && !matches!(
                        self.catalog.observed_model_support(
                            account.id(),
                            account.revision(),
                            request.upstream_model,
                        ),
                        Ok(Some(false))
                    )
            })
            .collect::<Vec<_>>();
        self.quota.prepare_scheduling(&accounts).await;
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
        let affinity_account = match request.session_affinity_key {
            Some(key) => match self.lookup_session_affinity(key).await {
                SessionAffinityLookup::Bound(account_id)
                    if candidates.iter().any(|candidate| {
                        candidate.account.id() == &account_id
                            && candidate.account.is_schedulable(SystemTime::now())
                    }) =>
                {
                    Some(account_id)
                }
                SessionAffinityLookup::Bound(_) => {
                    self.clear_session_affinity(key).await;
                    None
                }
                SessionAffinityLookup::Missing | SessionAffinityLookup::Unavailable => None,
            },
            None => None,
        };
        let mut excluded = request.attempt.excluded_accounts().clone();
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
        let preferred = pinned_account.or(affinity_account);
        let mut shortest_retry = None;

        loop {
            let context = AccountSelectionContext {
                policy: request.attempt.account_selection_policy(),
                now: SystemTime::now(),
                excluded_accounts: excluded.clone(),
                preferred_account: preferred.clone(),
                round_robin_cursor,
            };
            let Some(selected) = AccountSelector.select(&candidates, &context) else {
                return match shortest_retry {
                    Some(retry_after) => Err(CredentialSelectionError::CapacityUnavailable {
                        retry_after: Some(retry_after),
                    }),
                    None => Err(CredentialSelectionError::NoEligibleCredential),
                };
            };
            let account = selected.account.clone();
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
                    shortest_retry = minimum_duration(shortest_retry, retry_after);
                    excluded.insert(account.id().clone());
                }
                ProviderLeaseAcquisition::Acquired(guard) => {
                    let (account, runtime) = if account.authentication_kind()
                        == CODEX_AUTHENTICATION_KIND_AGENT_IDENTITY
                    {
                        let prepared = self
                            .agent_identity
                            .prepare(&account)
                            .await
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
                        _guard: guard,
                    });
                }
            }
        }
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
            CodexAccountFailure::RateLimited { retry_after } => {
                let until = now.checked_add(retry_after.unwrap_or(DEFAULT_RATE_LIMIT_COOLDOWN));
                (
                    AccountAvailability::QuotaExhausted,
                    Some("rate_limited".to_owned()),
                    until,
                    CookieRecovery::None,
                )
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

    pub async fn record_success(
        &self,
        account: &ProviderAccount,
        session_affinity_key: Option<&ProviderSessionAffinityKey>,
    ) {
        self.risk_recovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(account.id().as_str());
        let Some(key) = session_affinity_key else {
            return;
        };
        match self.lookup_session_affinity(key).await {
            SessionAffinityLookup::Bound(bound_account) if &bound_account != account.id() => {
                return;
            }
            SessionAffinityLookup::Unavailable => return,
            SessionAffinityLookup::Missing | SessionAffinityLookup::Bound(_) => {}
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
