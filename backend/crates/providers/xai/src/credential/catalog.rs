//! xAI OAuth account 的实时模型目录与可重建 TTL cache 边界。

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::{StreamExt, TryStreamExt, stream};
use gateway_core::engine::credential::{
    AccountAvailability, AccountQuotaSignals, CredentialRevision, OpaqueProviderData,
    ProviderAccount, ProviderAccountId, QuotaObservation,
};
use gateway_core::engine::provider::ProviderCatalogGeneration;
use gateway_core::provider_ports::{ProviderCatalogCacheKey, ProviderCatalogCachePort};
use gateway_core::routing::{ConfigRevision, ProviderKind};
use tokio::sync::Mutex;

use super::repository::{GrokCredentialRepository, LoadedGrokCredential};
use super::types::{GrokCredentialAvailability, UpdateGrokCredentialState};
use crate::XaiWireProfileState;
use crate::transport::catalog::{MAX_CATALOG_MODELS, valid_model_slug, validate_etag};
use crate::{
    GrokBillingClient, GrokBillingTransport, GrokCatalogModel, GrokModelCatalogClient,
    GrokModelCatalogSession, GrokModelCatalogSnapshot, GrokModelCatalogTransport, SecretValue,
    parse_grok_billing,
};

const MAX_CONCURRENT_CATALOG_REQUESTS: usize = 8;
const CATALOG_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(5 * 60);
const QUOTA_SCHEDULING_TTL: Duration = Duration::from_secs(10 * 60);
const QUOTA_HYDRATION_FAILURE_TTL: Duration = Duration::from_secs(5);

/// Grok Build Free 额度的滚动观察窗口。
pub const GROK_FREE_ROLLING_WINDOW_SECONDS: u64 = 86_400;

/// xAI Provider 从动态 billing JSON 解析出的旧账号页安全投影。
#[derive(Debug, Clone, PartialEq)]
pub struct GrokBillingPresentation {
    used_percent: Option<f64>,
    period_type: Option<String>,
    period_start: Option<String>,
    period_end: Option<String>,
    monthly_limit_cents: Option<i64>,
    included_used_cents: Option<i64>,
    on_demand_cap_cents: Option<i64>,
    on_demand_used_cents: Option<i64>,
    prepaid_balance_cents: Option<i64>,
}

/// xAI credits 当前周期的官方语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrokQuotaPeriodKind {
    Weekly,
    Monthly,
    Other,
}

impl GrokBillingPresentation {
    #[must_use]
    pub const fn used_percent(&self) -> Option<f64> {
        self.used_percent
    }

    /// 返回 billing 是否包含可直接用于账号额度展示的付费事实。
    #[must_use]
    pub fn has_authoritative_quota(&self) -> bool {
        self.used_percent.is_some()
            || [
                self.monthly_limit_cents,
                self.included_used_cents,
                self.on_demand_cap_cents,
                self.on_demand_used_cents,
                self.prepaid_balance_cents,
            ]
            .into_iter()
            .flatten()
            .any(|value| value > 0)
    }

    #[must_use]
    pub fn period_type(&self) -> Option<&str> {
        self.period_type.as_deref()
    }

