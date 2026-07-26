//! xAI OAuth account 选择、Redis lease 与失败反馈。

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::Utc;
use gateway_core::engine::credential::{
    AccountCandidate, AccountFeedbackStats, AccountSelectionContext, AccountSelector,
    ProviderAccount, ProviderAccountId,
};
use gateway_core::provider_ports::{
    ProviderCooldown, ProviderCooldownPort, ProviderLeaseAcquisition, ProviderLeasePort,
    ProviderLeaseRequest, ProviderSchedulingLeaseRequest,
};
use gateway_core::routing::ProviderKind;

use super::catalog::{GrokCatalogScope, GrokCredentialCatalogCache, GrokCredentialQuotaService};
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
const UNAUTHORIZED_COOLDOWN: Duration = Duration::from_secs(60);

/// 仅经 Core account port、TTL catalog cache 和 Redis lease 选择一个 OAuth session。
pub struct GrokAccountSessionSelector {
    provider_kind: ProviderKind,
    repository: GrokCredentialRepository,
    catalog_cache: Arc<dyn GrokCredentialCatalogCache>,
    quota: Arc<GrokCredentialQuotaService>,
    scheduling: Arc<dyn ProviderLeasePort>,
    cooldowns: Arc<dyn ProviderCooldownPort>,
    account_feedback: Arc<AccountFeedbackStats>,
}

impl GrokAccountSessionSelector {
    #[must_use]
    pub fn new(
        provider_kind: ProviderKind,
        repository: GrokCredentialRepository,
        catalog_cache: Arc<dyn GrokCredentialCatalogCache>,
        quota: Arc<GrokCredentialQuotaService>,
        scheduling: Arc<dyn ProviderLeasePort>,
        cooldowns: Arc<dyn ProviderCooldownPort>,
        account_feedback: Arc<AccountFeedbackStats>,
    ) -> Self {
        Self {
            provider_kind,
            repository,
            catalog_cache,
            quota,
            scheduling,
            cooldowns,
            account_feedback,
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
        // 目录支持度按套餐 scope 去重后并发批量读取，避免逐账号串行往返。
        let account_scopes = accounts
            .iter()
            .map(|account| GrokCatalogScope::for_account(account).ok())
            .collect::<Vec<_>>();
        let unique_scopes = account_scopes
            .iter()
            .flatten()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let support_reads = futures::future::join_all(unique_scopes.iter().map(|scope| {
            self.catalog_cache
                .observed_model_support(scope, request.upstream_model().as_str())
        }))
        .await;
        let support_by_scope = unique_scopes
            .into_iter()
            .zip(support_reads)
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut catalog_eligible = Vec::new();
        for (account, scope) in accounts.into_iter().zip(account_scopes) {
            let unsupported = scope
                .is_some_and(|scope| matches!(support_by_scope.get(&scope), Some(Ok(Some(false)))));
            if !unsupported {
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
        let runtime_cooldowns = self.runtime_cooldowns(&catalog_eligible).await;
        let mut candidates = catalog_eligible
            .into_iter()
            .filter(|account| !runtime_cooldowns.contains_key(account.id()))
            .map(|account| {
                let health = self
                    .account_feedback
                    .scheduling_signals(&self.provider_kind, account.id());
                let signals = scheduling
                    .signals()
                    .get(account.id())
                    .cloned()
                    .ok_or(GrokSessionSelectorError::Unavailable)?
                    .with_provider_quota(self.quota.scheduling_signals(&account))
                    .with_runtime_health(health.0, health.1);
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
        let context = AccountSelectionContext {
            policy: request.account_selection_policy(),
            now: SystemTime::now(),
            excluded_accounts: request.excluded_accounts().clone(),
            preferred_account: request.required_account().cloned().or(affinity_account),
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
            return Err(GrokSessionSelectorError::CapacityUnavailable { retry_after });
        }
        // 钉死账号时只看该账号，否则取最早退出 cooldown 的候选。
        let cooled_until = match request.required_account() {
            Some(required) => runtime_cooldowns.get(required).copied(),
            None => runtime_cooldowns.values().copied().min(),
        };
        if let Some(until) = cooled_until {
            return Err(GrokSessionSelectorError::AccountCoolingDown {
                retry_after: until
                    .duration_since(SystemTime::now())
                    .ok()
                    .filter(|remaining| !remaining.is_zero()),
            });
        }
        Err(GrokSessionSelectorError::NoEligibleSession)
    }

    async fn runtime_cooldowns(
        &self,
        accounts: &[ProviderAccount],
    ) -> std::collections::BTreeMap<ProviderAccountId, SystemTime> {
        let now = SystemTime::now();
        // 冷却状态并发批量读取，避免逐账号串行往返。
        let reads = futures::future::join_all(
            accounts
                .iter()
                .map(|account| self.cooldowns.read(account.id())),
        )
        .await;
        let mut cooled = std::collections::BTreeMap::new();
        for (account, read) in accounts.iter().zip(reads) {
            let Ok(Some(cooldown)) = read else {
                continue;
            };
            if cooldown.credential_revision() != account.revision() {
                if cooldown.credential_revision() < account.revision() {
                    let _ = self.cooldowns.clear(account.id(), account.revision()).await;
                }
                continue;
            }
            if cooldown.until() > now {
                cooled.insert(account.id().clone(), cooldown.until());
            }
        }
        cooled
    }

    async fn record_runtime_cooldown(&self, session: &SelectedGrokSession, duration: Duration) {
        let cooldown_until = SystemTime::now()
            .checked_add(duration)
            .unwrap_or_else(SystemTime::now);
        let _ = self
            .cooldowns
            .put_if_later(ProviderCooldown::new(
                session.account_id().clone(),
                session.credential_revision(),
                cooldown_until,
            ))
            .await;
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
            .field("cooldowns", &"[COOLDOWN_PORT]")
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
            let persistent = match failure {
                GrokCredentialFailure::Unauthorized => {
                    self.record_runtime_cooldown(session, UNAUTHORIZED_COOLDOWN)
                        .await;
                    return;
                }
                GrokCredentialFailure::QuotaExhausted => (
                    GrokCredentialAvailability::QuotaExhausted,
                    "upstream_quota_exhausted",
                    None,
                ),
                GrokCredentialFailure::RateLimited { retry_after } => {
                    let retry_after = retry_after
                        .unwrap_or(DEFAULT_RATE_LIMIT_COOLDOWN)
                        .min(MAX_RATE_LIMIT_COOLDOWN);
                    self.record_runtime_cooldown(session, retry_after).await;
                    return;
                }
                GrokCredentialFailure::StreamInterrupted => {
                    self.record_runtime_cooldown(session, STREAM_INTERRUPTION_COOLDOWN)
                        .await;
                    return;
                }
            };
            let (availability, reason, cooldown_until) = persistent;
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
