//! xAI OAuth account 选择、Redis lease 与失败反馈。

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::Utc;
use gateway_core::engine::credential::{
    AccountCandidate, AccountEligibilityPolicy, AccountFeedbackStats, AccountSelectionContext,
    AccountSelector, CredentialState, ProviderAccount, ProviderAccountId, QuotaAccessState,
    QuotaEvidence, QuotaState,
};
use gateway_core::provider_ports::{
    ProviderCooldown, ProviderCooldownPort, ProviderCooldownScope, ProviderLeaseAcquisition,
    ProviderLeasePort, ProviderLeaseRequest, ProviderSchedulingLeaseRequest,
    ProviderScopedCooldown,
};
use gateway_core::routing::{ProviderKind, UpstreamModelId};

use super::catalog::{GrokCatalogScope, GrokCredentialCatalogCache, GrokCredentialQuotaService};
use super::repository::{GrokCredentialRepository, GrokCredentialRepositoryError};
use super::types::UpdateGrokCredentialState;
use crate::{
    GrokCredentialFailure, GrokCredentialFeedbackFuture, GrokSessionBinding, GrokSessionSelection,
    GrokSessionSelector, GrokSessionSelectorError, GrokSessionSelectorFuture, SecretValue,
    SelectedGrokSession,
};

