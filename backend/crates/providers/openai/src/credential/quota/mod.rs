//! Codex quota 服务编排：权威额度事实、展示投影、主动/被动同步与 429 冷却。
//!
//! - [`document`]：上游 `/usage` 输入到多桶 map 的单向规范化。
//! - [`snapshot`]：快照/窗口解析、聚合、O(1) 滚动与调度信号。
//! - [`evidence`]：额度接口错误到凭据/额度事实的分类。

mod document;
pub(crate) mod evidence;
pub(crate) mod snapshot;
mod usage_statistics;

pub use snapshot::{
    CodexAccountQuotaSnapshot, CodexQuotaFact, CodexQuotaWindow, CodexQuotaWindowKind,
    CodexQuotaWindowRole, parse_codex_quota_usage,
};
pub use usage_statistics::{
    CodexUsageStatistics, CodexUsageStatisticsCycle, CodexUsageStatisticsDay,
    CodexUsageStatisticsMode, CodexUsageStatisticsModel, CodexUsageStatisticsServiceTier,
    CodexUsageStatisticsSummary, CodexUsageStatisticsTokens,
};

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, FixedOffset, Utc};
use gateway_core::engine::credential::{
    AccountErrorReason, AccountQuotaSignals, CredentialRevision, CredentialState,
    OpaqueProviderData, ProviderAccount, ProviderAccountId, ProviderAccountStore,
    QuotaAccessChange, QuotaAccessState, QuotaEvidence, QuotaObservation, QuotaObservationTouch,
    QuotaState, QuotaWriteOutcome,
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
use crate::transport::usage_statistics::CodexDailyUsageReport;
use crate::transport::{
    CodexBackendClient, CodexClientError, CodexRateLimitResetCredits,
    CodexRateLimitResetCreditsConsumeResult, CodexRequestContext,
};

use super::agent_identity::{CodexAgentIdentityTaskService, PreparedCodexRuntimeCredential};
use super::repository::{CodexCredentialRepository, CredentialRepositoryError};
use super::types::CODEX_AUTHENTICATION_KIND_OAUTH;
use document::{
    DEFAULT_CODEX_LIMIT_ID, RATE_LIMITS_BY_LIMIT_ID, RateLimitSnapshotsByLimitId,
    canonicalize_rate_limit_document,
};
use evidence::{QuotaEndpointFailure, classify_quota_endpoint_failure};
use snapshot::{
    parse_account_quota_snapshot, quota_projection_ttl, quota_snapshot_from_observation,
    scheduling_signals_from_snapshot,
};

const DEFAULT_RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(60);
pub(crate) const QUOTA_SCHEDULING_TTL: Duration = Duration::from_secs(10 * 60);
const QUOTA_HYDRATION_FAILURE_TTL: Duration = Duration::from_secs(5);
const EXHAUSTED_QUOTA_FALLBACK_RECHECK_INTERVAL: Duration = Duration::from_secs(30 * 60);
const EXHAUSTED_QUOTA_REFRESH_RETRY_INTERVAL: Duration = Duration::from_secs(30 * 60);
const RESET_RECOVERY_MAX_USED_PERCENT: f64 = 10.0;
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

/// 主动额度重置卡查询/消费失败。
#[derive(Error)]
pub enum CodexResetCreditsError {
    #[error("Codex reset-credit credential data is invalid")]
    InvalidCredentialData,
    #[error("Codex OAuth access token must be refreshed before using reset credits")]
    CredentialRefreshRequired { upstream_body: Option<String> },
    #[error("Codex reset-credit account was not found")]
    NotFound,
    #[error("provider account store is unavailable: {detail}")]
    Store { detail: String },
    #[error("Codex reset-credit upstream returned HTTP {status}")]
    Upstream {
        status: u16,
        body: String,
        retry_after_seconds: Option<u64>,
    },
    #[error("Codex reset-credit query transport is unavailable")]
    TransportUnavailable,
    #[error("Codex reset-credit consume result is unknown")]
    ConsumeResultUnknown,
}

/// 官方每日用量统计查询失败。
#[derive(Error)]
pub enum CodexUsageStatisticsError {
    #[error("Codex usage-statistics request is invalid")]
    InvalidRequest,
    #[error("Codex usage-statistics credential data is invalid")]
    InvalidCredentialData,
    #[error("Codex OAuth access token must be refreshed before querying usage statistics")]
    CredentialRefreshRequired { upstream_body: Option<String> },
    #[error("Codex usage-statistics account was not found")]
    NotFound,
    #[error("provider account store is unavailable: {detail}")]
    Store { detail: String },
    #[error("Codex usage-statistics quota window is unavailable")]
    QuotaWindowUnavailable,
    #[error("Codex usage-statistics upstream returned HTTP {status}")]
    Upstream {
        status: u16,
        body: String,
        retry_after_seconds: Option<u64>,
    },
    #[error("Codex usage-statistics query transport is unavailable")]
    TransportUnavailable,
}

impl std::fmt::Debug for CodexUsageStatisticsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRequest => formatter.write_str("InvalidRequest"),
            Self::InvalidCredentialData => formatter.write_str("InvalidCredentialData"),
            Self::CredentialRefreshRequired { .. } => {
                formatter.write_str("CredentialRefreshRequired { upstream_body: <redacted> }")
            }
            Self::NotFound => formatter.write_str("NotFound"),
            Self::Store { .. } => formatter.write_str("Store { detail: <redacted> }"),
            Self::QuotaWindowUnavailable => formatter.write_str("QuotaWindowUnavailable"),
            Self::Upstream {
                status,
                retry_after_seconds,
                ..
            } => formatter
                .debug_struct("Upstream")
                .field("status", status)
                .field("body", &"<redacted>")
                .field("retry_after_seconds", retry_after_seconds)
                .finish(),
            Self::TransportUnavailable => formatter.write_str("TransportUnavailable"),
        }
    }
}

