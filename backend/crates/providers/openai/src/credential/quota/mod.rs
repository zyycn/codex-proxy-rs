//! Codex quota 服务编排：请求级额度投影、被动/主动同步与 429 合成。
//!
//! - [`snapshot`]：快照/窗口解析、聚合、O(1) 滚动与调度信号。
//! - [`recovery`]：402 恢复证据与 availability 转换。

pub(crate) mod recovery;
pub(crate) mod snapshot;

pub use snapshot::{
    CodexAccountQuotaSnapshot, CodexQuotaFact, CodexQuotaWindow, CodexQuotaWindowKind,
    CodexQuotaWindowRole, parse_codex_quota_usage,
};

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::Utc;
use gateway_core::engine::credential::{
    AccountAvailability, AccountQuotaSignals, CredentialRevision, OpaqueProviderData,
    ProviderAccount, ProviderAccountId, ProviderAccountStore, QuotaObservation, QuotaWriteOutcome,
};
use gateway_core::provider_ports::{ProviderCooldown, ProviderCooldownPort};
use gateway_protocol::openai::events::{
    ParsedRateLimits, RateLimitDetails, RateLimitWindow, parse_rate_limit_headers,
};
use reqwest::Client;
use secrecy::ExposeSecret;
use serde_json::{Map, Value};
use thiserror::Error;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::transport::profile::CodexWireProfileState;
use crate::transport::{CodexBackendClient, CodexClientError, CodexRequestContext};

use super::agent_identity::{CodexAgentIdentityTaskService, PreparedCodexRuntimeCredential};
use super::repository::{CodexCredentialRepository, CredentialRepositoryError};
use recovery::{quota_state_reason, quota_state_transition, quota_success_state};
use snapshot::{
    parse_account_quota_snapshot, quota_projection_ttl, quota_snapshot_from_observation,
    scheduling_signals_from_snapshot,
};

const DEFAULT_RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(60);
pub(crate) const QUOTA_SCHEDULING_TTL: Duration = Duration::from_secs(10 * 60);
const QUOTA_HYDRATION_FAILURE_TTL: Duration = Duration::from_secs(5);
const QUOTA_PERIODIC_REFRESH_MIN_INTERVAL: Duration = Duration::from_secs(30 * 60);
/// 首次 OAuth 异步观察失败时，由既有 quota worker 兜底重试的单轮上限。
const INITIAL_QUOTA_SYNC_BATCH: usize = 100;
// 5xx 上游拒绝的短退避重试预算；指数退避 1s/2s，吞掉瞬时抖动。
const QUOTA_FETCH_5XX_MAX_RETRIES: u32 = 2;
const QUOTA_FETCH_5XX_BASE_DELAY: Duration = Duration::from_secs(1);

/// OpenAI Provider 主动额度刷新的调度策略。
///
/// 正常账号依赖请求响应的被动额度同步；周期 worker 仅复核已耗尽账号。
/// 该策略只控制 Provider 自己的 quota worker 频率，避免把 OpenAI
/// 额度语义泄漏到公共调度层。
#[derive(Debug, Clone, Copy)]
pub struct CodexQuotaRefreshPolicy {
    interval: Duration,
}

impl CodexQuotaRefreshPolicy {
    #[must_use]
    pub const fn new(interval: Duration) -> Self {
        Self { interval }
    }

