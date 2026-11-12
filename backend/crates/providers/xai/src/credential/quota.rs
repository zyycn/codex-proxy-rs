//! Grok billing 额度事实、展示投影与请求调度缓存

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Utc};
use gateway_core::account::{
    AccountQuotaResetWindow, AccountQuotaSignals, CredentialRevision, ProviderAccount,
    ProviderAccountId, QuotaObservation, QuotaResetPeriod, QuotaState,
};
use tokio::sync::Mutex;

use super::repository::{GrokCredentialRepository, LoadedGrokCredential};
use crate::XaiWireProfileState;
use crate::{
    GrokBillingClient, GrokBillingTransport, GrokModelCatalogSession, SecretValue,
    parse_grok_billing,
};

const QUOTA_SCHEDULING_TTL: Duration = Duration::from_secs(10 * 60);
const QUOTA_HYDRATION_FAILURE_TTL: Duration = Duration::from_secs(5);

/// Grok Build Free 额度的滚动观察窗口
pub const GROK_FREE_ROLLING_WINDOW_SECONDS: u64 = 86_400;

/// xAI Provider 从当前 credits 文档解析出的账号页安全投影
#[derive(Debug, Clone, PartialEq)]
pub struct GrokBillingPresentation {
    plan_type: Option<String>,
    used_percent: Option<f64>,
    period_type: Option<String>,
    period_start: Option<String>,
    period_end: Option<String>,
    on_demand_cap_cents: Option<i64>,
    on_demand_used_cents: Option<i64>,
    prepaid_balance_cents: Option<i64>,
}

/// xAI credits 当前周期的官方语义
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrokQuotaPeriodKind {
    Weekly,
    Monthly,
    Other,
}

impl GrokBillingPresentation {
    #[must_use]
    pub fn plan_type(&self) -> Option<&str> {
        self.plan_type.as_deref()
    }

    #[must_use]
    pub const fn used_percent(&self) -> Option<f64> {
        self.used_percent
    }

    /// 返回 billing 是否包含可直接用于账号额度展示的付费事实
    #[must_use]
    pub fn has_authoritative_quota(&self) -> bool {
        self.used_percent.is_some()
            || self.has_prepaid_credits()
            || [self.on_demand_cap_cents, self.on_demand_used_cents]
                .into_iter()
                .flatten()
                .any(|value| value > 0)
    }

    fn has_prepaid_credits(&self) -> bool {
        // 官方账本可用负数表示充值余额，非零即有预付额度，保留原始符号
        self.prepaid_balance_cents
            .is_some_and(|balance| balance != 0)
    }

    #[must_use]
    pub fn period_type(&self) -> Option<&str> {
        self.period_type.as_deref()
    }

    #[must_use]
    pub fn period_kind(&self) -> GrokQuotaPeriodKind {
        match self.period_type.as_deref() {
            Some("USAGE_PERIOD_TYPE_WEEKLY") => GrokQuotaPeriodKind::Weekly,
            Some("USAGE_PERIOD_TYPE_MONTHLY") => GrokQuotaPeriodKind::Monthly,
            _ => GrokQuotaPeriodKind::Other,
        }
    }

    #[must_use]
    pub fn period_start(&self) -> Option<&str> {
        self.period_start.as_deref()
    }

    #[must_use]
    pub fn period_end(&self) -> Option<&str> {
        self.period_end.as_deref()
    }

    #[must_use]
    pub const fn on_demand_cap_cents(&self) -> Option<i64> {
        self.on_demand_cap_cents
    }

    #[must_use]
    pub const fn on_demand_used_cents(&self) -> Option<i64> {
        self.on_demand_used_cents
    }

    #[must_use]
    pub const fn prepaid_balance_cents(&self) -> Option<i64> {
        self.prepaid_balance_cents
    }
}

/// 一个账号最近一次 xAI billing 观察结果
#[derive(Debug, Clone, PartialEq)]
pub struct GrokQuotaSnapshot {
    account_id: ProviderAccountId,
    credential_revision: CredentialRevision,
    observed_at: DateTime<Utc>,
    billing: GrokBillingPresentation,
}

impl GrokQuotaSnapshot {
    #[must_use]
    pub const fn account_id(&self) -> &ProviderAccountId {
        &self.account_id
    }

    #[must_use]
    pub const fn credential_revision(&self) -> CredentialRevision {
        self.credential_revision
    }

    #[must_use]
    pub const fn observed_at(&self) -> DateTime<Utc> {
        self.observed_at
    }

