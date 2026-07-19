//! xAI OAuth account 选择、Redis lease 与失败反馈。

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use chrono::Utc;
use gateway_core::engine::credential::{
    AccountCandidate, AccountRuntimeSignals, AccountSelectionContext, AccountSelector,
    CredentialRevision, ProviderAccountId,
};
use gateway_core::routing::ProviderInstanceId;

use super::catalog::GrokCredentialCatalogCache;
use super::repository::GrokCredentialRepository;
use super::types::{GrokCredentialAvailability, UpdateGrokCredentialState};
use crate::{
    GrokCredentialFailure, GrokCredentialFeedbackFuture, GrokSessionBinding, GrokSessionSelection,
    GrokSessionSelector, GrokSessionSelectorError, GrokSessionSelectorFuture, SecretValue,
    SelectedGrokSession,
};

const DEFAULT_RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(60);
const MAX_RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(24 * 60 * 60);

/// Redis coordinator 接收的全局调度策略事实。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrokCredentialLeaseRequest {
    pub provider_instance_id: ProviderInstanceId,
    pub account_id: ProviderAccountId,
    pub credential_revision: CredentialRevision,
    pub max_concurrent_per_account: u32,
    pub request_interval: Duration,
}

pub trait GrokCredentialLeaseGuard: Send + Sync + 'static {}

impl<T> GrokCredentialLeaseGuard for T where T: Send + Sync + 'static {}

pub enum GrokCredentialLeaseAcquisition {
    Acquired(Box<dyn GrokCredentialLeaseGuard>),
    Unavailable { retry_after: Option<Duration> },
}

impl fmt::Debug for GrokCredentialLeaseAcquisition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Acquired(_) => formatter.write_str("Acquired([LEASE])"),
            Self::Unavailable { retry_after } => formatter
                .debug_struct("Unavailable")
                .field("retry_after", retry_after)
                .finish(),
        }
    }
}

/// 一次 instance 的可失效调度状态；持久事实仍来自 PostgreSQL。
#[derive(Debug, Clone)]
pub struct GrokAccountSchedulingState {
    pub signals: BTreeMap<ProviderAccountId, AccountRuntimeSignals>,
    pub sticky_account: Option<ProviderAccountId>,
    pub round_robin_cursor: u64,
}

#[async_trait]
pub trait GrokCredentialLeaseCoordinator: Send + Sync {
    async fn load_scheduling_state(
        &self,
        provider_instance_id: &ProviderInstanceId,
        accounts: &[ProviderAccountId],
    ) -> Result<GrokAccountSchedulingState, GrokCredentialLeaseCoordinatorError>;

    async fn try_acquire(
        &self,
        request: &GrokCredentialLeaseRequest,
    ) -> Result<GrokCredentialLeaseAcquisition, GrokCredentialLeaseCoordinatorError>;
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GrokCredentialLeaseCoordinatorError {
    #[error("Grok credential lease coordinator is unavailable")]
    Unavailable,
}

/// 仅经 Core account port、TTL catalog cache 和 Redis lease 选择一个 OAuth session。
pub struct GrokAccountSessionSelector {
    repository: GrokCredentialRepository,
    catalog_cache: Arc<dyn GrokCredentialCatalogCache>,
    lease_coordinator: Arc<dyn GrokCredentialLeaseCoordinator>,
}

impl GrokAccountSessionSelector {
    #[must_use]
    pub fn new(
        repository: GrokCredentialRepository,
        catalog_cache: Arc<dyn GrokCredentialCatalogCache>,
        lease_coordinator: Arc<dyn GrokCredentialLeaseCoordinator>,
    ) -> Self {
        Self {
            repository,
            catalog_cache,
            lease_coordinator,
        }
    }