    #[must_use]
    pub const fn interval(self) -> Duration {
        self.interval
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CodexQuotaSyncSummary {
    pub updated: u64,
    pub exhausted: u64,
    pub banned: u64,
    pub transient: u64,
    pub stale: u64,
}

impl CodexQuotaSyncSummary {
    #[must_use]
    pub const fn has_operational_failures(self) -> bool {
        self.transient > 0
    }
}

#[derive(Debug, Error)]
pub enum CodexCredentialQuotaError {
    #[error("Codex quota response is invalid")]
    InvalidCredentialData,
    #[error("Codex OAuth access token must be refreshed before querying quota")]
    CredentialRefreshRequired,
    #[error(transparent)]
    Repository(#[from] CredentialRepositoryError),
    #[error("provider account store is unavailable: {detail}")]
    Store { detail: String },
    #[error("Codex quota account was not found")]
    NotFound,
    #[error("Codex quota credential revision is stale")]
    RevisionConflict,
    #[error("Codex quota upstream query failed: {detail}")]
    Upstream { detail: String },
}

impl From<gateway_core::error::StoreError> for CodexCredentialQuotaError {
    fn from(error: gateway_core::error::StoreError) -> Self {
        Self::Store {
            detail: error.to_string(),
        }
    }
}

pub struct CodexCredentialQuotaService {
    repository: CodexCredentialRepository,
    store: Arc<dyn ProviderAccountStore>,
    profile: CodexWireProfileState,
    http: Client,
    base_url: String,
    agent_identity: Arc<CodexAgentIdentityTaskService>,
    cooldowns: Arc<dyn ProviderCooldownPort>,
    scheduling: CodexQuotaSchedulingProjection,
}

#[derive(Clone, Default)]
struct CodexQuotaSchedulingProjection {
    state: Arc<RwLock<CodexQuotaProjectionState>>,
    hydration: Arc<Mutex<()>>,
}

#[derive(Default)]
struct CodexQuotaProjectionState {
    next_version: u64,
    entries: BTreeMap<ProviderAccountId, CodexQuotaSchedulingEntry>,
    last_periodic_refresh_at: BTreeMap<ProviderAccountId, Instant>,
}

#[derive(Debug, Clone, Copy)]
struct CodexQuotaSchedulingEntry {
    version: u64,
    revision: CredentialRevision,
    expires_at: Instant,
    signals: Option<AccountQuotaSignals>,
}

#[derive(Clone)]
struct CodexQuotaHydrationTarget {
    account: ProviderAccount,
    expected_version: Option<u64>,
}

struct FetchedCodexQuota {
    account: ProviderAccount,
    value: Value,
}

enum CodexQuotaFetchAttemptError {
    InvalidCredential,
    Upstream(CodexClientError),
}

enum CodexQuotaFetchError {
    InvalidCredential,
    Recovery,
    Upstream {
        account: Box<ProviderAccount>,
        error: CodexClientError,
    },
}

impl CodexQuotaSchedulingProjection {
    fn hydration_targets(&self, accounts: &[ProviderAccount]) -> Vec<CodexQuotaHydrationTarget> {
        let state = self
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        accounts
            .iter()
            .filter_map(|account| {
                let current = state.entries.get(account.id());
                current
                    .is_none_or(|entry| {
                        entry.revision != account.revision() || now >= entry.expires_at
                    })
                    .then(|| CodexQuotaHydrationTarget {
                        account: account.clone(),
                        expected_version: current.map(|entry| entry.version),
                    })
            })
            .collect()
    }

    fn observe(&self, snapshot: &CodexAccountQuotaSnapshot) -> bool {
        let Some(remaining_ttl) = quota_projection_ttl(snapshot) else {
            return false;
        };
        self.replace(
            snapshot.account_id().clone(),
            snapshot.credential_revision(),
            remaining_ttl,
            scheduling_signals_from_snapshot(snapshot),
        );
        true
    }

    fn mark_unknown_if_unchanged(&self, target: &CodexQuotaHydrationTarget, ttl: Duration) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .entries
            .get(target.account.id())
            .map(|entry| entry.version)
            != target.expected_version
        {
            return;
        }
        insert_projection_entry(
            &mut state,
            target.account.id().clone(),
            target.account.revision(),
            ttl,
            None,
        );
    }

    fn observe_if_unchanged(
        &self,
        target: &CodexQuotaHydrationTarget,
        snapshot: &CodexAccountQuotaSnapshot,
    ) -> bool {
        let Some(remaining_ttl) = quota_projection_ttl(snapshot) else {
            return false;
        };
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .entries
            .get(target.account.id())
            .map(|entry| entry.version)
            != target.expected_version
        {
            return true;
        }
        insert_projection_entry(
            &mut state,
            snapshot.account_id().clone(),
            snapshot.credential_revision(),
            remaining_ttl,
            scheduling_signals_from_snapshot(snapshot),
        );
        true
    }

    fn replace(
        &self,
        account_id: ProviderAccountId,
        revision: CredentialRevision,
        ttl: Duration,
        signals: Option<AccountQuotaSignals>,
    ) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        insert_projection_entry(&mut state, account_id, revision, ttl, signals);
    }

    fn signals(&self, account: &ProviderAccount) -> Option<AccountQuotaSignals> {
        let state = self
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .entries
            .get(account.id())
            .filter(|entry| {
                entry.revision == account.revision() && Instant::now() < entry.expires_at
            })
            .and_then(|entry| entry.signals)
    }

    fn reserve_periodic_refreshes(
        &self,
        accounts: Vec<ProviderAccount>,
        now: SystemTime,
    ) -> Vec<ProviderAccount> {
        let candidates = accounts
            .into_iter()
            .filter_map(|account| {
                let limit_reached = self
                    .signals(&account)
                    .is_some_and(|signals| signals.limit_reached());
                quota_refresh_candidate(account, now, limit_reached)
            })
            .collect::<Vec<_>>();
        let candidate_ids = candidates
            .iter()
            .map(|account| account.id().clone())
            .collect::<BTreeSet<_>>();
        let refreshed_at = Instant::now();
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .last_periodic_refresh_at
            .retain(|account_id, _| candidate_ids.contains(account_id));

        // 正常账号只由真实请求的响应头和 `codex.rate_limits` 被动同步。定时器只兜底
        // 已耗尽账号，且不依据 reset 时间跳过复核；每个账号仍至少间隔 30 分钟。
        let mut reserved = Vec::new();
        for account in candidates {
            if !periodic_quota_refresh_due(&state, account.id(), refreshed_at) {
                continue;
            }
            state
                .last_periodic_refresh_at
                .insert(account.id().clone(), refreshed_at);
            reserved.push(account);
        }
        reserved
    }
}

fn quota_refresh_candidate(
    account: ProviderAccount,
    now: SystemTime,
    limit_reached: bool,
) -> Option<ProviderAccount> {
    // 定时器只兜底已耗尽或任一窗口触顶的账号；
    // 正常账号由真实请求的响应头被动同步。
    (eligible_periodic_quota_refresh(&account, now)
        && (account.availability() == AccountAvailability::QuotaExhausted || limit_reached))
        .then_some(account)
}

fn periodic_quota_refresh_due(
    state: &CodexQuotaProjectionState,
    account_id: &ProviderAccountId,
    now: Instant,
) -> bool {
    state
        .last_periodic_refresh_at
        .get(account_id)
        .is_none_or(|last| {
            now.saturating_duration_since(*last) >= QUOTA_PERIODIC_REFRESH_MIN_INTERVAL
        })
}

fn insert_projection_entry(
    state: &mut CodexQuotaProjectionState,
    account_id: ProviderAccountId,
    revision: CredentialRevision,
    ttl: Duration,
    signals: Option<AccountQuotaSignals>,
) {
    state.next_version = state.next_version.saturating_add(1);
    state.entries.insert(
        account_id,
        CodexQuotaSchedulingEntry {
            version: state.next_version,
            revision,
            expires_at: Instant::now() + ttl,
            signals,
        },
    );
}

impl CodexCredentialQuotaService {
    pub fn new(
        repository: CodexCredentialRepository,
        profile: CodexWireProfileState,
        http: Client,
        base_url: String,
        agent_identity: Arc<CodexAgentIdentityTaskService>,
        cooldowns: Arc<dyn ProviderCooldownPort>,
    ) -> Self {
        Self {
            store: Arc::clone(repository.store()),
            repository,
            profile,
            http,
            base_url,
            agent_identity,
            cooldowns,
            scheduling: CodexQuotaSchedulingProjection::default(),
        }
    }