    #[must_use]
    pub const fn billing(&self) -> &GrokBillingPresentation {
        &self.billing
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GrokQuotaError {
    #[error("xAI quota account is unavailable")]
    AccountUnavailable,
    #[error("xAI quota credential snapshot is stale")]
    StaleCredentialSnapshot,
    #[error("xAI quota credential or billing data is invalid")]
    InvalidData,
    #[error("xAI quota upstream request failed")]
    Upstream,
    #[error("xAI quota store is unavailable")]
    Store,
}

/// 官方 Grok Build billing 同步与 Provider-owned quota JSON 解析服务
#[derive(Clone)]
pub struct GrokCredentialQuotaService {
    repository: GrokCredentialRepository,
    client: Arc<GrokBillingClient>,
    scheduling: GrokQuotaSchedulingProjection,
    wire_profile: XaiWireProfileState,
}

#[derive(Clone, Default)]
struct GrokQuotaSchedulingProjection {
    state: Arc<RwLock<GrokQuotaProjectionState>>,
    hydration: Arc<Mutex<()>>,
}

#[derive(Default)]
struct GrokQuotaProjectionState {
    next_version: u64,
    entries: BTreeMap<ProviderAccountId, GrokQuotaSchedulingEntry>,
}

#[derive(Debug, Clone)]
struct GrokQuotaSchedulingEntry {
    version: u64,
    revision: CredentialRevision,
    expires_at: Instant,
    signals: Option<AccountQuotaSignals>,
}

#[derive(Clone)]
struct GrokQuotaHydrationTarget {
    account: ProviderAccount,
    expected_version: Option<u64>,
}

impl GrokQuotaSchedulingProjection {
    fn invalidate(&self, account_ids: &[ProviderAccountId]) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for account_id in account_ids {
            state.entries.remove(account_id);
        }
    }

    fn hydration_targets(&self, accounts: &[ProviderAccount]) -> Vec<GrokQuotaHydrationTarget> {
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
                    .then(|| GrokQuotaHydrationTarget {
                        account: account.clone(),
                        expected_version: current.map(|entry| entry.version),
                    })
            })
            .collect()
    }

    fn observe(&self, snapshot: &GrokQuotaSnapshot) -> bool {
        let Some(remaining_ttl) = quota_projection_ttl(snapshot.observed_at()) else {
            return false;
        };
        self.replace(
            snapshot.account_id().clone(),
            snapshot.credential_revision(),
            remaining_ttl,
            quota_scheduling_signals(snapshot.billing()),
        );
        true
    }

    fn observe_if_unchanged(
        &self,
        target: &GrokQuotaHydrationTarget,
        snapshot: &GrokQuotaSnapshot,
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
        insert_grok_projection_entry(
            &mut state,
            snapshot.account_id().clone(),
            snapshot.credential_revision(),
            remaining_ttl,
            quota_scheduling_signals(snapshot.billing()),
        );
        true
    }

    fn mark_unknown_if_unchanged(&self, target: &GrokQuotaHydrationTarget, ttl: Duration) {
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
        insert_grok_projection_entry(
            &mut state,
            target.account.id().clone(),
            target.account.revision(),
            ttl,
            None,
        );
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
        insert_grok_projection_entry(&mut state, account_id, revision, ttl, signals);
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
            .and_then(|entry| entry.signals.clone())
    }
}

fn insert_grok_projection_entry(
    state: &mut GrokQuotaProjectionState,
    account_id: ProviderAccountId,
    revision: CredentialRevision,
    ttl: Duration,
    signals: Option<AccountQuotaSignals>,
) {
    state.next_version = state.next_version.saturating_add(1);
    state.entries.insert(
        account_id,
        GrokQuotaSchedulingEntry {
            version: state.next_version,
            revision,
            expires_at: Instant::now() + ttl,
            signals,
        },
    );
}

impl GrokCredentialQuotaService {
    #[must_use]
    pub fn new(
        repository: GrokCredentialRepository,
        transport: Arc<dyn GrokBillingTransport>,
        wire_profile: XaiWireProfileState,
    ) -> Self {
        Self {
            repository,
            client: Arc::new(GrokBillingClient::new(transport)),
            scheduling: GrokQuotaSchedulingProjection::default(),
            wire_profile,
        }
    }