    #[must_use]
    pub fn period_kind(&self) -> GrokQuotaPeriodKind {
        match self.period_type.as_deref().map(str::trim) {
            Some(value)
                if value.eq_ignore_ascii_case("USAGE_PERIOD_TYPE_WEEKLY")
                    || value.eq_ignore_ascii_case("WEEKLY") =>
            {
                GrokQuotaPeriodKind::Weekly
            }
            Some(value)
                if value.eq_ignore_ascii_case("USAGE_PERIOD_TYPE_MONTHLY")
                    || value.eq_ignore_ascii_case("MONTHLY") =>
            {
                GrokQuotaPeriodKind::Monthly
            }
            None if self.monthly_limit_cents.is_some() || self.included_used_cents.is_some() => {
                GrokQuotaPeriodKind::Monthly
            }
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
    pub const fn monthly_limit_cents(&self) -> Option<i64> {
        self.monthly_limit_cents
    }

    #[must_use]
    pub const fn included_used_cents(&self) -> Option<i64> {
        self.included_used_cents
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

/// 一个账号最近一次 xAI billing 观察结果。
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

/// 官方 Grok Build billing 同步与 Provider-owned quota JSON 解析服务。
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

#[derive(Debug, Clone, Copy)]
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
            .and_then(|entry| entry.signals)
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

    /// 立即刷新一个账号的动态 billing document，并以 credential revision CAS 写回。
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
            || loaded.account.access_token_expires_at() <= SystemTime::now()
        {
            return Err(GrokQuotaError::AccountUnavailable);
        }
        let session = billing_session(&loaded, &self.wire_profile)?;
        let billing = self
            .client
            .fetch(&session)
            .await
            .map_err(|_| GrokQuotaError::Upstream)?;
        let observed_at = Utc::now();
        let document = billing.into_document();
        let presentation = billing_presentation(&document)?;
        self.repository
            .replace_quota(
                loaded.account.id().clone(),
                loaded.account.revision(),
                document,
                observed_at.into(),
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
        let current = self
            .repository
            .load_current(account_id)
            .await
            .map_err(map_quota_repository_error)?;
        if quota_refresh_may_update_state(&loaded.account, &current.account)
            && let Some(availability) = quota_refresh_availability(
                current.account.availability(),
                quota_is_exhausted(snapshot.billing()),
            )
        {
            self.repository
                .update_state(&UpdateGrokCredentialState {
                    account_id: current.account.id().clone(),
                    expected_revision: current.account.revision(),
                    availability,
                    availability_reason: matches!(
                        availability,
                        GrokCredentialAvailability::QuotaExhausted
                    )
                    .then_some("quota_exhausted".to_owned()),
                    cooldown_until: None,
                    observed_at,
                })
                .await
                .map_err(map_quota_repository_error)?;
        }
        Ok(snapshot)
    }

    /// 读取并重新验证 Store 中的 Provider-owned quota JSON。
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
        let (Some(quota), Some(observed_at)) = (observation.quota, observation.observed_at) else {
            return Err(GrokQuotaError::InvalidData);
        };
        let document = quota.into_inner();
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

/// 官方 `/v1/models` 形成的一个 account 完整模型集合。
#[derive(Clone, PartialEq, Eq)]
pub struct GrokCredentialCatalogSeed {
    etag: Option<String>,
    model_slugs: Vec<String>,
}

impl GrokCredentialCatalogSeed {
    pub fn new<I, S>(models: I, etag: Option<String>) -> Result<Self, GrokCredentialCatalogError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut model_slugs = models.into_iter().map(Into::into).collect::<Vec<_>>();
        if model_slugs.is_empty()
            || model_slugs.len() > MAX_CATALOG_MODELS
            || model_slugs.iter().any(|model| !valid_model_slug(model))
        {
            return Err(GrokCredentialCatalogError::InvalidCredentialData);
        }
        model_slugs.sort();
        if model_slugs.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(GrokCredentialCatalogError::ConflictingModelFacts);
        }
        let etag = etag
            .map(|value| validate_etag(&value))
            .transpose()
            .map_err(|_| GrokCredentialCatalogError::InvalidCredentialData)?;
        Ok(Self { etag, model_slugs })
    }

    fn from_snapshot(
        snapshot: &GrokModelCatalogSnapshot,
    ) -> Result<Self, GrokCredentialCatalogError> {
        Self::new(
            snapshot
                .models()
                .iter()
                .map(|model| model.request_model().as_str()),
            snapshot.etag().map(str::to_owned),
        )
    }

    #[must_use]
    pub fn permits(&self, model: &str) -> bool {
        self.model_slugs
            .binary_search_by(|candidate| candidate.as_str().cmp(model))
            .is_ok()
    }

    #[must_use]
    pub fn models(&self) -> &[String] {
        &self.model_slugs
    }

    #[must_use]
    pub fn etag(&self) -> Option<&str> {
        self.etag.as_deref()
    }
}

impl fmt::Debug for GrokCredentialCatalogSeed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrokCredentialCatalogSeed")
            .field("model_count", &self.model_slugs.len())
            .field("etag", &self.etag.as_ref().map(|_| "[PRESENT]"))
            .finish()
    }
}

/// Redis/内存 TTL cache 中的一条可重建 account catalog。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrokAccountCatalog {
    account_id: ProviderAccountId,
    revision: CredentialRevision,
    observed_at: DateTime<Utc>,
    seed: GrokCredentialCatalogSeed,
}