    /// 写入新快照前读取 core primary 已确认耗尽窗口的 reset，用于判断该窗口是否真的推进。
    async fn previous_confirmed_reset(&self, account: &ProviderAccount) -> Option<SystemTime> {
        let observations = self
            .store
            .get_quotas(std::slice::from_ref(account.id()))
            .await
            .ok()?;
        let observation = observations.into_iter().find(|observation| {
            observation.account_id == *account.id()
                && observation.expected_revision == account.revision()
        })?;
        let snapshot = quota_snapshot_from_observation(&observation)?;
        snapshot
            .core_primary_window()
            .and_then(|window| window.reset_at())
            .map(SystemTime::from)
    }

    /// 将已确认的额度拒绝投影为 100%，使展示、调度和恢复共用同一窗口事实。
    ///
    /// `reset_at` 来自官方 `usage_limit_reached` 错误体；缺失时保留当前快照的 reset。
    pub(crate) async fn record_confirmed_exhaustion(
        &self,
        account: &ProviderAccount,
        reset_at: Option<i64>,
    ) -> Result<(), CodexCredentialQuotaError> {
        let existing = self
            .store
            .get_quotas(std::slice::from_ref(account.id()))
            .await?
            .into_iter()
            .find(|observation| {
                observation.account_id == *account.id()
                    && observation.expected_revision == account.revision()
            })
            .and_then(|observation| observation.quota)
            .map(OpaqueProviderData::into_inner)
            .unwrap_or_default();
        let quota = confirmed_exhaustion_projection(
            normalize_quota_window_placeholders(existing),
            reset_at,
        );
        let observed_at = SystemTime::now();
        let outcome = self
            .store
            .compare_and_swap_quota(QuotaObservation {
                account_id: account.id().clone(),
                expected_revision: account.revision(),
                quota: Some(OpaqueProviderData::new(quota.clone())),
                observed_at: Some(observed_at),
                limit_reached: Some(true),
            })
            .await?;
        if outcome == QuotaWriteOutcome::Conflict {
            return Err(CodexCredentialQuotaError::RevisionConflict);
        }
        if let Ok(snapshot) = parse_account_quota_snapshot(
            account.id().clone(),
            account.revision(),
            observed_at,
            &Value::Object(quota),
        ) {
            self.scheduling.observe(&snapshot);
        }
        Ok(())
    }

    /// 批量预热请求级额度投影；持久层或 Provider JSON 异常只退化为未知额度。
    pub async fn prepare_scheduling(&self, accounts: &[ProviderAccount]) {
        if self.scheduling.hydration_targets(accounts).is_empty() {
            return;
        }
        let _hydration = self.scheduling.hydration.lock().await;
        let pending = self.scheduling.hydration_targets(accounts);
        if pending.is_empty() {
            return;
        }
        let account_ids = pending
            .iter()
            .map(|target| target.account.id().clone())
            .collect::<Vec<_>>();
        let Ok(observations) = self.store.get_quotas(&account_ids).await else {
            for target in &pending {
                self.scheduling
                    .mark_unknown_if_unchanged(target, QUOTA_HYDRATION_FAILURE_TTL);
            }
            return;
        };
        let observations = observations
            .into_iter()
            .map(|observation| (observation.account_id.clone(), observation))
            .collect::<BTreeMap<_, _>>();
        for target in pending {
            let snapshot = observations
                .get(target.account.id())
                .filter(|observation| observation.expected_revision == target.account.revision())
                .and_then(quota_snapshot_from_observation);
            if !snapshot
                .is_some_and(|snapshot| self.scheduling.observe_if_unchanged(&target, &snapshot))
            {
                self.scheduling
                    .mark_unknown_if_unchanged(&target, QUOTA_SCHEDULING_TTL);
            }
        }
    }

    #[must_use]
    pub fn scheduling_signals(&self, account: &ProviderAccount) -> Option<AccountQuotaSignals> {
        self.scheduling.signals(account)
    }

