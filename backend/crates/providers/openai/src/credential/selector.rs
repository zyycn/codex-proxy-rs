//! AttemptContext 驱动的 Codex 账号选择与 Redis lease port。

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use gateway_core::engine::credential::{
    AccountAvailability, AccountCandidate, AccountRuntimeSignals, AccountSelectionContext,
    AccountSelector, ProviderAccount, ProviderAccountId,
};
use gateway_core::engine::{AttemptContext, ContinuationAttempt};
use gateway_core::provider_ports::{
    ProviderLeaseAcquisition, ProviderLeaseGuard, ProviderLeasePort, ProviderLeaseRequest,
    ProviderSchedulingLeaseRequest, ProviderStoreError,
};
use gateway_core::routing::ProviderInstanceId;
use secrecy::ExposeSecret;
use thiserror::Error;
use url::Url;

use super::catalog::CodexCredentialCatalogService;
use super::cookie::CodexCookiePolicy;
use super::quota::CodexCredentialQuotaService;
use super::repository::{CodexCredentialRepository, CredentialRepositoryError};
use super::types::{CodexCookie, CodexCookieCaptureOutcome, CodexOAuthSecret, RuntimeCodexCookie};

const PROVIDER_NAME: &str = "openai";
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
    pub provider_instance_id: &'a ProviderInstanceId,
    pub upstream_model: &'a str,
    pub request_url: &'a Url,
    pub attempt: &'a AttemptContext,
}

pub struct CodexCredentialSelector {
    repository: CodexCredentialRepository,
    leases: Arc<dyn ProviderLeasePort>,
    catalog: Arc<CodexCredentialCatalogService>,
    quota: Arc<CodexCredentialQuotaService>,
    cookie_policy: CodexCookiePolicy,
    risk_recovery: Mutex<HashMap<String, RiskRecoveryState>>,
}

impl CodexCredentialSelector {
    #[must_use]
    pub fn new(
        repository: CodexCredentialRepository,
        leases: Arc<dyn ProviderLeasePort>,
        catalog: Arc<CodexCredentialCatalogService>,
        quota: Arc<CodexCredentialQuotaService>,
        cookie_policy: CodexCookiePolicy,
    ) -> Self {
        Self {
            repository,
            leases,
            catalog,
            quota,
            cookie_policy,
            risk_recovery: Mutex::new(HashMap::new()),
        }
    }

    pub async fn select(
        &self,
        request: &SelectCodexCredential<'_>,
    ) -> Result<CodexCredentialLease, CredentialSelectionError> {
        let accounts = self
            .repository
            .list_for_instance(request.provider_instance_id)
            .await?;
        let accounts = accounts
            .into_iter()
            .filter(|account| {
                account.provider().as_str() == PROVIDER_NAME
                    && account.instance() == request.provider_instance_id
                    && !matches!(
                        self.catalog.observed_model_support(
                            request.provider_instance_id,
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
            .load_state(request.provider_instance_id, &account_ids)
            .await?;
        let round_robin_cursor = scheduling.round_robin_cursor();
        let candidates = accounts
            .into_iter()
            .map(|account| {
                let signals = scheduling
                    .signals()
                    .get(account.id())
                    .cloned()
                    .unwrap_or(AccountRuntimeSignals {
                        in_flight: 0,
                        last_started_at: None,
                        quota_reset_at: None,
                        quota_remaining_rank: None,
                    })
                    .with_provider_quota(self.quota.scheduling_signals(&account));
                AccountCandidate { account, signals }
            })
            .collect::<Vec<_>>();
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
                .filter(|owner| {
                    owner.provider().as_str() == PROVIDER_NAME
                        && owner.instance() == request.provider_instance_id
                })
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
        let sticky = pinned_account.or_else(|| scheduling.sticky_account().cloned());
        let mut shortest_retry = None;

        loop {
            let context = AccountSelectionContext {
                policy: request.attempt.account_selection_policy(),
                now: SystemTime::now(),
                excluded_accounts: excluded.clone(),
                sticky_account: sticky.clone(),
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
                        request.provider_instance_id.clone(),
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
                    let runtime = self.repository.load_runtime_credential(&account).await?;
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
                        secret: runtime.secret,
                        cookies,
                        _guard: guard,
                    });
                }
            }
        }
    }

    pub async fn record_failure(
        &self,
        lease: &CodexCredentialLease,
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
                    AccountAvailability::Cooldown,
                    Some("rate_limited".to_owned()),
                    until,
                    CookieRecovery::None,
                )
            }
            CodexAccountFailure::CloudflareChallenge { retry_after } => {
                let delay = self.cloudflare_challenge_delay(lease.account_id(), now, retry_after);
                let cooldown_until = now.checked_add(delay);
                (
                    AccountAvailability::Cooldown,
                    Some("cloudflare_challenge".to_owned()),
                    cooldown_until,
                    cooldown_until.map_or(CookieRecovery::Clear, CookieRecovery::ExpireAt),
                )
            }
            CodexAccountFailure::CloudflarePathBlocked => {
                let blocked = self.record_cloudflare_path_block(lease.account_id(), now);
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
            .apply_state(&lease.account, availability, reason, cooldown_until, now)
            .await?;
        self.apply_cookie_recovery(lease, cookie_recovery).await?;
        Ok(())
    }

    pub fn record_success(&self, lease: &CodexCredentialLease) {
        self.risk_recovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(lease.account_id().as_str());
    }

    pub async fn capture_response_cookies(
        &self,
        lease: &CodexCredentialLease,
        response_origin: &Url,
        headers: &[String],
    ) -> Result<CodexCookieCaptureOutcome, CredentialSelectionError> {
        let parsed = self.cookie_policy.parse_response_headers(
            lease.account.id().as_str(),
            lease.account.revision().get(),
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
        let mut data = self.repository.load_complete_data(&lease.account).await?;
        for input in parsed.inputs {
            let scope = self.cookie_policy.validate_capture(
                &input.response_origin,
                input.domain_attribute.as_deref(),
                &input.name,
                &input.path,
            )?;
            data.cookies.retain(|cookie| {
                !(cookie.name == input.name
                    && cookie.domain == scope.domain
                    && cookie.path == input.path)
            });
            if !input.delete {
                data.cookies.push(CodexCookie {
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
        let revision = self
            .repository
            .compare_and_swap_data(&lease.account, data)
            .await?;
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
        lease: &CodexCredentialLease,
        recovery: CookieRecovery,
    ) -> Result<(), CredentialSelectionError> {
        if matches!(recovery, CookieRecovery::None) {
            return Ok(());
        }
        let mut data = self.repository.load_complete_data(&lease.account).await?;
        if data.cookies.is_empty() {
            return Ok(());
        }
        match recovery {
            CookieRecovery::None => return Ok(()),
            CookieRecovery::ExpireAt(expires_at) => {
                let expires_at = chrono::DateTime::<chrono::Utc>::from(expires_at);
                for cookie in &mut data.cookies {
                    cookie.expires_at = Some(
                        cookie
                            .expires_at
                            .map_or(expires_at, |current| current.min(expires_at)),
                    );
                }
            }
            CookieRecovery::Clear => data.cookies.clear(),
        }
        self.repository
            .compare_and_swap_data(&lease.account, data)
            .await?;
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
    secret: CodexOAuthSecret,
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
    pub const fn secret(&self) -> &CodexOAuthSecret {
        &self.secret
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
            .field("secret", &"<redacted>")
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