impl GrokAccountCatalog {
    #[must_use]
    pub const fn new(
        account_id: ProviderAccountId,
        revision: CredentialRevision,
        observed_at: DateTime<Utc>,
        seed: GrokCredentialCatalogSeed,
    ) -> Self {
        Self {
            account_id,
            revision,
            observed_at,
            seed,
        }
    }

    #[must_use]
    pub const fn account_id(&self) -> &ProviderAccountId {
        &self.account_id
    }

    #[must_use]
    pub const fn revision(&self) -> CredentialRevision {
        self.revision
    }

    #[must_use]
    pub const fn observed_at(&self) -> DateTime<Utc> {
        self.observed_at
    }

    #[must_use]
    pub const fn seed(&self) -> &GrokCredentialCatalogSeed {
        &self.seed
    }
}

/// Provider-owned catalog cache；实现只能保存可重建 TTL 数据。
#[async_trait]
pub trait GrokCredentialCatalogCache: Send + Sync {
    async fn replace(&self, catalog: GrokAccountCatalog) -> Result<(), GrokCatalogCacheError>;

    async fn read(
        &self,
        account_id: &ProviderAccountId,
        revision: CredentialRevision,
    ) -> Result<Option<GrokAccountCatalog>, GrokCatalogCacheError>;

    async fn observed_model_support(
        &self,
        account_id: &ProviderAccountId,
        revision: CredentialRevision,
        model: &str,
    ) -> Result<Option<bool>, GrokCatalogCacheError>;
}

/// xAI 负责解释 catalog 文档，Store 只保存 opaque JSON。
pub struct GrokCatalogCache {
    port: Arc<dyn ProviderCatalogCachePort>,
    provider_kind: ProviderKind,
}

impl GrokCatalogCache {
    pub fn new(port: Arc<dyn ProviderCatalogCachePort>) -> Result<Self, GrokCatalogCacheError> {
        Ok(Self {
            port,
            provider_kind: ProviderKind::new("xai")
                .map_err(|_| GrokCatalogCacheError::InvalidData)?,
        })
    }

    fn key(
        &self,
        account_id: &ProviderAccountId,
        revision: CredentialRevision,
    ) -> ProviderCatalogCacheKey {
        ProviderCatalogCacheKey::new(self.provider_kind.clone(), account_id.clone(), revision)
    }

    fn encode(catalog: &GrokAccountCatalog) -> OpaqueProviderData {
        let mut document = serde_json::Map::new();
        document.insert("version".to_owned(), serde_json::Value::from(1));
        document.insert(
            "observedAt".to_owned(),
            serde_json::Value::String(catalog.observed_at().to_rfc3339()),
        );
        if let Some(etag) = catalog.seed().etag() {
            document.insert(
                "etag".to_owned(),
                serde_json::Value::String(etag.to_owned()),
            );
        }
        document.insert(
            "models".to_owned(),
            serde_json::Value::Array(
                catalog
                    .seed()
                    .models()
                    .iter()
                    .cloned()
                    .map(serde_json::Value::String)
                    .collect(),
            ),
        );
        OpaqueProviderData::new(document)
    }

    fn decode(
        account_id: &ProviderAccountId,
        revision: CredentialRevision,
        document: OpaqueProviderData,
    ) -> Result<GrokAccountCatalog, GrokCatalogCacheError> {
        let mut fields = document.into_inner();
        if fields.remove("version").and_then(|value| value.as_u64()) != Some(1) {
            return Err(GrokCatalogCacheError::InvalidData);
        }
        let observed_at = fields
            .remove("observedAt")
            .and_then(|value| value.as_str().map(ToOwned::to_owned))
            .and_then(|value| DateTime::parse_from_rfc3339(&value).ok())
            .map(|value| value.with_timezone(&Utc))
            .ok_or(GrokCatalogCacheError::InvalidData)?;
        let etag = match fields.remove("etag") {
            None => None,
            Some(serde_json::Value::String(value)) => Some(value),
            Some(_) => return Err(GrokCatalogCacheError::InvalidData),
        };
        let models = fields
            .remove("models")
            .and_then(|value| value.as_array().cloned())
            .ok_or(GrokCatalogCacheError::InvalidData)?
            .into_iter()
            .map(|value| {
                value
                    .as_str()
                    .map(ToOwned::to_owned)
                    .ok_or(GrokCatalogCacheError::InvalidData)
            })
            .collect::<Result<Vec<_>, _>>()?;
        if !fields.is_empty() {
            return Err(GrokCatalogCacheError::InvalidData);
        }
        let seed = GrokCredentialCatalogSeed::new(models, etag)
            .map_err(|_| GrokCatalogCacheError::InvalidData)?;
        Ok(GrokAccountCatalog::new(
            account_id.clone(),
            revision,
            observed_at,
            seed,
        ))
    }
}