    pub async fn synchronize(&self) -> Result<CodexQuotaSyncSummary, CodexCredentialQuotaError> {
        let accounts = self.repository.list_for_provider().await?;
        let mut summary = CodexQuotaSyncSummary::default();
        let now = SystemTime::now();
        let initial = self.initial_quota_sync_accounts(&accounts, now).await?;
        let periodic = self.scheduling.reserve_periodic_refreshes(accounts, now);
        let accounts = initial
            .into_iter()
            .chain(periodic)
            .fold(BTreeMap::new(), |mut unique, account| {
                unique.insert(account.id().clone(), account);
                unique
            })
            .into_values()
            .collect::<Vec<_>>();
        if accounts.is_empty() {
            return Ok(summary);
        }
        let client = CodexBackendClient::new(
            self.http.clone(),
            self.base_url.clone(),
            self.profile.clone(),
        );
        for account in accounts {
            let observed_at = SystemTime::now();
            match self.fetch_usage_with_recovery(&client, &account).await {
                Ok(FetchedCodexQuota { account, value }) => {
                    // 单账号解析或落库失败只影响该账号；其余账号继续同步。
                    if let Err(error) = self
                        .apply_fetched_quota(&account, &value, observed_at, &mut summary)
                        .await
                    {
                        summary.transient += 1;
                        tracing::warn!(
                            account_id = %account.id(),
                            error = %error,
                            "OpenAI quota synchronization skipped one account"
                        );
                    }
                }
                Err(CodexQuotaFetchError::InvalidCredential) => {
                    summary.stale += 1;
                }
                Err(CodexQuotaFetchError::Recovery) => {
                    summary.transient += 1;
                    tracing::warn!(
                        account_id = %account.id(),
                        "OpenAI quota credential recovery failed; refresh cycle will retry later"
                    );
                }
                Err(CodexQuotaFetchError::Upstream { account, error }) => {
                    match quota_state_transition(&error) {
                        // 402：真额度耗尽，写入 QuotaExhausted（worker 按 reset_at 自动恢复）。
                        Some(AccountAvailability::QuotaExhausted) => {
                            summary.exhausted += 1;
                            self.persist_upstream_state(
                                &account,
                                AccountAvailability::QuotaExhausted,
                                &error,
                                observed_at,
                            )
                            .await;
                        }
                        Some(AccountAvailability::Banned) => {
                            summary.banned += 1;
                            self.persist_upstream_state(
                                &account,
                                AccountAvailability::Banned,
                                &error,
                                observed_at,
                            )
                            .await;
                        }
                        // usage 401/403 与 429/503 都不写 availability；前者等待 OAuth 或真实请求确认。
                        None | Some(_) => {
                            summary.transient += 1;
                            tracing::warn!(
                                account_id = %account.id(),
                                error = %error,
                                "OpenAI quota upstream rejection; refresh cycle will retry later"
                            );
                        }
                    }
                }
            }
        }
        Ok(summary)
    }

    /// `quota_observed_at` 为空代表首次异步观察尚未成功；不另建同步状态表。
    async fn initial_quota_sync_accounts(
        &self,
        accounts: &[ProviderAccount],
        now: SystemTime,
    ) -> Result<Vec<ProviderAccount>, CodexCredentialQuotaError> {
        let account_ids = accounts
            .iter()
            .map(|account| account.id().clone())
            .collect::<Vec<_>>();
        let observed = self.store.get_quotas(&account_ids).await?;
        let observed_ids = observed
            .into_iter()
            .filter_map(|observation| observation.observed_at.map(|_| observation.account_id))
            .collect::<BTreeSet<_>>();
        Ok(accounts
            .iter()
            .filter(|account| {
                !observed_ids.contains(account.id()) && eligible_initial_quota_sync(account, now)
            })
            .take(INITIAL_QUOTA_SYNC_BATCH)
            .cloned()
            .collect())
    }

    /// 解析并 revision-fenced 落库单账号的 Provider quota JSON。
    async fn apply_fetched_quota(
        &self,
        account: &ProviderAccount,
        value: &Value,
        observed_at: SystemTime,
        summary: &mut CodexQuotaSyncSummary,
    ) -> Result<(), CodexCredentialQuotaError> {
        let refreshed_snapshot = parse_account_quota_snapshot(
            account.id().clone(),
            account.revision(),
            observed_at,
            value,
        )?;
        let previous_reset_at = self.previous_confirmed_reset(account).await;
        let object = normalize_quota_window_placeholders(
            value
                .as_object()
                .cloned()
                .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?,
        );
        let object = if account.availability() == AccountAvailability::QuotaExhausted
            && quota_success_state(
                account.availability(),
                &refreshed_snapshot,
                previous_reset_at,
            )
            .is_none()
        {
            confirmed_exhaustion_projection(
                object,
                confirmed_exhaustion_reset_at(previous_reset_at, &refreshed_snapshot),
            )
        } else {
            object
        };
        let snapshot = parse_account_quota_snapshot(
            account.id().clone(),
            account.revision(),
            observed_at,
            &Value::Object(object.clone()),
        )?;
        let fact = snapshot.fact();
        let outcome = self
            .store
            .compare_and_swap_quota(QuotaObservation {
                account_id: account.id().clone(),
                expected_revision: account.revision(),
                quota: Some(OpaqueProviderData::new(object)),
                observed_at: Some(observed_at),
                limit_reached: Some(fact.exhausted()),
            })
            .await?;
        if outcome == QuotaWriteOutcome::Conflict {
            summary.stale += 1;
            return Ok(());
        }
        let Some(current) = self.store.get_account(account.id()).await? else {
            summary.stale += 1;
            return Ok(());
        };
        if current.revision() != account.revision() {
            summary.stale += 1;
            return Ok(());
        }
        self.scheduling.observe(&snapshot);
        if fact.exhausted() {
            summary.exhausted += 1;
        } else {
            summary.updated += 1;
        }
        if let Some(availability) = quota_success_state(
            current.availability(),
            &refreshed_snapshot,
            previous_reset_at,
        ) {
            let _ = self
                .repository
                .apply_state(&current, availability, observed_at)
                .await;
        }
        Ok(())
    }