const DEFAULT_RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(60);
const MAX_RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(24 * 60 * 60);
const STREAM_INTERRUPTION_COOLDOWN: Duration = Duration::from_secs(30);
const UNAUTHORIZED_COOLDOWN: Duration = Duration::from_secs(60);
const MODEL_ACCESS_DENIED_COOLDOWN: Duration = Duration::from_secs(5 * 60);
const MODEL_QUOTA_COOLDOWN: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_MODEL_COOLDOWN: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeCooldownScope {
    Account,
    Model,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RuntimeCooldown {
    until: SystemTime,
    scope: RuntimeCooldownScope,
}

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
        let diagnostic = request.eligibility() == AccountEligibilityPolicy::BypassForDiagnostic;
        let accounts = self
            .repository
            .list_accounts_for_provider()
            .await
            .map_err(|_| GrokSessionSelectorError::Unavailable)?;
        let accounts = if diagnostic {
            accounts
        } else {
            accounts
                .into_iter()
                .filter(|account| request.account_scope().allows(account.id()))
                .collect()
        };
        let catalog_eligible = if diagnostic {
            accounts
        } else {
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
            accounts
                .into_iter()
                .zip(account_scopes)
                .filter_map(|(account, scope)| {
                    let unsupported = scope.is_some_and(|scope| {
                        matches!(support_by_scope.get(&scope), Some(Ok(Some(false))))
                    });
                    (!unsupported).then_some(account)
                })
                .collect()
        };
        if catalog_eligible.is_empty() {
            return Err(GrokSessionSelectorError::NoEligibleSession);
        }
        if !diagnostic {
            self.quota.prepare_scheduling(&catalog_eligible).await;
        }

        let account_ids = catalog_eligible
            .iter()
            .map(|account| account.id().clone())
            .collect::<Vec<_>>();
        let scheduling = self
            .scheduling
            .load_state(
                request.client_api_key_id(),
                &self.provider_kind,
                &account_ids,
            )
            .await
            .map_err(|_| GrokSessionSelectorError::Unavailable)?;
        let runtime_cooldowns = if diagnostic {
            std::collections::BTreeMap::new()
        } else {
            self.runtime_cooldowns(&catalog_eligible, request.upstream_model())
                .await
        };
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
            eligibility: request.eligibility(),
            account_scope: (!diagnostic).then(|| Arc::clone(request.account_scope())),
        };
        let mut capacity_denied = false;
        let mut retry_after = None;
        while let Some(selection) = AccountSelector.select(&candidates, &context) {
            let selected = selection.candidate();
            let selected_id = selected.account.id().clone();
            let selected_revision = selected.account.revision();
            let allows_account_state_mutation = !diagnostic || selected.account.enabled();
            let lease = self
                .scheduling
                .try_acquire(ProviderLeaseRequest::Scheduling(
                    ProviderSchedulingLeaseRequest::new(
                        self.provider_kind.clone(),
                        selected_id.clone(),
                        selected_revision,
                        selected.account.effective_concurrency(
                            request
                                .account_selection_policy()
                                .max_concurrent_per_account(),
                        ),
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
            if !diagnostic
                && loaded
                    .refresh_token_expires_at
                    .is_some_and(|expires_at| expires_at <= Utc::now())
            {
                return Err(GrokSessionSelectorError::InvalidSession);
            }
            let binding = GrokSessionBinding::new(selected_id.as_str())
                .map_err(|_| GrokSessionSelectorError::InvalidSession)?;
            let upstream_user_id = loaded
                .account
                .upstream_user_id()
                .ok_or(GrokSessionSelectorError::InvalidSession)?;
            let session = SelectedGrokSession::new(
                selected_id,
                selected_revision,
                loaded.access_token,
                SecretValue::new(upstream_user_id),
                loaded
                    .account
                    .email()
                    .map(|value| SecretValue::new(value.to_owned())),
                binding,
                guard,
            )
            .map_err(|_| GrokSessionSelectorError::InvalidSession)?;
            return Ok(if allows_account_state_mutation {
                session
            } else {
                session.without_account_state_mutation()
            });
        }

        if capacity_denied {
            return Err(GrokSessionSelectorError::CapacityUnavailable { retry_after });
        }
        // 钉死账号时只看该账号，否则取最早退出 cooldown 的候选。
        let cooled = match request.required_account() {
            Some(required) => runtime_cooldowns.get(required).copied(),
            None => runtime_cooldowns
                .values()
                .copied()
                .min_by_key(|item| item.until),
        };
        if let Some(cooled) = cooled {
            let retry_after = cooled
                .until
                .duration_since(SystemTime::now())
                .ok()
                .filter(|remaining| !remaining.is_zero());
            return Err(match cooled.scope {
                RuntimeCooldownScope::Account => {
                    GrokSessionSelectorError::AccountCoolingDown { retry_after }
                }
                RuntimeCooldownScope::Model => {
                    GrokSessionSelectorError::ModelCoolingDown { retry_after }
                }
            });
        }
        Err(GrokSessionSelectorError::NoEligibleSession)
    }

    async fn runtime_cooldowns(
        &self,
        accounts: &[ProviderAccount],
        upstream_model: &UpstreamModelId,
    ) -> std::collections::BTreeMap<ProviderAccountId, RuntimeCooldown> {
        let now = SystemTime::now();
        let model_scope = ProviderCooldownScope::upstream_model(upstream_model.clone());
        // 账号级和当前模型级冷却都并发批量读取，避免逐账号串行往返。
        let account_reads = futures::future::join_all(
            accounts
                .iter()
                .map(|account| self.cooldowns.read(account.id())),
        );
        let model_reads = futures::future::join_all(
            accounts
                .iter()
                .map(|account| self.cooldowns.read_scoped(account.id(), &model_scope)),
        );
        let (account_reads, model_reads) = futures::join!(account_reads, model_reads);
        let mut cooled = std::collections::BTreeMap::new();
        for ((account, account_read), model_read) in
            accounts.iter().zip(account_reads).zip(model_reads)
        {
            if let Ok(Some(cooldown)) = account_read
                && cooldown.until() > now
            {
                // 账号级限流冷却跨凭据轮换保留：上游限流不因本地凭据更换解除。
                insert_runtime_cooldown(
                    &mut cooled,
                    account.id(),
                    cooldown.until(),
                    RuntimeCooldownScope::Account,
                );
            }
            if let Ok(Some(cooldown)) = model_read
                && cooldown.until() > now
            {
                insert_runtime_cooldown(
                    &mut cooled,
                    account.id(),
                    cooldown.until(),
                    RuntimeCooldownScope::Model,
                );
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

    async fn record_model_runtime_cooldown(
        &self,
        session: &SelectedGrokSession,
        upstream_model: UpstreamModelId,
        duration: Duration,
    ) {
        let cooldown_until = SystemTime::now()
            .checked_add(duration)
            .unwrap_or_else(SystemTime::now);
        let _ = self
            .cooldowns
            .put_scoped_if_later(ProviderScopedCooldown::new(
                session.account_id().clone(),
                session.credential_revision(),
                ProviderCooldownScope::upstream_model(upstream_model),
                cooldown_until,
            ))
            .await;
    }

    async fn persist_quota_failure(
        &self,
        session: &SelectedGrokSession,
        observed_at: chrono::DateTime<Utc>,
    ) {
        match self
            .repository
            .update_quota_access(
                session.account_id().clone(),
                session.credential_revision(),
                QuotaState::exhausted(QuotaEvidence::UsageLimitReached, observed_at.into(), None),
            )
            .await
        {
            Ok(()) => return,
            Err(
                GrokCredentialRepositoryError::Conflict
                | GrokCredentialRepositoryError::StaleCredentialRevision,
            ) => {}
            Err(error) => {
                tracing::warn!(
                    account_id = %session.account_id(),
                    error = %error,
                    "xAI account failure state write failed"
                );
                return;
            }
        }
        let current = match self.repository.load_current(session.account_id()).await {
            Ok(current) if current.account.enabled() => current,
            Ok(_) => return,
            Err(error) => {
                tracing::warn!(
                    account_id = %session.account_id(),
                    error = %error,
                    "xAI account failure state reload failed"
                );
                return;
            }
        };
        if let Err(error) = self
            .repository
            .update_quota_access(
                session.account_id().clone(),
                current.account.revision(),
                QuotaState::exhausted(QuotaEvidence::UsageLimitReached, observed_at.into(), None),
            )
            .await
        {
            tracing::warn!(
                account_id = %session.account_id(),
                error = %error,
                "xAI account failure state retry failed"
            );
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
            if !session.allows_account_state_mutation() {
                return;
            }
            let observed_at = Utc::now();
            match failure {
                GrokCredentialFailure::Unauthorized => {
                    self.record_runtime_cooldown(session, UNAUTHORIZED_COOLDOWN)
                        .await;
                }
                GrokCredentialFailure::QuotaExhausted
                | GrokCredentialFailure::FreeQuotaExhausted => {
                    self.persist_quota_failure(session, observed_at).await;
                }
                // bare 402 没有结构化 quota code，不能证明账号额度耗尽：
                // 只写短期账号级 runtime cooldown（TransientBackoff），
                // 不持久化 QuotaExhausted（结构化 QuotaExhausted/FreeQuotaExhausted 走上面）。
                GrokCredentialFailure::PaymentRequired { retry_after } => {
                    let retry_after = retry_after
                        .unwrap_or(DEFAULT_RATE_LIMIT_COOLDOWN)
                        .min(MAX_RATE_LIMIT_COOLDOWN);
                    self.record_runtime_cooldown(session, retry_after).await;
                }
                // xAI 免费模型额度按该模型滚动窗口恢复：限流写 model-scoped
                // runtime cooldown（Redis 跨重启保留），不进入持久化账号状态，
                // 不阻止该账号服务其他模型。
                GrokCredentialFailure::ModelQuotaExhausted {
                    upstream_model,
                    retry_after,
                } => {
                    let retry_after =
                        bounded_cooldown(retry_after, MODEL_QUOTA_COOLDOWN, MAX_MODEL_COOLDOWN);
                    self.record_model_runtime_cooldown(session, upstream_model, retry_after)
                        .await;
                }
                GrokCredentialFailure::ModelAccessDenied {
                    upstream_model,
                    retry_after,
                } => {
                    let retry_after = bounded_cooldown(
                        retry_after,
                        MODEL_ACCESS_DENIED_COOLDOWN,
                        MAX_MODEL_COOLDOWN,
                    );
                    self.record_model_runtime_cooldown(session, upstream_model, retry_after)
                        .await;
                }
                GrokCredentialFailure::RateLimited { retry_after } => {
                    let retry_after = retry_after
                        .unwrap_or(DEFAULT_RATE_LIMIT_COOLDOWN)
                        .min(MAX_RATE_LIMIT_COOLDOWN);
                    self.record_runtime_cooldown(session, retry_after).await;
                }
                GrokCredentialFailure::StreamInterrupted => {
                    self.record_runtime_cooldown(session, STREAM_INTERRUPTION_COOLDOWN)
                        .await;
                }
            }
        })
    }

    fn record_success<'a>(
        &'a self,
        session: &'a SelectedGrokSession,
    ) -> GrokCredentialFeedbackFuture<'a> {
        Box::pin(async move {
            if !session.allows_account_state_mutation() {
                return;
            }
            let Ok(current) = self.repository.load_current(session.account_id()).await else {
                return;
            };
            let account = &current.account;
            if account.revision() != session.credential_revision() || !account.enabled() {
                return;
            }
            let observed_at = Utc::now();
            if account.credential_state() != CredentialState::Ready
                && let Err(error) = self
                    .repository
                    .update_state(&UpdateGrokCredentialState {
                        account_id: account.id().clone(),
                        expected_revision: account.revision(),
                        credential_state: CredentialState::Ready,
                        error_reason: None,
                        error_message: None,
                        observed_at,
                    })
                    .await
            {
                tracing::warn!(
                    account_id = %account.id(),
                    error = %error,
                    "xAI account state recovery after successful upstream response failed"
                );
            }
            // 已经发出的并发请求可能晚于耗尽事实成功返回；真实成功可以收敛
            // 该事实，但 selector 不会为恢复探测而放行耗尽账号。
            if account.quota().access() == QuotaAccessState::Exhausted
                && let Err(error) = self
                    .repository
                    .update_quota_access(
                        account.id().clone(),
                        account.revision(),
                        QuotaState::allowed(observed_at.into()),
                    )
                    .await
            {
                tracing::warn!(
                    account_id = %account.id(),
                    error = %error,
                    "xAI quota recovery after successful upstream response failed"
                );
            }
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

fn bounded_cooldown(
    retry_after: Option<Duration>,
    default: Duration,
    maximum: Duration,
) -> Duration {
    retry_after
        .filter(|duration| !duration.is_zero())
        .unwrap_or(default)
        .min(maximum)
}

fn insert_runtime_cooldown(
    cooldowns: &mut std::collections::BTreeMap<ProviderAccountId, RuntimeCooldown>,
    account_id: &ProviderAccountId,
    until: SystemTime,
    scope: RuntimeCooldownScope,
) {
    let replacement = RuntimeCooldown { until, scope };
    if cooldowns
        .get(account_id)
        .is_none_or(|current| current.until < until)
    {
        cooldowns.insert(account_id.clone(), replacement);
    }
}