    /// 批量预热请求级额度投影；持久层或 Provider JSON 异常只退化为未知额度
    pub async fn prepare_scheduling(&self, accounts: &[ProviderAccount]) {
        if self.scheduling.hydration_targets(accounts).is_empty() {
            return;
        }
        let _hydration = self.scheduling.hydration.lock().await;
        let pending = self.scheduling.hydration_targets(accounts);
        if pending.is_empty() {
            return;
        }
        let pending_accounts = pending
            .iter()
            .map(|target| target.account.clone())
            .collect::<Vec<_>>();
        let Ok(observations) = self.repository.quota_observations(&pending_accounts).await else {
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

    /// 立即刷新一个账号的动态 billing document，并以 credential revision CAS 写回
    pub async fn refresh_account(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<GrokQuotaSnapshot, GrokQuotaError> {
        let loaded = self
            .repository
            .load_current(account_id)
            .await
            .map_err(map_quota_repository_error)?;
        if !loaded.account.enabled()
            || loaded
                .account
                .access_token_expires_at()
                .is_none_or(|expires_at| expires_at <= SystemTime::now())
        {
            return Err(GrokQuotaError::AccountUnavailable);
        }
        let session = billing_session(&loaded, &self.wire_profile)?;
        let billing = self
            .client
            .fetch(&session)
            .await
            .map_err(|_| GrokQuotaError::Upstream)?;
        let mut document = billing.into_document();
        // 订阅查询失败不影响已取得的额度，也不能据此写入免费套餐
        match self.client.fetch_subscription(&session).await {
            Ok(Some(plan_type)) => {
                document.insert(
                    "subscriptionTier".to_owned(),
                    serde_json::Value::String(plan_type),
                );
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, "xAI subscription query failed"),
        }
        let observed_at = Utc::now();
        let presentation = billing_presentation(&document)?;
        let quota = loaded
            .account
            .quota()
            .merge_observation(quota_access_observation(&presentation, observed_at.into()));
        self.repository
            .replace_quota(
                loaded.account.id().clone(),
                loaded.account.revision(),
                document,
                observed_at.into(),
                quota,
            )
            .await
            .map_err(map_quota_repository_error)?;
        let snapshot = GrokQuotaSnapshot {
            account_id: loaded.account.id().clone(),
            credential_revision: loaded.account.revision(),
            observed_at,
            billing: presentation,
        };
        self.scheduling.observe(&snapshot);
        Ok(snapshot)
    }

    /// 读取并重新验证 Store 中的 Provider-owned quota JSON
    pub async fn read_account(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<Option<GrokQuotaSnapshot>, GrokQuotaError> {
        let Some(observation) = self
            .repository
            .quota(account_id)
            .await
            .map_err(map_quota_repository_error)?
        else {
            return Ok(None);
        };
        let observed_at = observation.observed_at;
        let document = observation.quota.into_inner();
        let snapshot = GrokQuotaSnapshot {
            account_id: observation.account_id,
            credential_revision: observation.expected_revision,
            observed_at: observed_at.into(),
            billing: billing_presentation(&document)?,
        };
        self.scheduling.observe(&snapshot);
        Ok(Some(snapshot))
    }
}

impl fmt::Debug for GrokCredentialQuotaService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrokCredentialQuotaService")
            .field("repository", &self.repository)
            .field("client", &self.client)
            .finish()
    }
}

fn billing_session(
    loaded: &LoadedGrokCredential,
    wire_profile: &XaiWireProfileState,
) -> Result<GrokModelCatalogSession, GrokQuotaError> {
    let upstream_user_id = loaded
        .account
        .upstream_user_id()
        .ok_or(GrokQuotaError::InvalidData)?;
    GrokModelCatalogSession::new(
        loaded.access_token.clone(),
        SecretValue::new(upstream_user_id),
        loaded
            .account
            .email()
            .map(|value| SecretValue::new(value.to_owned())),
        wire_profile.clone(),
    )
    .map(|session| session.with_outbound_proxy(loaded.account.outbound_proxy().cloned()))
    .map_err(|_| GrokQuotaError::InvalidData)
}

fn billing_presentation(
    document: &serde_json::Map<String, serde_json::Value>,
) -> Result<GrokBillingPresentation, GrokQuotaError> {
    let body = serde_json::to_vec(document).map_err(|_| GrokQuotaError::InvalidData)?;
    let snapshot = parse_grok_billing(&body).map_err(|_| GrokQuotaError::InvalidData)?;
    let config = snapshot
        .document()
        .get("config")
        .and_then(|value| value.as_object());
    let current_period = config
        .and_then(|config| config.get("currentPeriod"))
        .and_then(|value| value.as_object());
    let used_percent = config
        .and_then(|config| config.get("creditUsagePercent"))
        .and_then(serde_json::Value::as_f64);
    Ok(GrokBillingPresentation {
        plan_type: dynamic_string(Some(snapshot.document()), "subscriptionTier"),
        used_percent,
        period_type: dynamic_string(current_period, "type"),
        period_start: dynamic_string(current_period, "start"),
        period_end: dynamic_string(current_period, "end"),
        on_demand_cap_cents: cent_value(config, "onDemandCap"),
        on_demand_used_cents: cent_value(config, "onDemandUsed"),
        prepaid_balance_cents: cent_value(config, "prepaidBalance"),
    })
}

fn quota_snapshot_from_observation(observation: &QuotaObservation) -> Option<GrokQuotaSnapshot> {
    let observed_at = DateTime::<Utc>::from(observation.observed_at);
    let document = observation.quota.expose_to_provider();
    Some(GrokQuotaSnapshot {
        account_id: observation.account_id.clone(),
        credential_revision: observation.expected_revision,
        observed_at,
        billing: billing_presentation(document).ok()?,
    })
}

fn quota_projection_ttl(observed_at: DateTime<Utc>) -> Option<Duration> {
    let age = SystemTime::now()
        .duration_since(SystemTime::from(observed_at))
        .unwrap_or(Duration::ZERO);
    QUOTA_SCHEDULING_TTL
        .checked_sub(age)
        .filter(|remaining| !remaining.is_zero())
}

fn quota_scheduling_signals(billing: &GrokBillingPresentation) -> Option<AccountQuotaSignals> {
    let now = SystemTime::now();
    let period_end = billing
        .period_end()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| SystemTime::from(value.to_utc()));
    let observation_expired = period_end.is_some_and(|period_end| period_end <= now);
    let remaining_rank = (!observation_expired)
        .then(|| {
            billing
                .used_percent()
                .filter(|used| used.is_finite() && (0.0..=100.0).contains(used))
                .map(|used| ((100.0 - used) * 100.0).round() as u64)
        })
        .flatten();
    let reset_at = period_end.filter(|reset_at| *reset_at > now);
    (remaining_rank.is_some() || reset_at.is_some()).then(|| {
        let signals = AccountQuotaSignals::new(reset_at, remaining_rank);
        let period = match billing.period_kind() {
            GrokQuotaPeriodKind::Weekly => Some(QuotaResetPeriod::Weekly),
            GrokQuotaPeriodKind::Monthly => Some(QuotaResetPeriod::Monthly),
            GrokQuotaPeriodKind::Other => None,
        };
        match (period, reset_at) {
            (Some(period), Some(reset_at)) if billing.has_authoritative_quota() => {
                signals.with_reset_windows(vec![AccountQuotaResetWindow { period, reset_at }])
            }
            _ => signals,
        }
    })
}

/// 把 xAI billing 文档归一化为访问结论
/// 缺少正反证据时返回 Unknown，
/// 由 `QuotaState::merge_observation` 保留既有已确认结论
fn quota_access_observation(
    billing: &GrokBillingPresentation,
    observed_at: SystemTime,
) -> QuotaState {
    if !billing.has_authoritative_quota() {
        return QuotaState::observed_unknown(observed_at);
    }
    if billing_proves_access_allowed(billing, observed_at) {
        QuotaState::allowed(observed_at)
    } else {
        QuotaState::observed_unknown(observed_at)
    }
}

fn billing_proves_access_allowed(
    billing: &GrokBillingPresentation,
    observed_at: SystemTime,
) -> bool {
    if billing
        .period_end()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| SystemTime::from(value.to_utc()))
        .is_some_and(|period_end| period_end <= observed_at)
    {
        return false;
    }
    billing.has_prepaid_credits()
        || billing
            .used_percent()
            .is_some_and(|used| used.is_finite() && used < 100.0)
        || matches!(
            (billing.on_demand_used_cents(), billing.on_demand_cap_cents()),
            (Some(used), Some(cap)) if cap > 0 && used < cap
        )
}

fn dynamic_string(
    object: Option<&serde_json::Map<String, serde_json::Value>>,
    field: &str,
) -> Option<String> {
    object?
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

fn cent_value(
    object: Option<&serde_json::Map<String, serde_json::Value>>,
    field: &str,
) -> Option<i64> {
    let cent = object?.get(field)?.as_object()?;
    match cent.get("val") {
        Some(value) => value.as_i64(),
        None => Some(0),
    }
}

fn map_quota_repository_error(
    error: super::repository::GrokCredentialRepositoryError,
) -> GrokQuotaError {
    use super::repository::GrokCredentialRepositoryError as RepositoryError;
    match error {
        RepositoryError::CredentialNotFound | RepositoryError::WrongProviderKind => {
            GrokQuotaError::AccountUnavailable
        }
        RepositoryError::StaleCredentialRevision(_) | RepositoryError::Conflict(_) => {
            GrokQuotaError::StaleCredentialSnapshot
        }
        RepositoryError::Store(_) => GrokQuotaError::Store,
        RepositoryError::InvalidInput(_)
        | RepositoryError::IdentityRebind
        | RepositoryError::InvalidCredentialData(_)
        | RepositoryError::RevisionOverflow => GrokQuotaError::InvalidData,
    }
}