    /// 把正常推理响应携带的限流事实合并进 Provider 原始 quota JSON。
    pub async fn synchronize_passive_headers(
        &self,
        account: &ProviderAccount,
        headers: &[(String, String)],
    ) -> Result<bool, CodexCredentialQuotaError> {
        let Some(rate_limits) = parse_rate_limit_headers(headers) else {
            return Ok(false);
        };
        let has_quota_facts = rate_limits
            .limits
            .values()
            .any(|details| passive_rate_limit_snapshot(details).is_some());
        let previous_reset_at = if account.availability() == AccountAvailability::QuotaExhausted {
            self.previous_confirmed_reset(account).await
        } else {
            None
        };
        let existing = self
            .store
            .get_quotas(std::slice::from_ref(account.id()))
            .await?
            .into_iter()
            .find(|observation| {
                observation.account_id == *account.id()
                    && observation.expected_revision == account.revision()
            });
        let existing_observed_at = existing
            .as_ref()
            .and_then(|observation| observation.observed_at);
        let existing = existing
            .and_then(|observation| observation.quota)
            .map(OpaqueProviderData::into_inner)
            .unwrap_or_default();
        // 套餐、credits 等元数据可以更新，但没有额度窗口事实时必须保留旧观察时刻，
        // 也不能借旧快照重新推导 quota state。
        if !has_quota_facts {
            let Some(observed_at) = existing_observed_at else {
                return Ok(false);
            };
            let outcome = self
                .store
                .compare_and_swap_quota(QuotaObservation {
                    account_id: account.id().clone(),
                    expected_revision: account.revision(),
                    quota: Some(OpaqueProviderData::new(merge_passive_quota(
                        existing,
                        &rate_limits,
                    ))),
                    observed_at: Some(observed_at),
                    limit_reached: None,
                })
                .await?;
            return Ok(outcome != QuotaWriteOutcome::Conflict);
        }
        let quota = merge_passive_quota(existing, &rate_limits);
        let observed_at = SystemTime::now();
        let refreshed_snapshot = parse_account_quota_snapshot(
            account.id().clone(),
            account.revision(),
            observed_at,
            &Value::Object(quota.clone()),
        )?;
        let quota = if account.availability() == AccountAvailability::QuotaExhausted {
            // 被动成功响应不能解除确认额度耗尽；继续展示 100%，直到权威 usage 快照
            // 证明旧 reset 已跨过且新窗口已经推进。
            confirmed_exhaustion_projection(
                quota,
                confirmed_exhaustion_reset_at(previous_reset_at, &refreshed_snapshot),
            )
        } else {
            quota
        };
        let fact = parse_codex_quota_usage(&Value::Object(quota.clone()))?;
        let snapshot = parse_account_quota_snapshot(
            account.id().clone(),
            account.revision(),
            observed_at,
            &Value::Object(quota.clone()),
        );
        let outcome = self
            .store
            .compare_and_swap_quota(QuotaObservation {
                account_id: account.id().clone(),
                expected_revision: account.revision(),
                quota: Some(OpaqueProviderData::new(quota)),
                observed_at: Some(observed_at),
                limit_reached: Some(fact.exhausted()),
            })
            .await?;
        if outcome == QuotaWriteOutcome::Conflict {
            return Ok(false);
        }
        if let Ok(snapshot) = snapshot {
            self.scheduling.observe(&snapshot);
        }
        Ok(true)
    }

    /// 真实 429 的单一事实入口：写入 Redis 临时限流冷却（`until = now + retry_after`）。
    /// availability 与 quota 主窗口（额度重置时间）都不改变——临时限流是独立维度，
    /// 到期由 Redis key 过期自动解除，不污染配额耗尽状态。
    /// 已有更晚的冷却不会被缩短（put_if_later）。
    pub async fn apply_rate_limit_429(
        &self,
        account: &ProviderAccount,
        retry_after: Option<Duration>,
        observed_at: SystemTime,
    ) -> Result<(), CodexCredentialQuotaError> {
        let until = observed_at
            .checked_add(retry_after.unwrap_or(DEFAULT_RATE_LIMIT_COOLDOWN))
            .unwrap_or(observed_at);
        self.cooldowns
            .put_if_later(ProviderCooldown::new(
                account.id().clone(),
                account.revision(),
                until,
            ))
            .await
            .map_err(|error| CodexCredentialQuotaError::Store {
                detail: error.to_string(),
            })?;
        Ok(())
    }

    /// 读取账号当前是否处于临时限流（429）冷却，及到期时间。
    pub async fn rate_limited_until(
        &self,
        account_id: &ProviderAccountId,
        credential_revision: CredentialRevision,
    ) -> Result<Option<SystemTime>, CodexCredentialQuotaError> {
        let Some(cooldown) = self.cooldowns.read(account_id).await.map_err(|error| {
            CodexCredentialQuotaError::Store {
                detail: error.to_string(),
            }
        })?
        else {
            return Ok(None);
        };
        if cooldown.credential_revision() != credential_revision {
            return Ok(None);
        }
        let until = cooldown.until();
        if until <= SystemTime::now() {
            return Ok(None);
        }
        Ok(Some(until))
    }