#[async_trait]
impl GrokCredentialCatalogCache for GrokCatalogCache {
    async fn replace(&self, catalog: GrokAccountCatalog) -> Result<(), GrokCatalogCacheError> {
        self.port
            .replace(
                &self.key(catalog.account_id(), catalog.revision()),
                &Self::encode(&catalog),
                CATALOG_CACHE_TTL,
            )
            .await
            .map_err(|_| GrokCatalogCacheError::Unavailable)
    }

    async fn read(
        &self,
        account_id: &ProviderAccountId,
        revision: CredentialRevision,
    ) -> Result<Option<GrokAccountCatalog>, GrokCatalogCacheError> {
        self.port
            .read(&self.key(account_id, revision))
            .await
            .map_err(|_| GrokCatalogCacheError::Unavailable)?
            .map(|document| Self::decode(account_id, revision, document))
            .transpose()
    }

    async fn observed_model_support(
        &self,
        account_id: &ProviderAccountId,
        revision: CredentialRevision,
        model: &str,
    ) -> Result<Option<bool>, GrokCatalogCacheError> {
        Ok(self
            .read(account_id, revision)
            .await?
            .map(|catalog| catalog.seed().permits(model)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GrokCatalogCacheError {
    #[error("xAI model catalog cache is unavailable")]
    Unavailable,
    #[error("xAI model catalog cache data is invalid")]
    InvalidData,
}

/// 一次 Provider 同步得到的账号目录和严格模型并集。
#[derive(Clone, Debug)]
pub struct GrokCredentialCatalogSnapshot {
    config_revision: ConfigRevision,
    observed_at: DateTime<Utc>,
    accounts: Vec<GrokAccountCatalog>,
    models: Vec<GrokCatalogModel>,
}

impl GrokCredentialCatalogSnapshot {
    #[must_use]
    pub const fn config_revision(&self) -> ConfigRevision {
        self.config_revision
    }

    #[must_use]
    pub const fn observed_at(&self) -> DateTime<Utc> {
        self.observed_at
    }

    #[must_use]
    pub fn accounts(&self) -> &[GrokAccountCatalog] {
        &self.accounts
    }

    #[must_use]
    pub fn models(&self) -> &[GrokCatalogModel] {
        &self.models
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GrokCredentialCatalogError {
    #[error("Grok model catalog has no eligible OAuth account")]
    NoEligibleCredential,
    #[error("Grok model catalog credential snapshot is stale")]
    StaleCredentialSnapshot,
    #[error("Grok model catalog credential data is invalid")]
    InvalidCredentialData,
    #[error("Grok model catalog upstream snapshot failed")]
    Upstream,
    #[error("Grok model catalog contains conflicting account-scoped model facts")]
    ConflictingModelFacts,
    #[error("Grok model catalog cache update failed")]
    Cache,
    #[error("Grok provider account store is unavailable")]
    Store,
}

#[derive(Clone)]
pub struct GrokCredentialCatalogService {
    repository: GrokCredentialRepository,
    client: Arc<GrokModelCatalogClient>,
    cache: Arc<dyn GrokCredentialCatalogCache>,
    published: Arc<RwLock<PublishedCatalogState>>,
    wire_profile: XaiWireProfileState,
}

#[derive(Default)]
struct PublishedCatalogState {
    generation: u64,
    models: Vec<GrokCatalogModel>,
}

impl GrokCredentialCatalogService {
    #[must_use]
    pub fn new(
        repository: GrokCredentialRepository,
        transport: Arc<dyn GrokModelCatalogTransport>,
        cache: Arc<dyn GrokCredentialCatalogCache>,
        wire_profile: XaiWireProfileState,
    ) -> Self {
        Self {
            repository,
            client: Arc::new(GrokModelCatalogClient::new(transport)),
            cache,
            published: Arc::new(RwLock::new(PublishedCatalogState::default())),
            wire_profile,
        }
    }

    #[must_use]
    pub fn catalog_generation(&self) -> ProviderCatalogGeneration {
        let published = self
            .published
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ProviderCatalogGeneration::new(published.generation)
    }

    pub async fn fetch_seed(
        &self,
        access_token: SecretValue,
        user_id: SecretValue,
        email: Option<SecretValue>,
    ) -> Result<GrokCredentialCatalogSeed, GrokCredentialCatalogError> {
        let session =
            GrokModelCatalogSession::new(access_token, user_id, email, self.wire_profile.clone())
                .map_err(|_| GrokCredentialCatalogError::InvalidCredentialData)?;
        let snapshot = self
            .client
            .fetch(&session)
            .await
            .map_err(|_| GrokCredentialCatalogError::Upstream)?;
        GrokCredentialCatalogSeed::from_snapshot(&snapshot)
    }

    pub async fn cache_seed(
        &self,
        account_id: ProviderAccountId,
        revision: CredentialRevision,
        seed: GrokCredentialCatalogSeed,
    ) -> Result<(), GrokCredentialCatalogError> {
        self.cache
            .replace(GrokAccountCatalog::new(
                account_id,
                revision,
                Utc::now(),
                seed,
            ))
            .await
            .map_err(|_| GrokCredentialCatalogError::Cache)
    }

    /// 实时刷新一个指定账号的模型集合并写入可重建 cache。
    pub async fn refresh_account_catalog(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<GrokAccountCatalog, GrokCredentialCatalogError> {
        let candidate = self
            .repository
            .load_current(account_id)
            .await
            .map_err(|_| GrokCredentialCatalogError::Store)?;
        if !eligible_catalog_candidate(&candidate) {
            return Err(GrokCredentialCatalogError::NoEligibleCredential);
        }
        let revision = candidate.account.revision();
        let seed = self
            .fetch_seed(
                candidate.access_token,
                SecretValue::new(candidate.account.upstream_user_id().to_owned()),
                candidate
                    .account
                    .email()
                    .map(|email| SecretValue::new(email.to_owned())),
            )
            .await?;
        let catalog = GrokAccountCatalog::new(account_id.clone(), revision, Utc::now(), seed);
        self.cache
            .replace(catalog.clone())
            .await
            .map_err(|_| GrokCredentialCatalogError::Cache)?;
        Ok(catalog)
    }

    /// 读取一个指定 revision 的已验证模型 cache，不触发上游请求。
    pub async fn read_account_catalog(
        &self,
        account_id: &ProviderAccountId,
        revision: CredentialRevision,
    ) -> Result<Option<GrokAccountCatalog>, GrokCredentialCatalogError> {
        self.cache
            .read(account_id, revision)
            .await
            .map_err(|_| GrokCredentialCatalogError::Cache)
    }

    /// 实时拉取全部 eligible OAuth account；结果只写可重建 TTL cache，不写 PostgreSQL。
    pub async fn synchronize(
        &self,
        config_revision: ConfigRevision,
    ) -> Result<GrokCredentialCatalogSnapshot, GrokCredentialCatalogError> {
        let catalog = self.fetch_and_cache().await?;
        Ok(GrokCredentialCatalogSnapshot {
            config_revision,
            observed_at: catalog.observed_at,
            accounts: catalog.accounts,
            models: catalog.models,
        })
    }

    /// Provider Registry 构建 RuntimeSnapshot 时使用的实时能力目录。
    pub async fn query_models(&self) -> Result<Vec<GrokCatalogModel>, GrokCredentialCatalogError> {
        Ok(self.fetch_and_cache().await?.models)
    }

    async fn fetch_and_cache(&self) -> Result<FetchedProviderCatalog, GrokCredentialCatalogError> {
        let candidates = self
            .repository
            .list_loaded_for_provider()
            .await
            .map_err(|_| GrokCredentialCatalogError::Store)?
            .into_iter()
            .filter(eligible_catalog_candidate)
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return Err(GrokCredentialCatalogError::NoEligibleCredential);
        }

        let wire_profile = self.wire_profile.clone();
        let mut fetched = stream::iter(candidates.into_iter().map(|candidate| {
            let client = Arc::clone(&self.client);
            let wire_profile = wire_profile.clone();
            async move { fetch_candidate_catalog(client, candidate, wire_profile).await }
        }))
        .buffer_unordered(MAX_CONCURRENT_CATALOG_REQUESTS)
        .try_collect::<Vec<_>>()
        .await?;
        fetched.sort_by(|left, right| left.account_id.cmp(&right.account_id));
        let models = strict_model_union(&fetched)?;
        let observed_at = Utc::now();
        let accounts = fetched
            .into_iter()
            .map(|fetched| GrokAccountCatalog {
                account_id: fetched.account_id,
                revision: fetched.revision,
                observed_at,
                seed: fetched.seed,
            })
            .collect::<Vec<_>>();
        for account in &accounts {
            self.cache
                .replace(account.clone())
                .await
                .map_err(|_| GrokCredentialCatalogError::Cache)?;
        }
        self.publish_models(&models);

        Ok(FetchedProviderCatalog {
            observed_at,
            accounts,
            models,
        })
    }

    fn publish_models(&self, models: &[GrokCatalogModel]) {
        let mut published = self
            .published
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if published.models == models {
            return;
        }
        published.models = models.to_vec();
        published.generation = published.generation.saturating_add(1);
    }
}

impl fmt::Debug for GrokCredentialCatalogService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrokCredentialCatalogService")
            .field("repository", &self.repository)
            .field("client", &self.client)
            .field("cache", &"[TTL_CACHE]")
            .finish()
    }
}

struct FetchedCredentialCatalog {
    account_id: ProviderAccountId,
    revision: CredentialRevision,
    snapshot: GrokModelCatalogSnapshot,
    seed: GrokCredentialCatalogSeed,
}

struct FetchedProviderCatalog {
    observed_at: DateTime<Utc>,
    accounts: Vec<GrokAccountCatalog>,
    models: Vec<GrokCatalogModel>,
}

fn eligible_catalog_candidate(candidate: &LoadedGrokCredential) -> bool {
    let account = &candidate.account;
    let now = SystemTime::now();
    account.enabled()
        && account.access_token_expires_at() > now
        && candidate
            .refresh_token_expires_at
            .is_none_or(|expires_at| expires_at > Utc::now())
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

async fn fetch_candidate_catalog(
    client: Arc<GrokModelCatalogClient>,
    candidate: LoadedGrokCredential,
    wire_profile: XaiWireProfileState,
) -> Result<FetchedCredentialCatalog, GrokCredentialCatalogError> {
    let session = GrokModelCatalogSession::new(
        candidate.access_token,
        SecretValue::new(candidate.account.upstream_user_id().to_owned()),
        candidate
            .account
            .email()
            .map(|value| SecretValue::new(value.to_owned())),
        wire_profile,
    )
    .map_err(|_| GrokCredentialCatalogError::InvalidCredentialData)?;
    let snapshot = client
        .fetch(&session)
        .await
        .map_err(|_| GrokCredentialCatalogError::Upstream)?;
    let seed = GrokCredentialCatalogSeed::from_snapshot(&snapshot)?;
    Ok(FetchedCredentialCatalog {
        account_id: candidate.account.id().clone(),
        revision: candidate.account.revision(),
        snapshot,
        seed,
    })
}

fn strict_model_union(
    fetched: &[FetchedCredentialCatalog],
) -> Result<Vec<GrokCatalogModel>, GrokCredentialCatalogError> {
    let mut union = BTreeMap::<String, GrokCatalogModel>::new();
    for credential in fetched {
        for model in credential.snapshot.models() {
            let slug = model.request_model().as_str().to_owned();
            if let Some(existing) = union.get(&slug) {
                if existing != model {
                    return Err(GrokCredentialCatalogError::ConflictingModelFacts);
                }
            } else {
                union.insert(slug, model.clone());
            }
        }
    }
    if union.is_empty() {
        return Err(GrokCredentialCatalogError::Upstream);
    }
    Ok(union.into_values().collect())
}

fn billing_session(
    loaded: &LoadedGrokCredential,
    wire_profile: &XaiWireProfileState,
) -> Result<GrokModelCatalogSession, GrokQuotaError> {
    GrokModelCatalogSession::new(
        loaded.access_token.clone(),
        SecretValue::new(loaded.account.upstream_user_id().to_owned()),
        loaded
            .account
            .email()
            .map(|value| SecretValue::new(value.to_owned())),
        wire_profile.clone(),
    )
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
    let monthly_limit_cents = cent_value(config, "monthlyLimit");
    let included_used_cents = cent_value(config, "used");
    let used_percent = config
        .and_then(|config| config.get("creditUsagePercent"))
        .and_then(serde_json::Value::as_f64)
        .or_else(|| match (included_used_cents, monthly_limit_cents) {
            (Some(used), Some(limit)) if limit > 0 => {
                Some((used.min(limit) as f64 / limit as f64) * 100.0)
            }
            _ => None,
        });
    Ok(GrokBillingPresentation {
        used_percent,
        period_type: dynamic_string(current_period, "type"),
        period_start: dynamic_string(current_period, "start")
            .or_else(|| dynamic_string(config, "billingPeriodStart")),
        period_end: dynamic_string(current_period, "end")
            .or_else(|| dynamic_string(config, "billingPeriodEnd")),
        monthly_limit_cents,
        included_used_cents,
        on_demand_cap_cents: cent_value(config, "onDemandCap"),
        on_demand_used_cents: cent_value(config, "onDemandUsed"),
        prepaid_balance_cents: cent_value(config, "prepaidBalance"),
    })
}

fn quota_snapshot_from_observation(observation: &QuotaObservation) -> Option<GrokQuotaSnapshot> {
    let observed_at = DateTime::<Utc>::from(observation.observed_at?);
    let document = observation.quota.as_ref()?.expose_to_provider();
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
    let remaining_rank = billing
        .used_percent()
        .filter(|used| used.is_finite() && (0.0..=100.0).contains(used))
        .map(|used| ((100.0 - used) * 100.0).round() as u64);
    let now = SystemTime::now();
    let reset_at = billing
        .period_end()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| SystemTime::from(value.to_utc()))
        .filter(|reset_at| *reset_at > now);
    (remaining_rank.is_some() || reset_at.is_some())
        .then(|| AccountQuotaSignals::new(reset_at, remaining_rank))
}

fn quota_is_exhausted(billing: &GrokBillingPresentation) -> bool {
    if billing
        .prepaid_balance_cents()
        .is_some_and(|balance| balance > 0)
        || billing
            .used_percent()
            .is_some_and(|used| used.is_finite() && used < 100.0)
        || matches!(
            (billing.on_demand_used_cents(), billing.on_demand_cap_cents()),
            (Some(used), Some(cap)) if cap > 0 && used < cap
        )
        || matches!(
            (billing.included_used_cents(), billing.monthly_limit_cents()),
            (Some(used), Some(limit)) if limit > 0 && used < limit
        )
    {
        return false;
    }
    billing
        .used_percent()
        .is_some_and(|used| used.is_finite() && used >= 100.0)
        || matches!(
            (billing.included_used_cents(), billing.monthly_limit_cents()),
            (Some(used), Some(limit)) if limit > 0 && used >= limit
        )
        || matches!(
            (billing.on_demand_used_cents(), billing.on_demand_cap_cents()),
            (Some(used), Some(cap)) if cap > 0 && used >= cap
        )
}

fn quota_refresh_may_update_state(before: &ProviderAccount, current: &ProviderAccount) -> bool {
    current.enabled()
        && current.access_token_expires_at() > SystemTime::now()
        && current.revision() == before.revision()
        && current.availability() == before.availability()
        && current.cooldown_until() == before.cooldown_until()
}

fn quota_refresh_availability(
    current: AccountAvailability,
    exhausted: bool,
) -> Option<GrokCredentialAvailability> {
    match current {
        AccountAvailability::Invalid
        | AccountAvailability::Expired
        | AccountAvailability::Banned => None,
        AccountAvailability::QuotaExhausted => {
            (!exhausted).then_some(GrokCredentialAvailability::Ready)
        }
        AccountAvailability::Cooldown | AccountAvailability::Unknown => Some(if exhausted {
            GrokCredentialAvailability::QuotaExhausted
        } else {
            GrokCredentialAvailability::Ready
        }),
        AccountAvailability::Ready => {
            exhausted.then_some(GrokCredentialAvailability::QuotaExhausted)
        }
    }
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
        RepositoryError::StaleCredentialRevision | RepositoryError::Conflict => {
            GrokQuotaError::StaleCredentialSnapshot
        }
        RepositoryError::Store => GrokQuotaError::Store,
        RepositoryError::InvalidInput(_)
        | RepositoryError::IdentityRebind
        | RepositoryError::InvalidCredentialData
        | RepositoryError::RevisionOverflow => GrokQuotaError::InvalidData,
    }
}
