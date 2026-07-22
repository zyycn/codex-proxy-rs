//! xAI OAuth account 选择、Redis lease 与失败反馈。

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::Utc;
use gateway_core::engine::credential::{
    AccountCandidate, AccountSelectionContext, AccountSelectionPolicy, AccountSelector,
    RotationStrategy,
};
use gateway_core::provider_ports::{
    ProviderLeaseAcquisition, ProviderLeasePort, ProviderLeaseRequest,
    ProviderSchedulingLeaseRequest,
};
use gateway_core::routing::ProviderKind;

use super::catalog::{GrokCredentialCatalogCache, GrokCredentialQuotaService};
use super::repository::GrokCredentialRepository;
use super::types::{GrokCredentialAvailability, UpdateGrokCredentialState};
use crate::{
    GrokCredentialFailure, GrokCredentialFeedbackFuture, GrokSessionBinding, GrokSessionSelection,
    GrokSessionSelector, GrokSessionSelectorError, GrokSessionSelectorFuture, SecretValue,
    SelectedGrokSession,
};

const DEFAULT_RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(60);
const MAX_RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(24 * 60 * 60);
const STREAM_INTERRUPTION_COOLDOWN: Duration = Duration::from_secs(30);

/// 仅经 Core account port、TTL catalog cache 和 Redis lease 选择一个 OAuth session。
pub struct GrokAccountSessionSelector {
    provider_kind: ProviderKind,
    repository: GrokCredentialRepository,
    catalog_cache: Arc<dyn GrokCredentialCatalogCache>,
    quota: Arc<GrokCredentialQuotaService>,
    scheduling: Arc<dyn ProviderLeasePort>,
}

impl GrokAccountSessionSelector {
    #[must_use]
    pub fn new(
        provider_kind: ProviderKind,
        repository: GrokCredentialRepository,
        catalog_cache: Arc<dyn GrokCredentialCatalogCache>,
        quota: Arc<GrokCredentialQuotaService>,
        scheduling: Arc<dyn ProviderLeasePort>,
    ) -> Self {
        Self {
            provider_kind,
            repository,
            catalog_cache,
            quota,
            scheduling,
        }
    }

    async fn select_one(
        &self,
        request: GrokSessionSelection,
    ) -> Result<SelectedGrokSession, GrokSessionSelectorError> {
        let accounts = self
            .repository
            .list_accounts_for_provider()
            .await
            .map_err(|_| GrokSessionSelectorError::Unavailable)?;
        self.quota.prepare_scheduling(&accounts).await;
        let mut catalog_eligible = Vec::new();
        for account in accounts {
            let support = self
                .catalog_cache
                .observed_model_support(
                    account.id(),
                    account.revision(),
                    request.upstream_model().as_str(),
                )
                .await;
            if !matches!(support, Ok(Some(false))) {
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
            .scheduling
            .load_state(&self.provider_kind, &account_ids)
            .await
            .map_err(|_| GrokSessionSelectorError::Unavailable)?;
        let mut candidates = catalog_eligible
            .into_iter()
            .map(|account| {
                let signals = scheduling
                    .signals()
                    .get(account.id())
                    .cloned()
                    .ok_or(GrokSessionSelectorError::Unavailable)?
                    .with_provider_quota(self.quota.scheduling_signals(&account));
                Ok(AccountCandidate { account, signals })
            })
            .collect::<Result<Vec<_>, GrokSessionSelectorError>>()?;
        if let Some(required) = request.required_account() {
            candidates.retain(|candidate| candidate.account.id() == required);
        }

        let affinity_account = request.affinity().and_then(|affinity| {
            candidates
                .iter()
                .filter(|candidate| !request.excluded_accounts().contains(candidate.account.id()))
                .max_by_key(|candidate| affinity.score(candidate.account.id()))
                .map(|candidate| candidate.account.id().clone())
        });
        let configured_policy = request.account_selection_policy();
        let policy = if request.required_account().is_none() && affinity_account.is_some() {
            AccountSelectionPolicy::new(
                RotationStrategy::Sticky,
                configured_policy.max_concurrent_per_account(),
                configured_policy.request_interval(),
            )
        } else {
            configured_policy
        };
        let context = AccountSelectionContext {
            policy,
            now: SystemTime::now(),
            excluded_accounts: request.excluded_accounts().clone(),
            sticky_account: request
                .required_account()
                .cloned()
                .or(affinity_account)
                .or_else(|| scheduling.sticky_account().cloned()),
            round_robin_cursor: scheduling.round_robin_cursor(),
        };
        let mut capacity_denied = false;
        let mut retry_after = None;
        while let Some(selected) = AccountSelector.select(&candidates, &context) {
            let selected_id = selected.account.id().clone();
            let selected_revision = selected.account.revision();
            let lease = self
                .scheduling
                .try_acquire(ProviderLeaseRequest::Scheduling(
                    ProviderSchedulingLeaseRequest::new(
                        self.provider_kind.clone(),
                        selected_id.clone(),
                        selected_revision,
                        request
                            .account_selection_policy()
                            .max_concurrent_per_account(),
                        request.account_selection_policy().request_interval(),
                        request.deadline(),
                    ),
                ))
                .await
                .map_err(|_| GrokSessionSelectorError::Unavailable)?;
            let guard = match lease {
                ProviderLeaseAcquisition::Acquired(guard) => guard,
                ProviderLeaseAcquisition::Busy {
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
            .field("quota", &self.quota)
            .field("scheduling", &"[SCHEDULING_PORT]")
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
                GrokCredentialFailure::StreamInterrupted => {
                    let cooldown_until = chrono::Duration::from_std(STREAM_INTERRUPTION_COOLDOWN)
                        .ok()
                        .and_then(|duration| observed_at.checked_add_signed(duration));
                    (
                        GrokCredentialAvailability::Cooldown,
                        "upstream_stream_interrupted",
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