    async fn select_one(
        &self,
        request: GrokSessionSelection,
    ) -> Result<SelectedGrokSession, GrokSessionSelectorError> {
        let accounts = self
            .repository
            .list_accounts_for_instance(request.provider_instance_id())
            .await
            .map_err(|_| GrokSessionSelectorError::Unavailable)?;
        let mut catalog_eligible = Vec::new();
        for account in accounts {
            if self
                .catalog_cache
                .permits(
                    account.id(),
                    account.revision(),
                    request.upstream_model().as_str(),
                )
                .await
                .map_err(|_| GrokSessionSelectorError::Unavailable)?
            {
                catalog_eligible.push(account);
            }
        }
        if catalog_eligible.is_empty() {
            return Err(GrokSessionSelectorError::NoEligibleSession);
        }

        let account_ids = catalog_eligible
            .iter()
            .map(|account| account.id().clone())
            .collect::<Vec<_>>();
        let scheduling = self
            .lease_coordinator
            .load_scheduling_state(request.provider_instance_id(), &account_ids)
            .await
            .map_err(|_| GrokSessionSelectorError::Unavailable)?;
        let mut candidates = catalog_eligible
            .into_iter()
            .map(|account| {
                let signals = scheduling
                    .signals
                    .get(account.id())
                    .cloned()
                    .ok_or(GrokSessionSelectorError::Unavailable)?;
                Ok(AccountCandidate { account, signals })
            })
            .collect::<Result<Vec<_>, GrokSessionSelectorError>>()?;
        if let Some(required) = request.required_account() {
            candidates.retain(|candidate| candidate.account.id() == required);
        }

        let context = AccountSelectionContext {
            policy: request.account_selection_policy(),
            now: SystemTime::now(),
            excluded_accounts: request.excluded_accounts().clone(),
            sticky_account: request
                .required_account()
                .cloned()
                .or(scheduling.sticky_account),
            round_robin_cursor: scheduling.round_robin_cursor,
        };
        let mut capacity_denied = false;
        let mut retry_after = None;
        while let Some(selected) = AccountSelector.select(&candidates, &context) {
            let selected_id = selected.account.id().clone();
            let selected_revision = selected.account.revision();
            let lease = self
                .lease_coordinator
                .try_acquire(&GrokCredentialLeaseRequest {
                    provider_instance_id: request.provider_instance_id().clone(),
                    account_id: selected_id.clone(),
                    credential_revision: selected_revision,
                    max_concurrent_per_account: request
                        .account_selection_policy()
                        .max_concurrent_per_account()
                        .get(),
                    request_interval: request.account_selection_policy().request_interval(),
                })
                .await
                .map_err(|_| GrokSessionSelectorError::Unavailable)?;
            let guard = match lease {
                GrokCredentialLeaseAcquisition::Acquired(guard) => guard,
                GrokCredentialLeaseAcquisition::Unavailable {
                    retry_after: candidate_retry,
                } => {
                    capacity_denied = true;
                    retry_after = minimum_retry_after(retry_after, candidate_retry);
                    candidates.retain(|candidate| candidate.account.id() != &selected_id);
                    continue;
                }
            };

            let loaded = self
                .repository
                .load(&selected_id, selected_revision)
                .await
                .map_err(|_| GrokSessionSelectorError::InvalidSession)?;
            if loaded
                .refresh_token_expires_at
                .is_some_and(|expires_at| expires_at <= Utc::now())
            {
                return Err(GrokSessionSelectorError::InvalidSession);
            }
            let binding = GrokSessionBinding::new(selected_id.as_str())
                .map_err(|_| GrokSessionSelectorError::InvalidSession)?;
            return SelectedGrokSession::new(
                selected_id,
                selected_revision,
                loaded.access_token,
                SecretValue::new(loaded.account.upstream_user_id().to_owned()),
                loaded
                    .account
                    .email()
                    .map(|value| SecretValue::new(value.to_owned())),
                binding,
                guard,
            )
            .map_err(|_| GrokSessionSelectorError::InvalidSession);
        }

        if capacity_denied {
            Err(GrokSessionSelectorError::CapacityUnavailable { retry_after })
        } else {
            Err(GrokSessionSelectorError::NoEligibleSession)
        }
    }
}

impl fmt::Debug for GrokAccountSessionSelector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrokAccountSessionSelector")
            .field("repository", &self.repository)
            .field("catalog_cache", &"[TTL_CACHE]")
            .field("lease_coordinator", &"[LEASE_COORDINATOR]")
            .finish()
    }
}

impl GrokSessionSelector for GrokAccountSessionSelector {
    fn select(&self, request: GrokSessionSelection) -> GrokSessionSelectorFuture<'_> {
        Box::pin(self.select_one(request))
    }

    fn record_failure<'a>(
        &'a self,
        session: &'a SelectedGrokSession,
        failure: GrokCredentialFailure,
    ) -> GrokCredentialFeedbackFuture<'a> {
        Box::pin(async move {
            let observed_at = Utc::now();
            let (availability, reason, cooldown_until) = match failure {
                GrokCredentialFailure::Unauthorized => (
                    GrokCredentialAvailability::Invalid,
                    "upstream_unauthorized",
                    None,
                ),
                GrokCredentialFailure::QuotaExhausted => (
                    GrokCredentialAvailability::QuotaExhausted,
                    "upstream_quota_exhausted",
                    None,
                ),
                GrokCredentialFailure::RateLimited { retry_after } => {
                    let retry_after = retry_after
                        .unwrap_or(DEFAULT_RATE_LIMIT_COOLDOWN)
                        .min(MAX_RATE_LIMIT_COOLDOWN);
                    let cooldown_until = chrono::Duration::from_std(retry_after)
                        .ok()
                        .and_then(|duration| observed_at.checked_add_signed(duration));
                    (
                        GrokCredentialAvailability::Cooldown,
                        "upstream_rate_limited",
                        cooldown_until,
                    )
                }
            };
            let _ = self
                .repository
                .update_state(&UpdateGrokCredentialState {
                    account_id: session.account_id().clone(),
                    expected_revision: session.credential_revision(),
                    availability,
                    availability_reason: Some(reason.to_owned()),
                    cooldown_until,
                    observed_at,
                })
                .await;
        })
    }
}

fn minimum_retry_after(left: Option<Duration>, right: Option<Duration>) -> Option<Duration> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}
