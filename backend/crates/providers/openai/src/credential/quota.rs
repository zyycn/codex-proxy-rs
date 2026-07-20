//! Codex quota 原始观察、Provider-owned 解析与账号状态投影。

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Utc};
use gateway_core::engine::credential::{
    AccountAvailability, AccountQuotaSignals, CredentialRevision, OpaqueProviderData,
    ProviderAccount, ProviderAccountId, ProviderAccountStore, QuotaObservation, QuotaWriteOutcome,
};
use gateway_core::routing::ProviderInstance;
use gateway_protocol::openai::events::{
    ParsedRateLimits, RateLimitDetails, RateLimitWindow, parse_rate_limit_headers,
};
use reqwest::{Client, StatusCode};
use secrecy::ExposeSecret;
use serde_json::{Map, Value};
use thiserror::Error;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::provider::{CodexOriginPolicy, CodexProviderInstanceConfig};
use crate::transport::profile::CodexWireProfileState;
use crate::transport::{CodexBackendClient, CodexClientError, CodexRequestContext};

use super::repository::{CodexCredentialRepository, CredentialRepositoryError};

const DEFAULT_RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(60);
const MAX_RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(60 * 60);
const QUOTA_SCHEDULING_TTL: Duration = Duration::from_secs(10 * 60);
const QUOTA_HYDRATION_FAILURE_TTL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodexQuotaFact {
    remaining_percent: Option<u8>,
    resets_at: Option<DateTime<Utc>>,
    exhausted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexQuotaWindowKind {
    ShortTerm,
    Weekly,
    Monthly,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexQuotaWindowRole {
    Primary,
    Secondary,
    Monthly,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodexQuotaWindow {
    key: String,
    source: String,
    kind: CodexQuotaWindowKind,
    role: CodexQuotaWindowRole,
    window_seconds: Option<u64>,
    used_percent: Option<f64>,
    reset_at: Option<DateTime<Utc>>,
}

impl CodexQuotaWindow {
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    #[must_use]
    pub const fn kind(&self) -> CodexQuotaWindowKind {
        self.kind
    }

    #[must_use]
    pub const fn role(&self) -> CodexQuotaWindowRole {
        self.role
    }

    #[must_use]
    pub const fn window_seconds(&self) -> Option<u64> {
        self.window_seconds
    }

    #[must_use]
    pub const fn used_percent(&self) -> Option<f64> {
        self.used_percent
    }

    #[must_use]
    pub const fn reset_at(&self) -> Option<DateTime<Utc>> {
        self.reset_at
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodexAccountQuotaSnapshot {
    account_id: ProviderAccountId,
    credential_revision: CredentialRevision,
    observed_at: SystemTime,
    fact: CodexQuotaFact,
    windows: Vec<CodexQuotaWindow>,
}

impl CodexAccountQuotaSnapshot {
    #[must_use]
    pub const fn account_id(&self) -> &ProviderAccountId {
        &self.account_id
    }

    #[must_use]
    pub const fn credential_revision(&self) -> CredentialRevision {
        self.credential_revision
    }

    #[must_use]
    pub const fn observed_at(&self) -> SystemTime {
        self.observed_at
    }

    #[must_use]
    pub const fn fact(&self) -> CodexQuotaFact {
        self.fact
    }

    #[must_use]
    pub fn windows(&self) -> &[CodexQuotaWindow] {
        &self.windows
    }
}

impl CodexQuotaFact {
    #[must_use]
    pub const fn remaining_percent(&self) -> Option<u8> {
        self.remaining_percent
    }

    #[must_use]
    pub const fn resets_at(&self) -> Option<DateTime<Utc>> {
        self.resets_at
    }

    #[must_use]
    pub const fn exhausted(&self) -> bool {
        self.exhausted
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CodexQuotaSyncSummary {
    pub updated: u64,
    pub exhausted: u64,
    pub invalid: u64,
    pub cooldown: u64,
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
    #[error("Codex quota instance is invalid")]
    InvalidInstance,
    #[error("Codex quota response is invalid")]
    InvalidCredentialData,
    #[error(transparent)]
    Repository(#[from] CredentialRepositoryError),
    #[error("provider account store is unavailable")]
    Store,
    #[error("Codex quota account was not found")]
    NotFound,
    #[error("Codex quota credential revision is stale")]
    RevisionConflict,
    #[error("Codex quota upstream query failed")]
    Upstream,
}

impl From<gateway_core::error::StoreError> for CodexCredentialQuotaError {
    fn from(_: gateway_core::error::StoreError) -> Self {
        Self::Store
    }
}

pub struct CodexCredentialQuotaService {
    repository: CodexCredentialRepository,
    store: Arc<dyn ProviderAccountStore>,
    profile: CodexWireProfileState,
    http: Client,
    origin_policy: Arc<dyn CodexOriginPolicy>,
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

    fn observe(
        &self,
        account_id: ProviderAccountId,
        revision: CredentialRevision,
        observed_at: SystemTime,
        fact: CodexQuotaFact,
    ) -> bool {
        let Some(remaining_ttl) = quota_projection_ttl(observed_at) else {
            return false;
        };
        self.replace(
            account_id,
            revision,
            remaining_ttl,
            quota_scheduling_signals(fact),
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
        let Some(remaining_ttl) = quota_projection_ttl(snapshot.observed_at()) else {
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
            quota_scheduling_signals(snapshot.fact()),
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
        origin_policy: Arc<dyn CodexOriginPolicy>,
    ) -> Self {
        Self {
            store: Arc::clone(repository.store()),
            repository,
            profile,
            http,
            origin_policy,
            scheduling: CodexQuotaSchedulingProjection::default(),
        }
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

    pub async fn synchronize_instance(
        &self,
        instance: &gateway_core::routing::ProviderInstance,
    ) -> Result<CodexQuotaSyncSummary, CodexCredentialQuotaError> {
        let config =
            CodexProviderInstanceConfig::from_snapshot(instance, self.origin_policy.as_ref())
                .map_err(|_| CodexCredentialQuotaError::InvalidInstance)?;
        let client = CodexBackendClient::new(
            self.http.clone(),
            config.base_url().as_str(),
            self.profile.clone(),
        );
        let accounts = self.repository.list_for_instance(config.id()).await?;
        let mut summary = CodexQuotaSyncSummary::default();
        let now = SystemTime::now();
        for account in accounts
            .into_iter()
            .filter(|account| eligible_quota_sync_account(account, now))
        {
            let runtime = match self.repository.load_runtime_credential(&account).await {
                Ok(runtime) => runtime,
                Err(_) => {
                    summary.stale += 1;
                    continue;
                }
            };
            let request_id = format!("quota_{}", Uuid::now_v7().simple());
            let context = CodexRequestContext {
                access_token: runtime.secret.access_token.expose_secret(),
                account_id: account.upstream_account_id(),
                request_id: &request_id,
                turn_state: None,
                turn_metadata: None,
                beta_features: None,
                include_timing_metrics: None,
                version: None,
                codex_window_id: None,
                parent_thread_id: None,
                cookie_header: None,
                installation_id: None,
                session_id: None,
                thread_id: None,
                client_request_id: None,
                turn_id: None,
            };
            let observed_at = SystemTime::now();
            match client.fetch_usage(context).await {
                Ok(value) => {
                    let fact = parse_codex_quota_usage(&value)?;
                    let object = value
                        .as_object()
                        .cloned()
                        .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
                    let outcome = self
                        .store
                        .compare_and_swap_quota(QuotaObservation {
                            account_id: account.id().clone(),
                            expected_revision: account.revision(),
                            quota: Some(OpaqueProviderData::new(object)),
                            observed_at: Some(observed_at),
                        })
                        .await?;
                    if outcome == QuotaWriteOutcome::Conflict {
                        summary.stale += 1;
                        continue;
                    }
                    let Some(current) = self.store.get_account(account.id()).await? else {
                        summary.stale += 1;
                        continue;
                    };
                    if current.revision() != account.revision() {
                        summary.stale += 1;
                        continue;
                    }
                    self.scheduling.observe(
                        current.id().clone(),
                        current.revision(),
                        observed_at,
                        fact,
                    );
                    if fact.exhausted() {
                        summary.exhausted += 1;
                    } else {
                        summary.updated += 1;
                    }
                    if let Some(availability) = quota_success_availability(
                        current.availability(),
                        current.cooldown_until(),
                        fact.exhausted(),
                        SystemTime::now(),
                    ) {
                        let _ = self
                            .repository
                            .apply_state(
                                &current,
                                availability,
                                fact.exhausted().then_some("quota_exhausted".to_owned()),
                                None,
                                observed_at,
                            )
                            .await;
                    }
                }
                Err(error) => {
                    let (availability, reason, cooldown) = classify_error(&error, observed_at);
                    match availability {
                        Some(AccountAvailability::Invalid | AccountAvailability::Banned) => {
                            summary.invalid += 1;
                        }
                        Some(AccountAvailability::QuotaExhausted) => summary.exhausted += 1,
                        Some(AccountAvailability::Cooldown) => summary.cooldown += 1,
                        _ => {
                            summary.transient += 1;
                            continue;
                        }
                    }
                    let _ = self
                        .repository
                        .apply_state(
                            &account,
                            availability.expect("classified availability"),
                            reason.map(str::to_owned),
                            cooldown,
                            observed_at,
                        )
                        .await;
                }
            }
        }
        Ok(summary)
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
        let quota = merge_passive_quota(existing, &rate_limits);
        let fact = parse_codex_quota_usage(&Value::Object(quota.clone()))?;
        let observed_at = SystemTime::now();
        let outcome = self
            .store
            .compare_and_swap_quota(QuotaObservation {
                account_id: account.id().clone(),
                expected_revision: account.revision(),
                quota: Some(OpaqueProviderData::new(quota)),
                observed_at: Some(observed_at),
            })
            .await?;
        if outcome == QuotaWriteOutcome::Conflict {
            return Ok(false);
        }
        let Some(current) = self.store.get_account(account.id()).await? else {
            return Ok(false);
        };
        if current.revision() != account.revision() {
            return Ok(false);
        }
        self.scheduling
            .observe(current.id().clone(), current.revision(), observed_at, fact);
        if let Some(availability) = quota_success_availability(
            current.availability(),
            current.cooldown_until(),
            fact.exhausted(),
            observed_at,
        ) {
            self.repository
                .apply_state(
                    &current,
                    availability,
                    fact.exhausted().then_some("quota_exhausted".to_owned()),
                    None,
                    observed_at,
                )
                .await?;
        }
        Ok(true)
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
        self.scheduling.observe(
            snapshot.account_id().clone(),
            snapshot.credential_revision(),
            snapshot.observed_at(),
            snapshot.fact(),
        );
        Ok(Some(snapshot))
    }

    /// 只刷新指定账号，revision-fenced 写入动态 Provider JSON 后返回解析快照。
    pub async fn refresh_account(
        &self,
        instance: &ProviderInstance,
        account_id: &ProviderAccountId,
    ) -> Result<CodexAccountQuotaSnapshot, CodexCredentialQuotaError> {
        let config =
            CodexProviderInstanceConfig::from_snapshot(instance, self.origin_policy.as_ref())
                .map_err(|_| CodexCredentialQuotaError::InvalidInstance)?;
        let account = self
            .store
            .get_account(account_id)
            .await?
            .filter(|account| {
                account.provider().as_str() == "openai" && account.instance() == config.id()
            })
            .ok_or(CodexCredentialQuotaError::NotFound)?;
        let runtime = self.repository.load_runtime_credential(&account).await?;
        let client = CodexBackendClient::new(
            self.http.clone(),
            config.base_url().as_str(),
            self.profile.clone(),
        );
        let request_id = format!("quota_{}", Uuid::now_v7().simple());
        let context = CodexRequestContext {
            access_token: runtime.secret.access_token.expose_secret(),
            account_id: account.upstream_account_id(),
            request_id: &request_id,
            turn_state: None,
            turn_metadata: None,
            beta_features: None,
            include_timing_metrics: None,
            version: None,
            codex_window_id: None,
            parent_thread_id: None,
            cookie_header: None,
            installation_id: None,
            session_id: None,
            thread_id: None,
            client_request_id: None,
            turn_id: None,
        };
        let observed_at = SystemTime::now();
        let value = match client.fetch_usage(context).await {
            Ok(value) => value,
            Err(error) => {
                let (availability, reason, cooldown) = classify_error(&error, observed_at);
                if let Some(availability) = availability {
                    let _ = self
                        .repository
                        .apply_state(
                            &account,
                            availability,
                            reason.map(str::to_owned),
                            cooldown,
                            observed_at,
                        )
                        .await;
                }
                return Err(CodexCredentialQuotaError::Upstream);
            }
        };
        let snapshot = parse_account_quota_snapshot(
            account.id().clone(),
            account.revision(),
            observed_at,
            &value,
        )?;
        let object = value
            .as_object()
            .cloned()
            .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
        if self
            .store
            .compare_and_swap_quota(QuotaObservation {
                account_id: account.id().clone(),
                expected_revision: account.revision(),
                quota: Some(OpaqueProviderData::new(object)),
                observed_at: Some(observed_at),
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
        self.scheduling.observe(
            snapshot.account_id().clone(),
            snapshot.credential_revision(),
            snapshot.observed_at(),
            snapshot.fact(),
        );
        if let Some(availability) = quota_success_availability(
            current.availability(),
            current.cooldown_until(),
            snapshot.fact().exhausted(),
            SystemTime::now(),
        ) {
            self.repository
                .apply_state(
                    &current,
                    availability,
                    snapshot
                        .fact()
                        .exhausted()
                        .then_some("quota_exhausted".to_owned()),
                    None,
                    observed_at,
                )
                .await?;
        }
        Ok(snapshot)
    }
}

fn eligible_quota_sync_account(account: &ProviderAccount, now: SystemTime) -> bool {
    account.enabled()
        && account.access_token_expires_at() > now
        && match account.availability() {
            AccountAvailability::Unknown
            | AccountAvailability::Ready
            | AccountAvailability::Cooldown
            | AccountAvailability::QuotaExhausted => true,
            AccountAvailability::Expired
            | AccountAvailability::Banned
            | AccountAvailability::Invalid => false,
        }
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
        if Some(limit_id.as_str()) == active_limit {
            let mut value = quota
                .remove("rate_limit")
                .and_then(|value| value.as_object().cloned())
                .unwrap_or_default();
            merge_rate_limit_details(&mut value, details);
            quota.insert("rate_limit".to_owned(), Value::Object(value));
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
        let mut value = item
            .remove("rate_limit")
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        merge_rate_limit_details(&mut value, details);
        item.insert("rate_limit".to_owned(), Value::Object(value));
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

fn merge_rate_limit_details(target: &mut Map<String, Value>, details: &RateLimitDetails) {
    if let Some(allowed) = details.allowed {
        target.insert("allowed".to_owned(), Value::Bool(allowed));
    }
    if let Some(limit_reached) = details.limit_reached {
        target.insert("limit_reached".to_owned(), Value::Bool(limit_reached));
    }
    for (field, window) in [
        ("primary_window", details.primary),
        ("secondary_window", details.secondary),
    ] {
        let Some(window) = window else {
            continue;
        };
        let mut value = target
            .remove(field)
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        merge_rate_limit_window(&mut value, window);
        target.insert(field.to_owned(), Value::Object(value));
    }
}

fn merge_rate_limit_window(target: &mut Map<String, Value>, window: RateLimitWindow) {
    if let Some(number) = serde_json::Number::from_f64(window.used_percent) {
        target.insert("used_percent".to_owned(), Value::Number(number));
    }
    if let Some(seconds) = window
        .window_minutes
        .and_then(|minutes| minutes.checked_mul(60))
    {
        target.insert("limit_window_seconds".to_owned(), Value::from(seconds));
    }
    if let Some(reset_at) = window.reset_at {
        target.insert("reset_at".to_owned(), Value::from(reset_at));
    }
}

fn quota_success_availability(
    current: AccountAvailability,
    cooldown_until: Option<SystemTime>,
    exhausted: bool,
    now: SystemTime,
) -> Option<AccountAvailability> {
    match current {
        AccountAvailability::Invalid
        | AccountAvailability::Expired
        | AccountAvailability::Banned => None,
        AccountAvailability::QuotaExhausted => (!exhausted).then_some(AccountAvailability::Ready),
        AccountAvailability::Ready => exhausted.then_some(AccountAvailability::QuotaExhausted),
        AccountAvailability::Cooldown if exhausted => Some(AccountAvailability::QuotaExhausted),
        AccountAvailability::Cooldown
            if cooldown_until.is_some_and(|cooldown_until| cooldown_until <= now) =>
        {
            Some(AccountAvailability::Ready)
        }
        AccountAvailability::Cooldown => None,
        AccountAvailability::Unknown => Some(if exhausted {
            AccountAvailability::QuotaExhausted
        } else {
            AccountAvailability::Ready
        }),
    }
}

fn classify_error(
    error: &CodexClientError,
    observed_at: SystemTime,
) -> (
    Option<AccountAvailability>,
    Option<&'static str>,
    Option<SystemTime>,
) {
    let CodexClientError::Upstream {
        status,
        retry_after_seconds,
        body,
        ..
    } = error
    else {
        return (None, None, None);
    };
    if *status == StatusCode::PAYMENT_REQUIRED && is_deactivated_workspace(body) {
        return (
            Some(AccountAvailability::Banned),
            Some("quota_deactivated_workspace"),
            None,
        );
    }
    match *status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => (
            Some(AccountAvailability::Invalid),
            Some("quota_auth_rejected"),
            None,
        ),
        StatusCode::PAYMENT_REQUIRED => (
            Some(AccountAvailability::QuotaExhausted),
            Some("quota_exhausted"),
            None,
        ),
        StatusCode::TOO_MANY_REQUESTS => {
            let duration = Duration::from_secs(
                retry_after_seconds.unwrap_or(DEFAULT_RATE_LIMIT_COOLDOWN.as_secs()),
            )
            .min(MAX_RATE_LIMIT_COOLDOWN);
            (
                Some(AccountAvailability::Cooldown),
                Some("quota_rate_limited"),
                observed_at.checked_add(duration),
            )
        }
        _ => (None, None, None),
    }
}

fn is_deactivated_workspace(body: &str) -> bool {
    serde_json::from_str::<Value>(body)
        .ok()
        .is_some_and(|value| {
            value.pointer("/detail/code").and_then(Value::as_str) == Some("deactivated_workspace")
        })
}

/// 解析用于 Admin 展示/调度信号的 Codex 已知 quota；原始 JSON 原样落 Provider quota 字段。
pub fn parse_codex_quota_usage(usage: &Value) -> Result<CodexQuotaFact, CodexCredentialQuotaError> {
    let object = usage
        .as_object()
        .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
    let mut aggregate = QuotaAggregate::default();
    if let Some(rate_limit) = object.get("rate_limit") {
        aggregate.observe_rate_limit(rate_limit)?;
    }
    if let Some(additional) = object
        .get("additional_rate_limits")
        .filter(|value| !value.is_null())
    {
        for item in additional
            .as_array()
            .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?
        {
            if let Some(rate_limit) = item
                .as_object()
                .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?
                .get("rate_limit")
            {
                aggregate.observe_rate_limit(rate_limit)?;
            }
        }
    }
    if let Some(spend_control) = object.get("spend_control") {
        aggregate.observe_exhaustion_object(spend_control, "reached")?;
        if let Some(individual) = spend_control
            .as_object()
            .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?
            .get("individual_limit")
        {
            aggregate.observe_window(individual)?;
        }
    }
    if let Some(monthly_limit) = object.get("monthly_limit") {
        aggregate.observe_exhaustion_object(monthly_limit, "limit_reached")?;
        aggregate.observe_window(monthly_limit)?;
    }
    if let Some(credits) = object.get("credits") {
        aggregate.observe_exhaustion_object(credits, "overage_limit_reached")?;
    }
    if !aggregate.recognized {
        return Err(CodexCredentialQuotaError::InvalidCredentialData);
    }
    Ok(CodexQuotaFact {
        remaining_percent: aggregate.remaining_percent,
        resets_at: aggregate.resets_at,
        exhausted: aggregate.exhausted,
    })
}

fn quota_snapshot_from_observation(
    observation: &QuotaObservation,
) -> Option<CodexAccountQuotaSnapshot> {
    let observed_at = observation.observed_at?;
    let quota = observation.quota.as_ref()?;
    parse_account_quota_snapshot(
        observation.account_id.clone(),
        observation.expected_revision,
        observed_at,
        &Value::Object(quota.expose_to_provider().clone()),
    )
    .ok()
}

fn quota_projection_ttl(observed_at: SystemTime) -> Option<Duration> {
    let age = SystemTime::now()
        .duration_since(observed_at)
        .unwrap_or(Duration::ZERO);
    QUOTA_SCHEDULING_TTL
        .checked_sub(age)
        .filter(|remaining| !remaining.is_zero())
}

fn quota_scheduling_signals(fact: CodexQuotaFact) -> Option<AccountQuotaSignals> {
    let remaining_rank = fact.remaining_percent().map(|value| u64::from(value) * 100);
    let now = SystemTime::now();
    let reset_at = fact
        .resets_at()
        .map(SystemTime::from)
        .filter(|reset_at| *reset_at > now);
    (remaining_rank.is_some() || reset_at.is_some())
        .then(|| AccountQuotaSignals::new(reset_at, remaining_rank))
}

fn parse_account_quota_snapshot(
    account_id: ProviderAccountId,
    credential_revision: CredentialRevision,
    observed_at: SystemTime,
    usage: &Value,
) -> Result<CodexAccountQuotaSnapshot, CodexCredentialQuotaError> {
    let fact = parse_codex_quota_usage(usage)?;
    let object = usage
        .as_object()
        .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
    let mut windows = Vec::new();
    if let Some(rate_limit) = object.get("rate_limit") {
        parse_rate_limit_windows("core", rate_limit, &mut windows)?;
    }
    if let Some(additional) = object
        .get("additional_rate_limits")
        .filter(|value| !value.is_null())
    {
        for (index, value) in additional
            .as_array()
            .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?
            .iter()
            .enumerate()
        {
            let item = value
                .as_object()
                .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
            let source = item
                .get("limit_name")
                .or_else(|| item.get("metered_feature"))
                .and_then(Value::as_str)
                .filter(|value| {
                    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
                })
                .map(str::to_owned)
                .unwrap_or_else(|| format!("additional-{index}"));
            if let Some(rate_limit) = item.get("rate_limit") {
                parse_rate_limit_windows(&source, rate_limit, &mut windows)?;
            }
        }
    }
    if let Some(spend_control) = object.get("spend_control").filter(|value| !value.is_null()) {
        parse_spend_control_window(spend_control, &mut windows)?;
    }
    if let Some(monthly_limit) = object.get("monthly_limit").filter(|value| !value.is_null()) {
        parse_monthly_limit_window(monthly_limit, &mut windows)?;
    }
    windows.sort_by(|left, right| left.key.cmp(&right.key));
    Ok(CodexAccountQuotaSnapshot {
        account_id,
        credential_revision,
        observed_at,
        fact,
        windows,
    })
}

fn parse_rate_limit_windows(
    source: &str,
    value: &Value,
    output: &mut Vec<CodexQuotaWindow>,
) -> Result<(), CodexCredentialQuotaError> {
    let object = value
        .as_object()
        .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
    for (name, role) in [
        ("primary_window", CodexQuotaWindowRole::Primary),
        ("secondary_window", CodexQuotaWindowRole::Secondary),
    ] {
        let Some(window) = object.get(name) else {
            continue;
        };
        if window.is_null() {
            continue;
        }
        let window = window
            .as_object()
            .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
        let window_seconds = optional_positive_u64(window, "limit_window_seconds")?
            .or(optional_positive_u64(window, "window_seconds")?)
            .or(optional_positive_u64(window, "window_minutes")?
                .and_then(|minutes| minutes.checked_mul(60)));
        let used_percent = window
            .get("used_percent")
            .map(|value| {
                value
                    .as_f64()
                    .filter(|value| value.is_finite() && *value >= 0.0)
                    .map(|value| value.clamp(0.0, 100.0))
                    .ok_or(CodexCredentialQuotaError::InvalidCredentialData)
            })
            .transpose()?;
        let reset_at = window
            .get("reset_at")
            .map(|value| {
                value
                    .as_i64()
                    .filter(|value| *value > 0)
                    .and_then(|value| DateTime::<Utc>::from_timestamp(value, 0))
                    .ok_or(CodexCredentialQuotaError::InvalidCredentialData)
            })
            .transpose()?;
        let kind = quota_window_kind(window_seconds);
        output.push(CodexQuotaWindow {
            key: format!("{}-{}", quota_key(source), quota_role_name(role, kind)),
            source: source.to_owned(),
            kind,
            role,
            window_seconds,
            used_percent,
            reset_at,
        });
    }
    Ok(())
}

fn parse_spend_control_window(
    value: &Value,
    output: &mut Vec<CodexQuotaWindow>,
) -> Result<(), CodexCredentialQuotaError> {
    let object = value
        .as_object()
        .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
    let Some(individual) = object.get("individual_limit") else {
        return Ok(());
    };
    if individual.is_null() {
        return Ok(());
    }
    let individual = individual
        .as_object()
        .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
    let reached = object
        .get("reached")
        .map(|value| {
            value
                .as_bool()
                .ok_or(CodexCredentialQuotaError::InvalidCredentialData)
        })
        .transpose()?
        .unwrap_or(false);
    let used_percent = quota_percent(individual, "used_percent")?;
    let remaining_percent = quota_percent(individual, "remaining_percent")?;
    let used_percent =
        used_percent.or_else(|| remaining_percent.map(|value| (100.0 - value).clamp(0.0, 100.0)));
    let reset_at = quota_reset_at(individual)?;
    let window_seconds = quota_window_seconds(individual)?.or(Some(2_592_000));
    output.push(CodexQuotaWindow {
        key: "spend-control-monthly".to_owned(),
        source: "spend_control".to_owned(),
        kind: CodexQuotaWindowKind::Monthly,
        role: CodexQuotaWindowRole::Monthly,
        window_seconds,
        used_percent: used_percent.map(|value| if reached { 100.0_f64.max(value) } else { value }),
        reset_at,
    });
    Ok(())
}

fn parse_monthly_limit_window(
    value: &Value,
    output: &mut Vec<CodexQuotaWindow>,
) -> Result<(), CodexCredentialQuotaError> {
    let object = value
        .as_object()
        .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
    let used_percent = quota_percent(object, "used_percent")?;
    let remaining_percent = quota_percent(object, "remaining_percent")?;
    let used_percent =
        used_percent.or_else(|| remaining_percent.map(|value| (100.0 - value).clamp(0.0, 100.0)));
    output.push(CodexQuotaWindow {
        key: "monthly-limit".to_owned(),
        source: "monthly_limit".to_owned(),
        kind: CodexQuotaWindowKind::Monthly,
        role: CodexQuotaWindowRole::Monthly,
        window_seconds: quota_window_seconds(object)?.or(Some(2_592_000)),
        used_percent,
        reset_at: quota_reset_at(object)?,
    });
    Ok(())
}

fn quota_percent(
    object: &Map<String, Value>,
    key: &str,
) -> Result<Option<f64>, CodexCredentialQuotaError> {
    object
        .get(key)
        .map(|value| {
            value
                .as_f64()
                .filter(|value| value.is_finite() && (0.0..=100.0).contains(value))
                .ok_or(CodexCredentialQuotaError::InvalidCredentialData)
        })
        .transpose()
}

fn quota_reset_at(
    object: &Map<String, Value>,
) -> Result<Option<DateTime<Utc>>, CodexCredentialQuotaError> {
    object
        .get("reset_at")
        .map(|value| {
            value
                .as_i64()
                .filter(|value| *value > 0)
                .and_then(|value| DateTime::<Utc>::from_timestamp(value, 0))
                .ok_or(CodexCredentialQuotaError::InvalidCredentialData)
        })
        .transpose()
}

fn quota_window_seconds(
    object: &Map<String, Value>,
) -> Result<Option<u64>, CodexCredentialQuotaError> {
    if let Some(seconds) = optional_positive_u64(object, "window_seconds")? {
        return Ok(Some(seconds));
    }
    if let Some(seconds) = optional_positive_u64(object, "limit_window_seconds")? {
        return Ok(Some(seconds));
    }
    optional_positive_u64(object, "window_minutes")?
        .map(|minutes| {
            minutes
                .checked_mul(60)
                .ok_or(CodexCredentialQuotaError::InvalidCredentialData)
        })
        .transpose()
}

fn optional_positive_u64(
    object: &Map<String, Value>,
    key: &str,
) -> Result<Option<u64>, CodexCredentialQuotaError> {
    object
        .get(key)
        .map(|value| {
            value
                .as_u64()
                .filter(|value| *value > 0)
                .ok_or(CodexCredentialQuotaError::InvalidCredentialData)
        })
        .transpose()
}

const fn quota_window_kind(seconds: Option<u64>) -> CodexQuotaWindowKind {
    match seconds {
        Some(value) if value >= 17_100 && value <= 18_900 => CodexQuotaWindowKind::ShortTerm,
        Some(value) if value >= 574_560 && value <= 635_040 => CodexQuotaWindowKind::Weekly,
        Some(value) if value >= 2_462_400 && value <= 2_721_600 => CodexQuotaWindowKind::Monthly,
        _ => CodexQuotaWindowKind::Other,
    }
}

fn quota_key(value: &str) -> String {
    let mut key = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    while key.contains("--") {
        key = key.replace("--", "-");
    }
    let key = key.trim_matches('-');
    if key.is_empty() {
        "quota".to_owned()
    } else {
        key.to_owned()
    }
}

const fn quota_role_name(role: CodexQuotaWindowRole, kind: CodexQuotaWindowKind) -> &'static str {
    match kind {
        CodexQuotaWindowKind::ShortTerm => "five-hour",
        CodexQuotaWindowKind::Weekly => "weekly",
        CodexQuotaWindowKind::Monthly => "monthly",
        CodexQuotaWindowKind::Other => match role {
            CodexQuotaWindowRole::Primary => "primary",
            CodexQuotaWindowRole::Secondary => "secondary",
            CodexQuotaWindowRole::Monthly => "monthly",
        },
    }
}

#[derive(Default)]
struct QuotaAggregate {
    recognized: bool,
    exhausted: bool,
    remaining_percent: Option<u8>,
    resets_at: Option<DateTime<Utc>>,
}

impl QuotaAggregate {
    fn observe_rate_limit(&mut self, value: &Value) -> Result<(), CodexCredentialQuotaError> {
        let object = value
            .as_object()
            .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
        self.recognized = true;
        self.exhausted |= optional_bool(object, "limit_reached")?.unwrap_or(false);
        self.exhausted |= optional_bool(object, "allowed")?.is_some_and(|allowed| !allowed);
        for key in ["primary_window", "secondary_window"] {
            if let Some(window) = object.get(key) {
                self.observe_window(window)?;
            }
        }
        Ok(())
    }

    fn observe_window(&mut self, value: &Value) -> Result<(), CodexCredentialQuotaError> {
        if value.is_null() {
            return Ok(());
        }
        let object = value
            .as_object()
            .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
        if let Some(used) = object.get("used_percent") {
            let used = used
                .as_f64()
                .filter(|value| value.is_finite() && *value >= 0.0)
                .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
            let remaining = (100.0 - used).clamp(0.0, 100.0).round() as u8;
            self.remaining_percent = Some(
                self.remaining_percent
                    .map_or(remaining, |current| current.min(remaining)),
            );
            self.exhausted |= used >= 100.0;
        }
        if let Some(reset) = object.get("reset_at") {
            let seconds = reset
                .as_i64()
                .filter(|seconds| *seconds > 0)
                .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
            let reset = DateTime::<Utc>::from_timestamp(seconds, 0)
                .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
            self.resets_at = Some(self.resets_at.map_or(reset, |current| current.min(reset)));
        }
        Ok(())
    }

    fn observe_exhaustion_object(
        &mut self,
        value: &Value,
        key: &str,
    ) -> Result<(), CodexCredentialQuotaError> {
        if value.is_null() {
            return Ok(());
        }
        let object = value
            .as_object()
            .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
        self.recognized = true;
        self.exhausted |= optional_bool(object, key)?.unwrap_or(false);
        Ok(())
    }
}

fn optional_bool(
    object: &Map<String, Value>,
    key: &str,
) -> Result<Option<bool>, CodexCredentialQuotaError> {
    object
        .get(key)
        .map(|value| {
            value
                .as_bool()
                .ok_or(CodexCredentialQuotaError::InvalidCredentialData)
        })
        .transpose()
}
