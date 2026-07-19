//! xAI OAuth account 的实时模型目录与可重建 TTL cache 边界。

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::{StreamExt, TryStreamExt, stream};
use gateway_core::engine::credential::{
    AccountAvailability, CredentialRevision, ProviderAccountId,
};
use gateway_core::routing::{ConfigRevision, ProviderInstance};

use super::repository::{GrokCredentialRepository, LoadedGrokCredential};
use crate::transport::GROK_CLIENT_VERSION;
use crate::transport::catalog::{MAX_CATALOG_MODELS, valid_model_slug, validate_etag};
use crate::{
    GrokBillingClient, GrokBillingTransport, GrokBuildProvider, GrokCatalogModel,
    GrokModelCatalogClient, GrokModelCatalogSession, GrokModelCatalogSnapshot,
    GrokModelCatalogTransport, SecretValue, parse_grok_billing,
};

const MAX_CONCURRENT_CATALOG_REQUESTS: usize = 8;

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
}

impl GrokCredentialQuotaService {
    #[must_use]
    pub fn new(
        repository: GrokCredentialRepository,
        transport: Arc<dyn GrokBillingTransport>,
    ) -> Self {
        Self {
            repository,
            client: Arc::new(GrokBillingClient::new(transport)),
        }
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
        let session = billing_session(&loaded)?;
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
        Ok(GrokQuotaSnapshot {
            account_id: loaded.account.id().clone(),
            credential_revision: loaded.account.revision(),
            observed_at,
            billing: presentation,
        })
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
        Ok(Some(GrokQuotaSnapshot {
            account_id: observation.account_id,
            credential_revision: observation.expected_revision,
            observed_at: observed_at.into(),
            billing: billing_presentation(&document)?,
        }))
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

    async fn permits(
        &self,
        account_id: &ProviderAccountId,
        revision: CredentialRevision,
        model: &str,
    ) -> Result<bool, GrokCatalogCacheError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GrokCatalogCacheError {
    #[error("xAI model catalog cache is unavailable")]
    Unavailable,
    #[error("xAI model catalog cache data is invalid")]
    InvalidData,
}

/// 一次 instance 同步得到的账号目录和严格模型并集。
#[derive(Clone, Debug)]
pub struct GrokCredentialCatalogSnapshot {
    config_revision: ConfigRevision,
    provider_instance_id: String,
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
    pub fn provider_instance_id(&self) -> &str {
        &self.provider_instance_id
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
    #[error("Grok model catalog instance configuration is invalid")]
    InvalidInstance,
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
}

impl GrokCredentialCatalogService {
    #[must_use]
    pub fn new(
        repository: GrokCredentialRepository,
        transport: Arc<dyn GrokModelCatalogTransport>,
        cache: Arc<dyn GrokCredentialCatalogCache>,
    ) -> Self {
        Self {
            repository,
            client: Arc::new(GrokModelCatalogClient::new(transport)),
            cache,
        }
    }

    pub async fn fetch_seed(
        &self,
        access_token: SecretValue,
        user_id: SecretValue,
        email: Option<SecretValue>,
        client_version: impl Into<String>,
    ) -> Result<GrokCredentialCatalogSeed, GrokCredentialCatalogError> {
        let session = GrokModelCatalogSession::new(access_token, user_id, email, client_version)
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
        client_version: impl Into<String>,
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
                client_version,
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
    pub async fn synchronize_instance(
        &self,
        instance: &ProviderInstance,
        config_revision: ConfigRevision,
    ) -> Result<GrokCredentialCatalogSnapshot, GrokCredentialCatalogError> {
        let catalog = self.fetch_and_cache(instance).await?;
        Ok(GrokCredentialCatalogSnapshot {
            config_revision,
            provider_instance_id: catalog.provider_instance_id,
            observed_at: catalog.observed_at,
            accounts: catalog.accounts,
            models: catalog.models,
        })
    }

    /// Provider Registry 构建 RuntimeSnapshot 时使用的实时能力目录。
    pub async fn query_instance_models(
        &self,
        instance: &ProviderInstance,
    ) -> Result<Vec<GrokCatalogModel>, GrokCredentialCatalogError> {
        Ok(self.fetch_and_cache(instance).await?.models)
    }

    async fn fetch_and_cache(
        &self,
        instance: &ProviderInstance,
    ) -> Result<FetchedInstanceCatalog, GrokCredentialCatalogError> {
        let instance_config = GrokBuildProvider::validate_instance(instance)
            .map_err(|_| GrokCredentialCatalogError::InvalidInstance)?;
        let candidates = self
            .repository
            .list_loaded_for_instance(instance_config.id())
            .await
            .map_err(|_| GrokCredentialCatalogError::Store)?
            .into_iter()
            .filter(eligible_catalog_candidate)
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return Err(GrokCredentialCatalogError::NoEligibleCredential);
        }

        let client_version = instance_config.client_version().to_owned();
        let mut fetched = stream::iter(candidates.into_iter().map(|candidate| {
            let client = Arc::clone(&self.client);
            let client_version = client_version.clone();
            async move { fetch_candidate_catalog(client, candidate, client_version).await }
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

        Ok(FetchedInstanceCatalog {
            provider_instance_id: instance_config.id().as_str().to_owned(),
            observed_at,
            accounts,
            models,
        })
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

struct FetchedInstanceCatalog {
    provider_instance_id: String,
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
            | AccountAvailability::QuotaExhausted => true,
            AccountAvailability::Cooldown => {
                account.cooldown_until().is_some_and(|until| until <= now)
            }
            AccountAvailability::Expired
            | AccountAvailability::Banned
            | AccountAvailability::Invalid => false,
        }
}

async fn fetch_candidate_catalog(
    client: Arc<GrokModelCatalogClient>,
    candidate: LoadedGrokCredential,
    client_version: String,
) -> Result<FetchedCredentialCatalog, GrokCredentialCatalogError> {
    let session = GrokModelCatalogSession::new(
        candidate.access_token,
        SecretValue::new(candidate.account.upstream_user_id().to_owned()),
        candidate
            .account
            .email()
            .map(|value| SecretValue::new(value.to_owned())),
        client_version,
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
) -> Result<GrokModelCatalogSession, GrokQuotaError> {
    GrokModelCatalogSession::new(
        loaded.access_token.clone(),
        SecretValue::new(loaded.account.upstream_user_id().to_owned()),
        loaded
            .account
            .email()
            .map(|value| SecretValue::new(value.to_owned())),
        GROK_CLIENT_VERSION,
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