    /// 读取单账号最后一次落库的 Provider quota，并由 Codex 域解析展示窗口。
    pub async fn read_account(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<Option<CodexAccountQuotaSnapshot>, CodexCredentialQuotaError> {
        let account = self
            .store
            .get_account(account_id)
            .await?
            .filter(|account| account.provider().as_str() == "openai")
            .ok_or(CodexCredentialQuotaError::NotFound)?;
        self.read_snapshot_for(&account).await
    }

    async fn read_snapshot_for(
        &self,
        account: &ProviderAccount,
    ) -> Result<Option<CodexAccountQuotaSnapshot>, CodexCredentialQuotaError> {
        if account.provider().as_str() != "openai" {
            return Err(CodexCredentialQuotaError::NotFound);
        }
        let account_id = account.id();
        let Some(observation) = self
            .store
            .get_quotas(std::slice::from_ref(account_id))
            .await?
            .into_iter()
            .next()
        else {
            return Ok(None);
        };
        if observation.account_id != *account_id
            || observation.expected_revision != account.revision()
        {
            return Err(CodexCredentialQuotaError::RevisionConflict);
        }
        let Some(data) = observation.quota else {
            return Ok(None);
        };
        let observed_at = observation
            .observed_at
            .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
        let snapshot = parse_account_quota_snapshot(
            account_id.clone(),
            account.revision(),
            observed_at,
            &Value::Object(data.expose_to_provider().clone()),
        )?;
        self.scheduling.observe(&snapshot);
        Ok(Some(snapshot))
    }

    /// 只刷新指定账号，revision-fenced 写入动态 Provider JSON 后返回解析快照。
    pub async fn refresh_account(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<CodexAccountQuotaSnapshot, CodexCredentialQuotaError> {
        self.refresh_account_snapshot(account_id).await
    }

    /// 真实限额失败后的异步刷新只补齐展示快照，不允许 usage 快照撤销已确认的失败状态。
    pub(crate) async fn refresh_account_after_failure(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<CodexAccountQuotaSnapshot, CodexCredentialQuotaError> {
        self.refresh_account_snapshot(account_id).await
    }

    async fn refresh_account_snapshot(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<CodexAccountQuotaSnapshot, CodexCredentialQuotaError> {
        let account = self
            .store
            .get_account(account_id)
            .await?
            .filter(|account| account.provider().as_str() == "openai")
            .ok_or(CodexCredentialQuotaError::NotFound)?;
        let observed_at = SystemTime::now();
        if !access_token_is_current(&account, observed_at) {
            return Err(CodexCredentialQuotaError::CredentialRefreshRequired);
        }
        let client = CodexBackendClient::new(
            self.http.clone(),
            self.base_url.clone(),
            self.profile.clone(),
        );
        let FetchedCodexQuota { account, value } =
            match self.fetch_usage_with_recovery(&client, &account).await {
                Ok(fetched) => fetched,
                Err(CodexQuotaFetchError::InvalidCredential) => {
                    return Err(CodexCredentialQuotaError::InvalidCredentialData);
                }
                Err(CodexQuotaFetchError::Recovery) => {
                    return Err(CodexCredentialQuotaError::Upstream {
                        detail: "credential recovery failed".to_owned(),
                    });
                }
                Err(CodexQuotaFetchError::Upstream { account, error }) => {
                    // 402 与明确的停用错误写对应状态；usage 401/403 不写 availability。
                    if let Some(availability) = quota_state_transition(&error) {
                        self.persist_upstream_state(&account, availability, &error, observed_at)
                            .await;
                    }
                    return Err(CodexCredentialQuotaError::Upstream {
                        detail: error.to_string(),
                    });
                }
            };
        let refreshed_snapshot = parse_account_quota_snapshot(
            account.id().clone(),
            account.revision(),
            observed_at,
            &value,
        )?;
        let previous_reset_at = self.previous_confirmed_reset(&account).await;
        let object = normalize_quota_window_placeholders(
            value
                .as_object()
                .cloned()
                .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?,
        );
        let object = if account.availability() == AccountAvailability::QuotaExhausted
            && quota_success_state(
                account.availability(),
                &refreshed_snapshot,
                previous_reset_at,
            )
            .is_none()
        {
            confirmed_exhaustion_projection(
                object,
                confirmed_exhaustion_reset_at(previous_reset_at, &refreshed_snapshot),
            )
        } else {
            object
        };
        let snapshot = parse_account_quota_snapshot(
            account.id().clone(),
            account.revision(),
            observed_at,
            &Value::Object(object.clone()),
        )?;
        if self
            .store
            .compare_and_swap_quota(QuotaObservation {
                account_id: account.id().clone(),
                expected_revision: account.revision(),
                quota: Some(OpaqueProviderData::new(object)),
                observed_at: Some(observed_at),
                limit_reached: Some(snapshot.fact().exhausted()),
            })
            .await?
            == QuotaWriteOutcome::Conflict
        {
            return Err(CodexCredentialQuotaError::RevisionConflict);
        }
        let current = self
            .store
            .get_account(account.id())
            .await?
            .ok_or(CodexCredentialQuotaError::NotFound)?;
        if current.revision() != account.revision() {
            return Err(CodexCredentialQuotaError::RevisionConflict);
        }
        self.scheduling.observe(&snapshot);
        if let Some(availability) = quota_success_state(
            current.availability(),
            &refreshed_snapshot,
            previous_reset_at,
        ) {
            self.repository
                .apply_state(&current, availability, observed_at)
                .await?;
        }
        Ok(snapshot)
    }

    async fn persist_upstream_state(
        &self,
        account: &ProviderAccount,
        availability: AccountAvailability,
        upstream_error: &CodexClientError,
        observed_at: SystemTime,
    ) {
        let reason = quota_state_reason(upstream_error).map(str::to_owned);
        if let Err(error) = self
            .repository
            .apply_state_with_message(account, availability, observed_at, reason.clone())
            .await
        {
            tracing::warn!(
                account_id = %account.id(),
                ?availability,
                reason = reason.as_deref().unwrap_or_default(),
                error = %error,
                "OpenAI quota upstream state write failed"
            );
        }
    }

    async fn fetch_usage_with_recovery(
        &self,
        client: &CodexBackendClient,
        account: &ProviderAccount,
    ) -> Result<FetchedCodexQuota, CodexQuotaFetchError> {
        let mut prepared = self
            .agent_identity
            .prepare(account)
            .await
            .map_err(|_| CodexQuotaFetchError::InvalidCredential)?;
        let mut result = fetch_usage_with_5xx_retry(client, &prepared).await;
        if let Err(CodexQuotaFetchAttemptError::Upstream(error)) = &result
            && let Some(recovered) = self
                .agent_identity
                .recover_after_rejected_task(
                    prepared.account.id(),
                    &prepared.credential.authentication,
                    error,
                )
                .await
                .map_err(|_| CodexQuotaFetchError::Recovery)?
        {
            prepared = recovered;
            result = fetch_usage_with_5xx_retry(client, &prepared).await;
        }
        match result {
            Ok(value) => Ok(FetchedCodexQuota {
                account: prepared.account,
                value,
            }),
            Err(CodexQuotaFetchAttemptError::InvalidCredential) => {
                Err(CodexQuotaFetchError::InvalidCredential)
            }
            Err(CodexQuotaFetchAttemptError::Upstream(error)) => {
                Err(CodexQuotaFetchError::Upstream {
                    account: Box::new(prepared.account),
                    error,
                })
            }
        }
    }
}

async fn fetch_usage_once(
    client: &CodexBackendClient,
    prepared: &PreparedCodexRuntimeCredential,
) -> Result<Value, CodexQuotaFetchAttemptError> {
    let authorization = prepared
        .credential
        .authentication
        .authorization_header(Utc::now())
        .map_err(|_| CodexQuotaFetchAttemptError::InvalidCredential)?;
    let request_id = format!("quota_{}", Uuid::now_v7().simple());
    client
        .fetch_usage(CodexRequestContext::auxiliary(
            authorization.expose_secret(),
            prepared.account.upstream_account_id(),
            &request_id,
            None,
        ))
        .await
        .map_err(CodexQuotaFetchAttemptError::Upstream)
}

/// 对 5xx 上游拒绝做有限次指数退避重试（1s/2s），吞掉瞬时抖动。
///
/// 4xx（含 402/429）不重试：它们已经走额度状态转换，重试只会放大上游负载。
async fn fetch_usage_with_5xx_retry(
    client: &CodexBackendClient,
    prepared: &PreparedCodexRuntimeCredential,
) -> Result<Value, CodexQuotaFetchAttemptError> {
    let mut attempt = 0_u32;
    loop {
        let result = fetch_usage_once(client, prepared).await;
        let retryable = match &result {
            Ok(_) => false,
            Err(CodexQuotaFetchAttemptError::Upstream(CodexClientError::Upstream {
                status,
                ..
            })) => status.is_server_error(),
            Err(_) => false,
        };
        if !retryable || attempt >= QUOTA_FETCH_5XX_MAX_RETRIES {
            return result;
        }
        attempt += 1;
        let delay = QUOTA_FETCH_5XX_BASE_DELAY.saturating_mul(attempt);
        tracing::warn!(
            account_id = %prepared.account.id(),
            retry_attempt = attempt,
            retry_delay_ms = delay.as_millis(),
            "OpenAI quota usage 5xx upstream rejection; retrying with backoff"
        );
        tokio::time::sleep(delay).await;
    }
}

fn eligible_periodic_quota_refresh(account: &ProviderAccount, now: SystemTime) -> bool {
    account.enabled() && access_token_is_current(account, now)
}

fn eligible_initial_quota_sync(account: &ProviderAccount, now: SystemTime) -> bool {
    // 首次观察只兜底刚入库、尚无 quota 快照的账号。已耗尽等运行时状态必须由
    // periodic 路径处理，才能保留同一账号的最小复核间隔。
    matches!(
        account.availability(),
        AccountAvailability::Ready | AccountAvailability::Unknown
    ) && eligible_periodic_quota_refresh(account, now)
}

fn access_token_is_current(account: &ProviderAccount, now: SystemTime) -> bool {
    account
        .access_token_expires_at()
        .is_none_or(|expires_at| expires_at > now)
}

fn merge_passive_quota(
    mut quota: Map<String, Value>,
    rate_limits: &ParsedRateLimits,
) -> Map<String, Value> {
    let active_limit = rate_limits
        .active_limit
        .as_deref()
        .or_else(|| rate_limits.limits.contains_key("codex").then_some("codex"));
    if let Some(active_limit) = active_limit {
        quota.insert(
            "active_limit".to_owned(),
            Value::String(active_limit.to_owned()),
        );
    }
    if let Some(plan_type) = rate_limits.plan_type.as_ref() {
        quota.insert("plan_type".to_owned(), Value::String(plan_type.clone()));
    }
    if let Some(promo_message) = rate_limits.promo_message.as_ref() {
        quota.insert(
            "promo_message".to_owned(),
            Value::String(promo_message.clone()),
        );
    }
    if let Some(reached_type) = rate_limits.rate_limit_reached_type.as_ref() {
        quota.insert(
            "rate_limit_reached_type".to_owned(),
            Value::String(reached_type.clone()),
        );
    }
    if let Some(credits) = rate_limits.credits.as_ref() {
        let mut value = quota
            .remove("credits")
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        value.insert("has_credits".to_owned(), Value::Bool(credits.has_credits));
        value.insert("unlimited".to_owned(), Value::Bool(credits.unlimited));
        if let Some(balance) = credits.balance.as_ref() {
            value.insert("balance".to_owned(), Value::String(balance.clone()));
        }
        quota.insert("credits".to_owned(), Value::Object(value));
    }

    let mut additional = quota
        .remove("additional_rate_limits")
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    for (limit_id, details) in &rate_limits.limits {
        let Some(rate_limit) = passive_rate_limit_snapshot(details) else {
            continue;
        };
        if Some(limit_id.as_str()) == active_limit {
            quota.insert("rate_limit".to_owned(), Value::Object(rate_limit));
            continue;
        }
        let index = additional.iter().position(|item| {
            item.get("metered_feature")
                .or_else(|| item.get("limit_name"))
                .and_then(Value::as_str)
                .is_some_and(|existing| existing == limit_id)
        });
        let mut item = index
            .and_then(|index| additional.get(index))
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        item.insert(
            "metered_feature".to_owned(),
            Value::String(limit_id.clone()),
        );
        if let Some(name) = details.limit_name.as_ref() {
            item.insert("limit_name".to_owned(), Value::String(name.clone()));
        }
        item.insert("rate_limit".to_owned(), Value::Object(rate_limit));
        match index {
            Some(index) => additional[index] = Value::Object(item),
            None => additional.push(Value::Object(item)),
        }
    }
    quota.insert(
        "additional_rate_limits".to_owned(),
        Value::Array(additional),
    );
    quota
}

/// 丢弃 core 限额中上游给出的无事实 `secondary_window` 占位。
///
/// 生产响应头可能携带 `used_percent=0`、零时长和空 reset 的占位，`/usage` 也会
/// 返回 `secondary_window: null`。正常被动同步保留该响应事实，Admin 展示会将其
/// 隐藏；但主动 `/usage` 刷新或 402 确认投影会把一个存在但无事实的字段写成
/// 100%，从而显示并不存在的“次级额度”。因此这些非被动写入口在落库前移除
/// 无事实值。带 reset、时长、正用量、触顶、未知或非法字段的次级窗口均完全按
/// 原有额度逻辑保留。
fn normalize_quota_window_placeholders(mut quota: Map<String, Value>) -> Map<String, Value> {
    if let Some(rate_limit) = quota.get_mut("rate_limit").and_then(Value::as_object_mut) {
        drop_secondary_window_placeholder(rate_limit);
    }
    quota
}

fn drop_secondary_window_placeholder(rate_limit: &mut Map<String, Value>) {
    let placeholder = rate_limit.get("secondary_window").is_some_and(|window| {
        window.is_null()
            || window
                .as_object()
                .is_some_and(secondary_window_is_placeholder)
    });
    if placeholder {
        rate_limit.remove("secondary_window");
    }
}

fn secondary_window_is_placeholder(window: &Map<String, Value>) -> bool {
    window.iter().all(|(field, value)| match field.as_str() {
        "used_percent" => value
            .as_f64()
            .is_some_and(|used_percent| used_percent.is_finite() && used_percent == 0.0),
        "limit_reached" => value.as_bool() == Some(false),
        _ => false,
    })
}

/// 用已确认的上游额度拒绝覆盖 core 限额投影。
///
/// 上游 usage 快照可能在结算期间短暂滞后为 98/99% 或 `allowed=true`；保留其他
/// 原始 metadata，但把 core 窗口固定为 100%，直到权威快照证明进入下一窗口。
fn confirmed_exhaustion_projection(
    mut quota: Map<String, Value>,
    reset_at: Option<i64>,
) -> Map<String, Value> {
    let mut rate_limit = quota
        .remove("rate_limit")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    rate_limit.insert("allowed".to_owned(), Value::Bool(false));
    rate_limit.insert("limit_reached".to_owned(), Value::Bool(true));
    for field in ["primary_window", "secondary_window"] {
        let existed = rate_limit.contains_key(field);
        let mut window = rate_limit
            .remove(field)
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        if field == "primary_window" || existed {
            window.insert("used_percent".to_owned(), Value::from(100_u64));
            if field == "primary_window"
                && let Some(reset_at) = reset_at
            {
                window.insert("reset_at".to_owned(), Value::from(reset_at));
            }
            rate_limit.insert(field.to_owned(), Value::Object(window));
        }
    }
    quota.insert("rate_limit".to_owned(), Value::Object(rate_limit));
    quota
}

/// 选择应继续锁定的 reset：新鲜快照仍确认耗尽时可把锁推进到更晚窗口；
/// 否则保留已确认失败的旧窗口，避免 98/99% 快照反向解锁。
fn confirmed_exhaustion_reset_at(
    previous_reset_at: Option<SystemTime>,
    refreshed_snapshot: &CodexAccountQuotaSnapshot,
) -> Option<i64> {
    let refreshed_reset_at = refreshed_snapshot
        .core_primary_window()
        .and_then(|window| window.reset_at())
        .map(SystemTime::from);
    let reset_at = match (previous_reset_at, refreshed_reset_at) {
        (Some(previous), Some(refreshed))
            if refreshed_snapshot.fact().exhausted() && refreshed > previous =>
        {
            refreshed
        }
        (Some(previous), _) => previous,
        (None, refreshed) => refreshed?,
    };
    system_time_to_unix_seconds(reset_at)
}

fn system_time_to_unix_seconds(value: SystemTime) -> Option<i64> {
    value
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs()
        .try_into()
        .ok()
}

fn passive_rate_limit_snapshot(details: &RateLimitDetails) -> Option<Map<String, Value>> {
    // 响应头的窗口属于同一次上游观测；跨响应合并会制造不存在的额度窗口。
    let mut snapshot = Map::new();
    if let Some(allowed) = details.allowed {
        snapshot.insert("allowed".to_owned(), Value::Bool(allowed));
    }
    if let Some(limit_reached) = details.limit_reached {
        snapshot.insert("limit_reached".to_owned(), Value::Bool(limit_reached));
    }
    for (field, window) in [
        ("primary_window", details.primary),
        ("secondary_window", details.secondary),
    ] {
        let Some(window) = window else {
            continue;
        };
        snapshot.insert(
            field.to_owned(),
            Value::Object(passive_rate_limit_window(window)),
        );
    }
    (!snapshot.is_empty()).then_some(snapshot)
}

fn passive_rate_limit_window(window: RateLimitWindow) -> Map<String, Value> {
    let mut snapshot = Map::new();
    if let Some(number) = serde_json::Number::from_f64(window.used_percent) {
        snapshot.insert("used_percent".to_owned(), Value::Number(number));
    }
    if let Some(seconds) = window
        .window_minutes
        .and_then(|minutes| minutes.checked_mul(60))
    {
        snapshot.insert("limit_window_seconds".to_owned(), Value::from(seconds));
    }
    if let Some(reset_at) = window.reset_at {
        snapshot.insert("reset_at".to_owned(), Value::from(reset_at));
    }
    snapshot
}