impl std::fmt::Debug for CodexResetCreditsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidCredentialData => formatter.write_str("InvalidCredentialData"),
            Self::CredentialRefreshRequired { .. } => {
                formatter.write_str("CredentialRefreshRequired { upstream_body: <redacted> }")
            }
            Self::NotFound => formatter.write_str("NotFound"),
            Self::Store { .. } => formatter.write_str("Store { detail: <redacted> }"),
            Self::Upstream {
                status,
                retry_after_seconds,
                ..
            } => formatter
                .debug_struct("Upstream")
                .field("status", status)
                .field("body", &"<redacted>")
                .field("retry_after_seconds", retry_after_seconds)
                .finish(),
            Self::TransportUnavailable => formatter.write_str("TransportUnavailable"),
            Self::ConsumeResultUnknown => formatter.write_str("ConsumeResultUnknown"),
        }
    }
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
    reset_consume_locks: Mutex<HashMap<ProviderAccountId, Arc<Mutex<()>>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuotaRefreshAuthority {
    ObserveAccess,
    PreserveAccess,
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

enum ResetCreditAttemptError {
    InvalidCredential,
    Upstream(CodexClientError),
}

enum DailyUsageAttemptError {
    InvalidCredential,
    Upstream(CodexClientError),
}

enum DailyUsageFetchError {
    InvalidCredential,
    Recovery,
    Upstream {
        prepared: Box<PreparedCodexRuntimeCredential>,
        error: CodexClientError,
    },
}

struct FetchedDailyUsage {
    mode: CodexUsageStatisticsMode,
    model_breakdown: Value,
    daily_totals: Option<Value>,
}

impl CodexQuotaSchedulingProjection {
    fn invalidate(&self, account_ids: &[ProviderAccountId]) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for account_id in account_ids {
            state.entries.remove(account_id);
            state.last_periodic_refresh_at.remove(account_id);
        }
    }

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
            .filter_map(|account| quota_refresh_candidate(account, now))
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

        // 正常账号只由真实请求的响应头和 `codex.rate_limits` 被动同步。定时器只复核
        // 已耗尽且已到 reset/fallback 时刻的账号；重复复核至少间隔 30 分钟。
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

fn quota_refresh_candidate(account: ProviderAccount, now: SystemTime) -> Option<ProviderAccount> {
    // reset 到期是“应刷新”的信号，不是“已恢复”的证据。
    (eligible_periodic_quota_refresh(&account, now)
        && account
            .quota()
            .exhaustion_refresh_due(now, EXHAUSTED_QUOTA_FALLBACK_RECHECK_INTERVAL))
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
            now.saturating_duration_since(*last) >= EXHAUSTED_QUOTA_REFRESH_RETRY_INTERVAL
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
            reset_consume_locks: Mutex::new(HashMap::new()),
        }
    }

    /// 查询当前账号由 Codex Desktop 暴露的主动额度重置卡。
    pub async fn list_reset_credits(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<CodexRateLimitResetCredits, CodexResetCreditsError> {
        let account = self.reset_credit_account(account_id).await?;
        let client = CodexBackendClient::new(
            self.http.clone(),
            self.base_url.clone(),
            self.profile.clone(),
        );
        let mut prepared = self
            .agent_identity
            .prepare(&account)
            .await
            .map_err(|_| CodexResetCreditsError::InvalidCredentialData)?;
        let request_id = format!("reset_credits_{}", Uuid::now_v7().simple());
        let mut result = list_reset_credits_once(&client, &prepared, &request_id).await;
        if let Err(ResetCreditAttemptError::Upstream(error)) = &result
            && let Some(recovered) = self
                .agent_identity
                .recover_after_rejected_task(
                    prepared.account.id(),
                    &prepared.credential.authentication,
                    error,
                )
                .await
                .map_err(|_| CodexResetCreditsError::InvalidCredentialData)?
        {
            prepared = recovered;
            result = list_reset_credits_once(&client, &prepared, &request_id).await;
        }
        result.map_err(|error| map_reset_credit_attempt_error(error, &prepared, false))
    }

    /// 查询当前账号的官方每日用量报表，并把报表范围锚定到最接近七天的额度窗口。
    pub async fn usage_statistics(
        &self,
        account_id: &ProviderAccountId,
        cycle_offset: i8,
        utc_offset_minutes: i16,
    ) -> Result<CodexUsageStatistics, CodexUsageStatisticsError> {
        const MAX_CYCLE_OFFSET: u8 = 8;
        const MAX_UTC_OFFSET_MINUTES: i16 = 14 * 60;
        const TARGET_WINDOW_SECONDS: u64 = 7 * 24 * 60 * 60;

        if !(-(MAX_CYCLE_OFFSET as i8)..=0).contains(&cycle_offset)
            || !(-MAX_UTC_OFFSET_MINUTES..=MAX_UTC_OFFSET_MINUTES).contains(&utc_offset_minutes)
        {
            return Err(CodexUsageStatisticsError::InvalidRequest);
        }
        let account = self
            .store
            .get_account(account_id)
            .await
            .map_err(|error| CodexUsageStatisticsError::Store {
                detail: error.to_string(),
            })?
            .filter(|account| account.provider().as_str() == "openai")
            .ok_or(CodexUsageStatisticsError::NotFound)?;
        let observed_at = SystemTime::now();
        if !access_token_is_current(&account, observed_at) {
            return Err(CodexUsageStatisticsError::CredentialRefreshRequired {
                upstream_body: None,
            });
        }
        let client = CodexBackendClient::new(
            self.http.clone(),
            self.base_url.clone(),
            self.profile.clone(),
        );
        let FetchedCodexQuota { account, value } = self
            .fetch_usage_with_recovery(&client, &account)
            .await
            .map_err(map_statistics_quota_fetch_error)?;
        let snapshot = parse_account_quota_snapshot(
            account.id().clone(),
            account.revision(),
            observed_at,
            &value,
        )
        .map_err(|_| CodexUsageStatisticsError::InvalidCredentialData)?;
        let window = snapshot
            .windows()
            .iter()
            .filter(|window| window.is_account_wide())
            .filter_map(|window| {
                Some((
                    window,
                    window.window_seconds()?.abs_diff(TARGET_WINDOW_SECONDS),
                ))
            })
            .min_by_key(|(_, distance)| *distance)
            .map(|(window, _)| window)
            .ok_or(CodexUsageStatisticsError::QuotaWindowUnavailable)?;
        let window_seconds = window
            .window_seconds()
            .filter(|seconds| *seconds > 0)
            .ok_or(CodexUsageStatisticsError::QuotaWindowUnavailable)?;
        let end_at = window
            .reset_at()
            .ok_or(CodexUsageStatisticsError::QuotaWindowUnavailable)?;
        let window_delta = chrono::TimeDelta::seconds(
            i64::try_from(window_seconds)
                .map_err(|_| CodexUsageStatisticsError::QuotaWindowUnavailable)?,
        );
        let current_start_at = end_at - window_delta;
        let timezone = FixedOffset::east_opt(i32::from(utc_offset_minutes) * 60)
            .ok_or(CodexUsageStatisticsError::InvalidRequest)?;
        let now = DateTime::<Utc>::from(observed_at);
        let period = usage_statistics::statistics_period(
            current_start_at,
            end_at,
            window_seconds,
            cycle_offset,
            timezone,
            now,
        )
        .ok_or(CodexUsageStatisticsError::InvalidRequest)?;
        let fetched = self
            .fetch_daily_usage_with_recovery(
                &client,
                &account,
                period.query_start_date,
                period.query_end_date,
            )
            .await
            .map_err(map_daily_usage_fetch_error)?;

        Ok(usage_statistics::build_usage_statistics(
            usage_statistics::BuildUsageStatistics {
                mode: fetched.mode,
                period,
                timezone,
                current_used_percent: window.used_percent(),
                now,
                model_breakdown: &fetched.model_breakdown,
                daily_totals: fetched.daily_totals.as_ref(),
                max_cycle_offset: MAX_CYCLE_OFFSET,
            },
        ))
    }

    /// 消费一张主动额度重置卡。相同账号在本进程内串行，且不做传输重试。
    pub async fn consume_reset_credit(
        &self,
        account_id: &ProviderAccountId,
        credit_id: Option<&str>,
        redeem_request_id: Uuid,
    ) -> Result<CodexRateLimitResetCreditsConsumeResult, CodexResetCreditsError> {
        let lock = {
            let mut locks = self.reset_consume_locks.lock().await;
            Arc::clone(
                locks
                    .entry(account_id.clone())
                    .or_insert_with(|| Arc::new(Mutex::new(()))),
            )
        };
        let _guard = lock.lock().await;
        let account = self.reset_credit_account(account_id).await?;
        let client = CodexBackendClient::new(
            self.http.clone(),
            self.base_url.clone(),
            self.profile.clone(),
        );
        let mut prepared = self
            .agent_identity
            .prepare(&account)
            .await
            .map_err(|_| CodexResetCreditsError::InvalidCredentialData)?;
        let request_id = format!("reset_credit_consume_{}", Uuid::now_v7().simple());
        let mut result = consume_reset_credit_once(
            &client,
            &prepared,
            &request_id,
            credit_id,
            redeem_request_id,
        )
        .await;
        // Agent Identity task recovery is the only automatic retry below. It keeps the exact
        // official credit selection and redeem_request_id; generic HTTP/transport failures are
        // never retried because the debit result may already be committed upstream.
        if let Err(ResetCreditAttemptError::Upstream(error)) = &result
            && let Some(recovered) = self
                .agent_identity
                .recover_after_rejected_task(
                    prepared.account.id(),
                    &prepared.credential.authentication,
                    error,
                )
                .await
                .map_err(|_| CodexResetCreditsError::InvalidCredentialData)?
        {
            prepared = recovered;
            result = consume_reset_credit_once(
                &client,
                &prepared,
                &request_id,
                credit_id,
                redeem_request_id,
            )
            .await;
        }
        result.map_err(|error| map_reset_credit_attempt_error(error, &prepared, true))
    }

    async fn reset_credit_account(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<ProviderAccount, CodexResetCreditsError> {
        let account = self
            .store
            .get_account(account_id)
            .await
            .map_err(|error| CodexResetCreditsError::Store {
                detail: error.to_string(),
            })?
            .filter(|account| account.provider().as_str() == "openai")
            .ok_or(CodexResetCreditsError::NotFound)?;
        if !access_token_is_current(&account, SystemTime::now()) {
            return Err(CodexResetCreditsError::CredentialRefreshRequired {
                upstream_body: None,
            });
        }
        Ok(account)
    }

    /// 真实推理错误只更新额度访问事实，不伪造 Provider JSON 或展示百分比。
    pub(crate) async fn record_confirmed_exhaustion(
        &self,
        account: &ProviderAccount,
        evidence: QuotaEvidence,
        reset_at: Option<SystemTime>,
        observed_at: SystemTime,
    ) -> Result<(), CodexCredentialQuotaError> {
        let outcome = self
            .store
            .apply_quota_access(QuotaAccessChange {
                account_id: account.id().clone(),
                expected_revision: account.revision(),
                state: QuotaState::exhausted(evidence, observed_at, reset_at),
            })
            .await?;
        if outcome == QuotaWriteOutcome::Conflict {
            return Err(CodexCredentialQuotaError::RevisionConflict);
        }
        Ok(())
    }

    /// 成功推理是额度可访问的权威证据，同时解除账号级 429 冷却。
    pub(crate) async fn record_successful_inference(
        &self,
        account: &ProviderAccount,
        observed_at: SystemTime,
    ) -> Result<(), CodexCredentialQuotaError> {
        // 已经发出的并发请求可能晚于耗尽事实成功返回；真实成功仍是独立的
        // Allowed 权威证据，但调度不会为了试探恢复而放行耗尽账号。
        if account.quota().access() == QuotaAccessState::Exhausted {
            let outcome = self
                .store
                .apply_quota_access(QuotaAccessChange {
                    account_id: account.id().clone(),
                    expected_revision: account.revision(),
                    state: QuotaState::allowed(observed_at),
                })
                .await?;
            if outcome == QuotaWriteOutcome::Conflict {
                return Err(CodexCredentialQuotaError::RevisionConflict);
            }
        }
        self.cooldowns
            .clear(account.id(), account.revision())
            .await
            .map_err(|error| CodexCredentialQuotaError::Store {
                detail: error.to_string(),
            })?;
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

    pub(crate) fn invalidate_scheduling(&self, account_ids: &[ProviderAccountId]) {
        self.scheduling.invalidate(account_ids);
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
                    match classify_quota_endpoint_failure(&error) {
                        Some(QuotaEndpointFailure::Exhausted(evidence)) => {
                            summary.exhausted += 1;
                            if let Err(write_error) = self
                                .record_confirmed_exhaustion(&account, evidence, None, observed_at)
                                .await
                            {
                                summary.transient += 1;
                                tracing::warn!(
                                    account_id = %account.id(),
                                    error = %write_error,
                                    "OpenAI quota exhaustion fact write failed"
                                );
                            }
                        }
                        Some(QuotaEndpointFailure::Credential { state, reason }) => {
                            summary.banned += 1;
                            self.persist_credential_failure(&account, state, reason, observed_at)
                                .await;
                        }
                        None => {
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
            .map(|observation| observation.account_id)
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
        let object = normalize_quota_window_placeholders(
            value
                .as_object()
                .cloned()
                .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?,
        );
        let snapshot = parse_account_quota_snapshot(
            account.id().clone(),
            account.revision(),
            observed_at,
            &Value::Object(object.clone()),
        )?;
        let previous = if account.quota().is_exhausted() {
            self.read_snapshot_for(account).await?
        } else {
            None
        };
        if account.quota().is_exhausted()
            && !quota_refresh_allows_recovery(account, &snapshot, previous.as_ref())
        {
            let outcome = self
                .store
                .touch_quota_observation(QuotaObservationTouch {
                    account_id: account.id().clone(),
                    expected_revision: account.revision(),
                    observed_at,
                })
                .await?;
            if outcome == QuotaWriteOutcome::Conflict {
                summary.stale += 1;
                return Ok(());
            }
            if let Some(previous) = previous {
                self.scheduling
                    .observe(&previous.with_observed_at(observed_at));
            }
            summary.exhausted += 1;
            return Ok(());
        }
        let state = if account.quota().is_exhausted() {
            QuotaState::allowed(observed_at)
        } else {
            account.quota().merge_observation(snapshot.quota())
        };
        let snapshot = snapshot.with_quota_state(state);
        let outcome = self
            .store
            .compare_and_swap_quota(QuotaObservation {
                account_id: account.id().clone(),
                expected_revision: account.revision(),
                quota: OpaqueProviderData::new(object),
                observed_at,
                state,
            })
            .await?;
        if outcome == QuotaWriteOutcome::Conflict {
            summary.stale += 1;
            return Ok(());
        }
        self.scheduling.observe(&snapshot);
        if snapshot.quota().is_exhausted() {
            summary.exhausted += 1;
        } else {
            summary.updated += 1;
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
        self.synchronize_passive_rate_limits(account, std::slice::from_ref(&rate_limits))
            .await
    }

    /// 把一次推理响应中采集的结构化限流观察合并后单次落库。
    pub async fn synchronize_passive_rate_limits(
        &self,
        account: &ProviderAccount,
        rate_limits: &[ParsedRateLimits],
    ) -> Result<bool, CodexCredentialQuotaError> {
        if rate_limits.is_empty() {
            return Ok(false);
        }
        let has_quota_facts = rate_limits.iter().any(|observation| {
            observation
                .limits
                .values()
                .any(|details| passive_rate_limit_snapshot(details).is_some())
        });
        let existing = self
            .store
            .get_quotas(std::slice::from_ref(account.id()))
            .await?
            .into_iter()
            .find(|observation| {
                observation.account_id == *account.id()
                    && observation.expected_revision == account.revision()
            });
        let existing_state = existing.as_ref().map(|observation| observation.state);
        let existing = existing
            .map(|observation| observation.quota.into_inner())
            .unwrap_or_default();
        // 套餐、credits 等元数据可以更新，但没有额度窗口事实时必须保留旧观察时刻，
        // 也不能借旧快照重新推导 quota state。
        if !has_quota_facts {
            let Some(state) = existing_state else {
                return Ok(false);
            };
            let outcome = self
                .store
                .compare_and_swap_quota(QuotaObservation {
                    account_id: account.id().clone(),
                    expected_revision: account.revision(),
                    quota: OpaqueProviderData::new(merge_passive_quota(existing, rate_limits)),
                    observed_at: SystemTime::now(),
                    state,
                })
                .await?;
            return Ok(outcome != QuotaWriteOutcome::Conflict);
        }
        let quota = merge_passive_quota(existing, rate_limits);
        let observed_at = SystemTime::now();
        let snapshot = parse_account_quota_snapshot(
            account.id().clone(),
            account.revision(),
            observed_at,
            &Value::Object(quota.clone()),
        )?;
        // 这些 headers 来自一次成功推理，访问结论优先于可能滞后的百分比。
        let state = QuotaState::allowed(observed_at);
        let outcome = self
            .store
            .compare_and_swap_quota(QuotaObservation {
                account_id: account.id().clone(),
                expected_revision: account.revision(),
                quota: OpaqueProviderData::new(quota),
                observed_at,
                state,
            })
            .await?;
        if outcome == QuotaWriteOutcome::Conflict {
            return Ok(false);
        }
        self.scheduling.observe(&snapshot);
        Ok(true)
    }

    /// 真实 429 的单一事实入口：写入 Redis 临时限流冷却（`until = now + retry_after`）。
    /// 凭据与额度主窗口（额度重置时间）都不改变——临时限流是独立维度，
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
    ) -> Result<Option<SystemTime>, CodexCredentialQuotaError> {
        let Some(cooldown) = self.cooldowns.read(account_id).await.map_err(|error| {
            CodexCredentialQuotaError::Store {
                detail: error.to_string(),
            }
        })?
        else {
            return Ok(None);
        };
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
        if observation.quota.expose_to_provider().is_empty() {
            return Ok(None);
        }
        let observed_at = observation.observed_at;
        let snapshot = parse_account_quota_snapshot(
            account_id.clone(),
            account.revision(),
            observed_at,
            &Value::Object(observation.quota.expose_to_provider().clone()),
        )?
        .with_quota_state(observation.state);
        self.scheduling.observe(&snapshot);
        Ok(Some(snapshot))
    }

    /// 只刷新指定账号，revision-fenced 写入动态 Provider JSON 后返回解析快照。
    pub async fn refresh_account(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<CodexAccountQuotaSnapshot, CodexCredentialQuotaError> {
        self.refresh_account_snapshot(account_id, QuotaRefreshAuthority::ObserveAccess)
            .await
    }

    /// 真实限额失败后的异步刷新只补齐展示快照，不允许 usage 快照撤销已确认的失败状态。
    pub(crate) async fn refresh_account_after_failure(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<CodexAccountQuotaSnapshot, CodexCredentialQuotaError> {
        self.refresh_account_snapshot(account_id, QuotaRefreshAuthority::PreserveAccess)
            .await
    }

    async fn refresh_account_snapshot(
        &self,
        account_id: &ProviderAccountId,
        authority: QuotaRefreshAuthority,
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
                    match classify_quota_endpoint_failure(&error) {
                        Some(QuotaEndpointFailure::Exhausted(evidence)) => {
                            self.record_confirmed_exhaustion(&account, evidence, None, observed_at)
                                .await?;
                        }
                        Some(QuotaEndpointFailure::Credential { state, reason }) => {
                            self.persist_credential_failure(&account, state, reason, observed_at)
                                .await;
                        }
                        None => {}
                    }
                    return Err(CodexCredentialQuotaError::Upstream {
                        detail: error.to_string(),
                    });
                }
            };
        let object = normalize_quota_window_placeholders(
            value
                .as_object()
                .cloned()
                .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?,
        );
        let snapshot = parse_account_quota_snapshot(
            account.id().clone(),
            account.revision(),
            observed_at,
            &Value::Object(object.clone()),
        )?;
        let previous = if account.quota().is_exhausted() {
            self.read_snapshot_for(&account).await?
        } else {
            None
        };
        if !quota_refresh_allows_recovery(&account, &snapshot, previous.as_ref()) {
            if self
                .store
                .touch_quota_observation(QuotaObservationTouch {
                    account_id: account.id().clone(),
                    expected_revision: account.revision(),
                    observed_at,
                })
                .await?
                == QuotaWriteOutcome::Conflict
            {
                return Err(CodexCredentialQuotaError::RevisionConflict);
            }
            return Ok(match previous {
                Some(previous) => {
                    let previous = previous.with_observed_at(observed_at);
                    self.scheduling.observe(&previous);
                    previous
                }
                None => snapshot
                    .with_quota_state(account.quota())
                    .with_observed_at(observed_at),
            });
        }
        let state = if account.quota().is_exhausted() {
            QuotaState::allowed(observed_at)
        } else {
            match authority {
                QuotaRefreshAuthority::ObserveAccess => {
                    account.quota().merge_observation(snapshot.quota())
                }
                QuotaRefreshAuthority::PreserveAccess => account.quota(),
            }
        };
        let snapshot = snapshot.with_quota_state(state);
        if self
            .store
            .compare_and_swap_quota(QuotaObservation {
                account_id: account.id().clone(),
                expected_revision: account.revision(),
                quota: OpaqueProviderData::new(object),
                observed_at,
                state,
            })
            .await?
            == QuotaWriteOutcome::Conflict
        {
            return Err(CodexCredentialQuotaError::RevisionConflict);
        }
        self.scheduling.observe(&snapshot);
        Ok(snapshot)
    }

    async fn persist_credential_failure(
        &self,
        account: &ProviderAccount,
        credential_state: CredentialState,
        reason: AccountErrorReason,
        observed_at: SystemTime,
    ) {
        if let Err(error) = self
            .repository
            .apply_state_with_reason(account, credential_state, observed_at, Some(reason), None)
            .await
        {
            tracing::warn!(
                account_id = %account.id(),
                ?credential_state,
                reason = reason.as_str(),
                error = %error,
                "OpenAI quota credential fact write failed"
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

    async fn fetch_daily_usage_with_recovery(
        &self,
        client: &CodexBackendClient,
        account: &ProviderAccount,
        start_date: chrono::NaiveDate,
        end_date: chrono::NaiveDate,
    ) -> Result<FetchedDailyUsage, DailyUsageFetchError> {
        let mut prepared = self
            .agent_identity
            .prepare(account)
            .await
            .map_err(|_| DailyUsageFetchError::InvalidCredential)?;
        let mut result = fetch_daily_usage_once(client, &prepared, start_date, end_date).await;
        if let Err(DailyUsageAttemptError::Upstream(error)) = &result
            && let Some(recovered) = self
                .agent_identity
                .recover_after_rejected_task(
                    prepared.account.id(),
                    &prepared.credential.authentication,
                    error,
                )
                .await
                .map_err(|_| DailyUsageFetchError::Recovery)?
        {
            prepared = recovered;
            result = fetch_daily_usage_once(client, &prepared, start_date, end_date).await;
        }
        match result {
            Ok(value) => Ok(value),
            Err(DailyUsageAttemptError::InvalidCredential) => {
                Err(DailyUsageFetchError::InvalidCredential)
            }
            Err(DailyUsageAttemptError::Upstream(error)) => Err(DailyUsageFetchError::Upstream {
                prepared: Box::new(prepared),
                error,
            }),
        }
    }
}

async fn fetch_daily_usage_once(
    client: &CodexBackendClient,
    prepared: &PreparedCodexRuntimeCredential,
    start_date: chrono::NaiveDate,
    end_date: chrono::NaiveDate,
) -> Result<FetchedDailyUsage, DailyUsageAttemptError> {
    let authorization = prepared
        .credential
        .authentication
        .authorization_header(Utc::now())
        .map_err(|_| DailyUsageAttemptError::InvalidCredential)?;
    let workspace_request_id = format!("usage_statistics_{}", Uuid::now_v7().simple());
    let workspace = client
        .fetch_daily_usage(
            CodexRequestContext::auxiliary(
                authorization.expose_secret(),
                prepared.account.upstream_account_id(),
                &workspace_request_id,
                None,
            ),
            CodexDailyUsageReport::WorkspaceModelTokens,
            start_date,
            end_date,
        )
        .await;
    match workspace {
        Ok(model_breakdown) => {
            return Ok(FetchedDailyUsage {
                mode: CodexUsageStatisticsMode::Workspace,
                model_breakdown,
                daily_totals: None,
            });
        }
        Err(error) if workspace_is_unavailable_for_account(&error) => {}
        Err(error) => return Err(DailyUsageAttemptError::Upstream(error)),
    }

    let model_request_id = format!("usage_statistics_{}", Uuid::now_v7().simple());
    let totals_request_id = format!("usage_statistics_{}", Uuid::now_v7().simple());
    let model_breakdown = client.fetch_daily_usage(
        CodexRequestContext::auxiliary(
            authorization.expose_secret(),
            prepared.account.upstream_account_id(),
            &model_request_id,
            None,
        ),
        CodexDailyUsageReport::PersonalModelCredits,
        start_date,
        end_date,
    );
    let daily_totals = client.fetch_daily_usage(
        CodexRequestContext::auxiliary(
            authorization.expose_secret(),
            prepared.account.upstream_account_id(),
            &totals_request_id,
            None,
        ),
        CodexDailyUsageReport::PersonalTokenTotals,
        start_date,
        end_date,
    );
    let (model_breakdown, daily_totals) = tokio::try_join!(model_breakdown, daily_totals)
        .map_err(DailyUsageAttemptError::Upstream)?;
    Ok(FetchedDailyUsage {
        mode: CodexUsageStatisticsMode::Personal,
        model_breakdown,
        daily_totals: Some(daily_totals),
    })
}

fn workspace_is_unavailable_for_account(error: &CodexClientError) -> bool {
    matches!(
        error,
        CodexClientError::Upstream { status, body, .. }
            if *status == reqwest::StatusCode::BAD_REQUEST
                && body.to_ascii_lowercase().contains("no active workspace")
    )
}

fn map_statistics_quota_fetch_error(error: CodexQuotaFetchError) -> CodexUsageStatisticsError {
    match error {
        CodexQuotaFetchError::InvalidCredential => CodexUsageStatisticsError::InvalidCredentialData,
        CodexQuotaFetchError::Recovery => CodexUsageStatisticsError::TransportUnavailable,
        CodexQuotaFetchError::Upstream { account, error } => map_statistics_client_error(
            error,
            account.authentication_kind() == CODEX_AUTHENTICATION_KIND_OAUTH,
        ),
    }
}

fn map_daily_usage_fetch_error(error: DailyUsageFetchError) -> CodexUsageStatisticsError {
    match error {
        DailyUsageFetchError::InvalidCredential => CodexUsageStatisticsError::InvalidCredentialData,
        DailyUsageFetchError::Recovery => CodexUsageStatisticsError::TransportUnavailable,
        DailyUsageFetchError::Upstream { prepared, error } => {
            map_statistics_client_error(error, prepared.credential.authentication.oauth().is_some())
        }
    }
}

fn map_statistics_client_error(
    error: CodexClientError,
    credential_can_refresh: bool,
) -> CodexUsageStatisticsError {
    match error {
        CodexClientError::Upstream { status, body, .. }
            if status == reqwest::StatusCode::UNAUTHORIZED && credential_can_refresh =>
        {
            CodexUsageStatisticsError::CredentialRefreshRequired {
                upstream_body: Some(body),
            }
        }
        CodexClientError::Upstream {
            status,
            body,
            retry_after_seconds,
            ..
        } => CodexUsageStatisticsError::Upstream {
            status: status.as_u16(),
            body,
            retry_after_seconds,
        },
        _ => CodexUsageStatisticsError::TransportUnavailable,
    }
}

async fn list_reset_credits_once(
    client: &CodexBackendClient,
    prepared: &PreparedCodexRuntimeCredential,
    request_id: &str,
) -> Result<CodexRateLimitResetCredits, ResetCreditAttemptError> {
    let authorization = prepared
        .credential
        .authentication
        .authorization_header(Utc::now())
        .map_err(|_| ResetCreditAttemptError::InvalidCredential)?;
    client
        .list_rate_limit_reset_credits(CodexRequestContext::auxiliary(
            authorization.expose_secret(),
            prepared.account.upstream_account_id(),
            request_id,
            None,
        ))
        .await
        .map_err(ResetCreditAttemptError::Upstream)
}

async fn consume_reset_credit_once(
    client: &CodexBackendClient,
    prepared: &PreparedCodexRuntimeCredential,
    request_id: &str,
    credit_id: Option<&str>,
    redeem_request_id: Uuid,
) -> Result<CodexRateLimitResetCreditsConsumeResult, ResetCreditAttemptError> {
    let authorization = prepared
        .credential
        .authentication
        .authorization_header(Utc::now())
        .map_err(|_| ResetCreditAttemptError::InvalidCredential)?;
    client
        .consume_rate_limit_reset_credit(
            CodexRequestContext::auxiliary(
                authorization.expose_secret(),
                prepared.account.upstream_account_id(),
                request_id,
                None,
            ),
            credit_id,
            redeem_request_id,
        )
        .await
        .map_err(ResetCreditAttemptError::Upstream)
}

fn map_reset_credit_attempt_error(
    error: ResetCreditAttemptError,
    prepared: &PreparedCodexRuntimeCredential,
    consume: bool,
) -> CodexResetCreditsError {
    match error {
        ResetCreditAttemptError::InvalidCredential => CodexResetCreditsError::InvalidCredentialData,
        ResetCreditAttemptError::Upstream(error) => {
            map_reset_credit_client_error(error, prepared, consume)
        }
    }
}

fn map_reset_credit_client_error(
    error: CodexClientError,
    prepared: &PreparedCodexRuntimeCredential,
    consume: bool,
) -> CodexResetCreditsError {
    match error {
        CodexClientError::Upstream {
            status,
            body,
            diagnostics,
            ..
        } if status == reqwest::StatusCode::UNAUTHORIZED
            && reset_credit_response_was_explicit_rejection(status, &diagnostics)
            && prepared.credential.authentication.oauth().is_some() =>
        {
            CodexResetCreditsError::CredentialRefreshRequired {
                upstream_body: Some(body),
            }
        }
        CodexClientError::Upstream {
            status,
            body,
            retry_after_seconds,
            diagnostics,
            ..
        } => {
            // 2xx 后发生的解码/响应体上限错误使用 synthetic 502 表示，但额度卡
            // 可能已经消费。只有 transport 记录的真实非成功状态与错误状态一致时，
            // 才能把它当作确定的上游拒绝并允许前端清除 pending 幂等键。
            if consume && !reset_credit_response_was_explicit_rejection(status, &diagnostics) {
                CodexResetCreditsError::ConsumeResultUnknown
            } else {
                CodexResetCreditsError::Upstream {
                    status: status.as_u16(),
                    body,
                    retry_after_seconds,
                }
            }
        }
        _ if consume => CodexResetCreditsError::ConsumeResultUnknown,
        _ => CodexResetCreditsError::TransportUnavailable,
    }
}

fn reset_credit_response_was_explicit_rejection(
    status: reqwest::StatusCode,
    diagnostics: &crate::transport::CodexUpstreamDiagnostics,
) -> bool {
    !status.is_success() && diagnostics.status_code == Some(status.as_u16())
}

fn quota_refresh_allows_recovery(
    account: &ProviderAccount,
    refreshed: &CodexAccountQuotaSnapshot,
    previous: Option<&CodexAccountQuotaSnapshot>,
) -> bool {
    if !account.quota().is_exhausted() {
        return true;
    }
    if refreshed.quota().is_exhausted() {
        return false;
    }
    let previous_reset_at = account.quota().reset_at().or_else(|| {
        previous?
            .windows()
            .iter()
            .find(|window| {
                window.source() == "codex" && window.role() == CodexQuotaWindowRole::Primary
            })?
            .reset_at()
            .map(SystemTime::from)
    });
    let Some(previous_reset_at) = previous_reset_at else {
        return false;
    };
    let Some(primary) = refreshed.windows().iter().find(|window| {
        window.source() == "codex" && window.role() == CodexQuotaWindowRole::Primary
    }) else {
        return false;
    };
    let Some(refreshed_reset_at) = primary.reset_at().map(SystemTime::from) else {
        return false;
    };
    let Some(used_percent) = primary.used_percent() else {
        return false;
    };
    refreshed_reset_at > previous_reset_at && used_percent < RESET_RECOVERY_MAX_USED_PERCENT
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
    account.credential_state() == CredentialState::Ready
        && eligible_periodic_quota_refresh(account, now)
}

fn access_token_is_current(account: &ProviderAccount, now: SystemTime) -> bool {
    account
        .access_token_expires_at()
        .is_none_or(|expires_at| expires_at > now)
}

fn merge_passive_quota(
    mut quota: Map<String, Value>,
    observations: &[ParsedRateLimits],
) -> Map<String, Value> {
    let mut snapshots = RateLimitSnapshotsByLimitId::take_from_document(&mut quota);
    for rate_limits in observations {
        let default_is_named_alias = default_limit_is_named_alias(rate_limits);
        let mut resolved_limit_ids = BTreeMap::new();
        for (wire_limit_id, details) in &rate_limits.limits {
            // HTTP 与 WebSocket 都可能把活动具名桶镜像为默认 `codex` 窗口。
            // 同一 wire 观察里存在相同具名事实时丢弃镜像，不触碰 core 桶。
            if wire_limit_id == DEFAULT_CODEX_LIMIT_ID && default_is_named_alias {
                continue;
            }
            let Some(limit_id) = snapshots.resolve_limit_id(details) else {
                continue;
            };
            let Some(rate_limit) = passive_rate_limit_snapshot(details) else {
                continue;
            };
            snapshots.upsert(&limit_id, details, rate_limit);
            resolved_limit_ids.insert(wire_limit_id.as_str(), limit_id);
        }

        let active_limit = rate_limits
            .active_limit
            .as_deref()
            .and_then(|wire_limit_id| {
                resolved_limit_ids.get(wire_limit_id).cloned().or_else(|| {
                    (!rate_limits.limits.contains_key(wire_limit_id))
                        .then(|| wire_limit_id.to_owned())
                })
            })
            .or_else(|| resolved_limit_ids.get(DEFAULT_CODEX_LIMIT_ID).cloned());
        if let Some(active_limit) = active_limit {
            quota.insert("active_limit".to_owned(), Value::String(active_limit));
        }
        merge_passive_metadata(&mut quota, rate_limits);
    }
    snapshots.write_to_document(&mut quota);
    quota
}

fn merge_passive_metadata(quota: &mut Map<String, Value>, rate_limits: &ParsedRateLimits) {
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
}

fn default_limit_is_named_alias(rate_limits: &ParsedRateLimits) -> bool {
    let Some(default_primary) = rate_limits
        .limits
        .get(DEFAULT_CODEX_LIMIT_ID)
        .and_then(|details| details.primary)
    else {
        return false;
    };
    rate_limits.limits.iter().any(|(limit_id, details)| {
        limit_id != DEFAULT_CODEX_LIMIT_ID && details.primary == Some(default_primary)
    })
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
    quota = canonicalize_rate_limit_document(quota);
    if let Some(rate_limit) = quota
        .get_mut(RATE_LIMITS_BY_LIMIT_ID)
        .and_then(Value::as_object_mut)
        .and_then(|limits| limits.get_mut(DEFAULT_CODEX_LIMIT_ID))
        .and_then(Value::as_object_mut)
    {
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
