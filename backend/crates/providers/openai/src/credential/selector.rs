//! AttemptContext 驱动的 Codex 账号选择与 Redis lease port。

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use gateway_core::engine::credential::{
    AccountAvailability, AccountCandidate, AccountRuntimeSignals, AccountSelectionContext,
    AccountSelector, ProviderAccount, ProviderAccountId,
};
use gateway_core::engine::{AttemptContext, UpstreamSendState};
use gateway_core::error::ProviderErrorKind;
use gateway_core::routing::ProviderInstanceId;
use secrecy::ExposeSecret;
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;

use super::cookie::CodexCookiePolicy;
use super::repository::{CodexCredentialRepository, CredentialRepositoryError};
use super::types::{CodexCookie, CodexCookieCaptureOutcome, CodexOAuthSecret, RuntimeCodexCookie};

const PROVIDER_NAME: &str = "openai";
const DEFAULT_COOLDOWN: Duration = Duration::from_secs(30);

pub struct SelectCodexCredential<'a> {
    pub provider_instance_id: &'a ProviderInstanceId,
    pub request_url: &'a Url,
    pub attempt: &'a AttemptContext,
}

#[derive(Debug, Clone)]
pub struct CredentialLeaseRequest {
    pub account_id: ProviderAccountId,
    pub max_concurrent: u32,
    pub request_interval: Duration,
    pub deadline: SystemTime,
}

pub trait CredentialLeaseGuard: Send + Sync + 'static {}
impl<T> CredentialLeaseGuard for T where T: Send + Sync + 'static {}

pub enum LeaseAcquisition {
    Acquired(Box<dyn CredentialLeaseGuard>),
    Busy { retry_after: Option<Duration> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CredentialLeaseCoordinatorError {
    #[error("credential lease runtime is unavailable")]
    Unavailable,
}

/// Redis adapter 实现：信号可失效，lease guard 通过 Drop 释放。
#[async_trait]
pub trait CredentialLeaseCoordinator: Send + Sync {
    async fn runtime_signals(
        &self,
        accounts: &[ProviderAccountId],
    ) -> Result<BTreeMap<ProviderAccountId, AccountRuntimeSignals>, CredentialLeaseCoordinatorError>;

    fn next_round_robin_cursor(
        &self,
        provider_instance_id: &ProviderInstanceId,
    ) -> Result<u64, CredentialLeaseCoordinatorError>;

    async fn try_acquire(
        &self,
        request: CredentialLeaseRequest,
    ) -> Result<LeaseAcquisition, CredentialLeaseCoordinatorError>;
}

pub struct CodexCredentialSelector {
    repository: CodexCredentialRepository,
    leases: Arc<dyn CredentialLeaseCoordinator>,
    cookie_policy: CodexCookiePolicy,
}

impl CodexCredentialSelector {
    #[must_use]
    pub const fn new(
        repository: CodexCredentialRepository,
        leases: Arc<dyn CredentialLeaseCoordinator>,
        cookie_policy: CodexCookiePolicy,
    ) -> Self {
        Self {
            repository,
            leases,
            cookie_policy,
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
            })
            .collect::<Vec<_>>();
        let account_ids = accounts
            .iter()
            .map(|account| account.id().clone())
            .collect::<Vec<_>>();
        let signals = self.leases.runtime_signals(&account_ids).await?;
        let round_robin_cursor = self
            .leases
            .next_round_robin_cursor(request.provider_instance_id)?;
        let candidates = accounts
            .into_iter()
            .map(|account| AccountCandidate {
                signals: signals
                    .get(account.id())
                    .cloned()
                    .unwrap_or(AccountRuntimeSignals {
                        in_flight: 0,
                        last_started_at: None,
                        quota_reset_at: None,
                        quota_remaining_rank: None,
                    }),
                account,
            })
            .collect::<Vec<_>>();
        let mut excluded = request.attempt.excluded_accounts().clone();
        let continuation_account = request
            .attempt
            .continuation()
            .map(|continuation| continuation.account().clone());
        let required_account = request.attempt.required_account().cloned();
        if required_account
            .as_ref()
            .zip(continuation_account.as_ref())
            .is_some_and(|(required, continuation)| required != continuation)
        {
            return Err(CredentialSelectionError::NoEligibleCredential);
        }
        let sticky = required_account.or(continuation_account);
        if let Some(required) = sticky.as_ref() {
            for candidate in &candidates {
                if candidate.account.id() != required {
                    excluded.insert(candidate.account.id().clone());
                }
            }
        }
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
                .try_acquire(CredentialLeaseRequest {
                    account_id: account.id().clone(),
                    max_concurrent: policy.max_concurrent_per_account().get(),
                    request_interval: policy.request_interval(),
                    deadline: request.attempt.deadline(),
                })
                .await?
            {
                LeaseAcquisition::Busy { retry_after } => {
                    shortest_retry = minimum_duration(shortest_retry, retry_after);
                    excluded.insert(account.id().clone());
                }
                LeaseAcquisition::Acquired(guard) => {
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
                        installation_id: installation_id(
                            account
                                .upstream_account_id()
                                .unwrap_or(account.upstream_user_id()),
                        ),
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
        kind: ProviderErrorKind,
        send_state: UpstreamSendState,
        retry_after: Option<Duration>,
    ) {
        let now = SystemTime::now();
        let (availability, reason, cooldown_until) = match kind {
            ProviderErrorKind::Unauthorized => (
                AccountAvailability::Invalid,
                Some("credential_rejected".to_owned()),
                None,
            ),
            ProviderErrorKind::QuotaExhausted => (
                AccountAvailability::QuotaExhausted,
                Some("quota_exhausted".to_owned()),
                None,
            ),
            ProviderErrorKind::RateLimited if send_state != UpstreamSendState::Ambiguous => {
                let until = now.checked_add(retry_after.unwrap_or(DEFAULT_COOLDOWN));
                (
                    AccountAvailability::Cooldown,
                    Some("rate_limited".to_owned()),
                    until,
                )
            }
            _ => return,
        };
        let _ = self
            .repository
            .apply_state(&lease.account, availability, reason, cooldown_until, now)
            .await;
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
}

impl fmt::Debug for CodexCredentialSelector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexCredentialSelector")
            .field("repository", &"ProviderAccountStore")
            .field("leases", &"CredentialLeaseCoordinator")
            .field("cookie_policy", &self.cookie_policy)
            .finish()
    }
}

pub struct CodexCredentialLease {
    account: ProviderAccount,
    secret: CodexOAuthSecret,
    cookies: Vec<RuntimeCodexCookie>,
    installation_id: String,
    _guard: Box<dyn CredentialLeaseGuard>,
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

impl From<CredentialLeaseCoordinatorError> for CredentialSelectionError {
    fn from(_: CredentialLeaseCoordinatorError) -> Self {
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

fn installation_id(identity: &str) -> String {
    let digest = Sha256::digest(identity.as_bytes());
    format!("cpr-{}", hex::encode(&digest[..16]))
}
