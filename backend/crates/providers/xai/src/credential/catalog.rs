//! xAI OAuth account 的实时模型目录与可重建 TTL cache 边界

use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};
use std::fmt;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gateway_core::account::{
    CredentialState, OpaqueProviderData, ProviderAccount, ProviderAccountId,
};
use gateway_core::provider_ports::{
    ProviderCatalogCacheKey, ProviderCatalogCachePort, ProviderCatalogScope,
    ProviderRuntimePolicyPort,
};
use gateway_core::routing::{ProviderCatalogGeneration, ProviderKind};

use super::repository::{GrokCredentialRepository, LoadedGrokCredential};
use crate::XaiWireProfileState;
use crate::transport::catalog::{MAX_CATALOG_MODELS, valid_model_slug, validate_etag};
use crate::{
    GrokCatalogModel, GrokModelCatalogClient, GrokModelCatalogSession, GrokModelCatalogSnapshot,
    GrokModelCatalogTransport, SecretValue,
};

const CATALOG_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(5 * 60);
const MAX_CATALOG_FETCH_ATTEMPTS: usize = 3;

/// 官方 `/v1/models` 形成的一个 account 完整模型集合
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

/// xAI 以套餐划分的模型目录作用域
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GrokCatalogScope(ProviderCatalogScope);

impl GrokCatalogScope {
    /// 从账号已验证的套餐事实构造目录作用域
    pub fn for_account(account: &ProviderAccount) -> Result<Self, GrokCatalogCacheError> {
        let plan = account
            .plan_type()
            .map(str::trim)
            .filter(|plan| !plan.is_empty())
            .map(str::to_ascii_lowercase)
            .unwrap_or_else(|| "unknown".to_owned());
        ProviderCatalogScope::new(format!("plan:{plan}"))
            .map(Self)
            .map_err(|_| GrokCatalogCacheError::InvalidData)
    }

    /// 返回稳定的 Provider-owned 作用域
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// Redis/内存 TTL cache 中的一条可重建套餐 catalog
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrokPlanCatalog {
    scope: GrokCatalogScope,
    observed_at: DateTime<Utc>,
    seed: GrokCredentialCatalogSeed,
}

impl GrokPlanCatalog {
    #[must_use]
    pub const fn new(
        scope: GrokCatalogScope,
        observed_at: DateTime<Utc>,
        seed: GrokCredentialCatalogSeed,
    ) -> Self {
        Self {
            scope,
            observed_at,
            seed,
        }
    }

    #[must_use]
    pub const fn scope(&self) -> &GrokCatalogScope {
        &self.scope
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

/// Provider-owned catalog cache；实现只能保存可重建 TTL 数据
#[async_trait]
pub trait GrokCredentialCatalogCache: Send + Sync {
    async fn replace(&self, catalog: GrokPlanCatalog) -> Result<(), GrokCatalogCacheError>;

    async fn read(
        &self,
        scope: &GrokCatalogScope,
    ) -> Result<Option<GrokPlanCatalog>, GrokCatalogCacheError>;

    async fn observed_model_support(
        &self,
        scope: &GrokCatalogScope,
        model: &str,
    ) -> Result<Option<bool>, GrokCatalogCacheError>;
}

/// xAI 负责解释 catalog 文档，Store 只保存 opaque JSON
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

    fn key(&self, scope: &GrokCatalogScope) -> ProviderCatalogCacheKey {
        ProviderCatalogCacheKey::new(self.provider_kind.clone(), scope.0.clone())
    }

    fn encode(catalog: &GrokPlanCatalog) -> OpaqueProviderData {
        let mut document = serde_json::Map::new();
        document.insert("version".to_owned(), serde_json::Value::from(1));
        document.insert(
            "scope".to_owned(),
            serde_json::Value::String(catalog.scope().as_str().to_owned()),
        );
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
        scope: &GrokCatalogScope,
        document: OpaqueProviderData,
    ) -> Result<GrokPlanCatalog, GrokCatalogCacheError> {
        let mut fields = document.into_inner();
        if fields.remove("version").and_then(|value| value.as_u64()) != Some(1) {
            return Err(GrokCatalogCacheError::InvalidData);
        }
        if fields
            .remove("scope")
            .and_then(|value| value.as_str().map(ToOwned::to_owned))
            .as_deref()
            != Some(scope.as_str())
        {
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
        Ok(GrokPlanCatalog::new(scope.clone(), observed_at, seed))
    }
}

#[async_trait]
impl GrokCredentialCatalogCache for GrokCatalogCache {
    async fn replace(&self, catalog: GrokPlanCatalog) -> Result<(), GrokCatalogCacheError> {
        self.port
            .replace(
                &self.key(catalog.scope()),
                &Self::encode(&catalog),
                CATALOG_CACHE_TTL,
            )
            .await
            .map_err(|_| GrokCatalogCacheError::Unavailable)
    }

    async fn read(
        &self,
        scope: &GrokCatalogScope,
    ) -> Result<Option<GrokPlanCatalog>, GrokCatalogCacheError> {
        let Some(document) = self
            .port
            .read(&self.key(scope))
            .await
            .map_err(|_| GrokCatalogCacheError::Unavailable)?
        else {
            return Ok(None);
        };
        // cache 只保存可重建 TTL 数据：损坏条目按 miss 丢弃，由调用方实时重建
        match Self::decode(scope, document) {
            Ok(catalog) => Ok(Some(catalog)),
            Err(_) => {
                tracing::warn!(
                    scope = scope.as_str(),
                    "discarding corrupt xAI model catalog cache entry"
                );
                Ok(None)
            }
        }
    }

    async fn observed_model_support(
        &self,
        scope: &GrokCatalogScope,
        model: &str,
    ) -> Result<Option<bool>, GrokCatalogCacheError> {
        Ok(self
            .read(scope)
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
    #[error("Grok client profile is unavailable")]
    Profile(#[source] gateway_core::error::ErrorSource),
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
    runtime_policy: Arc<dyn ProviderRuntimePolicyPort>,
}

/// 同一目录模型可由多个套餐的成员账号发现
pub(crate) type SourcedCatalogModel = (GrokCatalogModel, BTreeSet<ProviderAccountId>);

#[derive(Default)]
struct PublishedCatalogState {
    generation: u64,
    models: Vec<SourcedCatalogModel>,
}

impl GrokCredentialCatalogService {
    #[must_use]
    pub fn new(
        repository: GrokCredentialRepository,
        transport: Arc<dyn GrokModelCatalogTransport>,
        cache: Arc<dyn GrokCredentialCatalogCache>,
        wire_profile: XaiWireProfileState,
        runtime_policy: Arc<dyn ProviderRuntimePolicyPort>,
    ) -> Self {
        Self {
            repository,
            client: Arc::new(GrokModelCatalogClient::new(transport)),
            cache,
            published: Arc::new(RwLock::new(PublishedCatalogState::default())),
            wire_profile,
            runtime_policy,
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

    /// 优先读取套餐目录 cache；缺失时才用当前账号所属套餐的有限候选集实时填充
    pub async fn cached_or_refresh_account_catalog(
        &self,
        account: &ProviderAccount,
    ) -> Result<GrokPlanCatalog, GrokCredentialCatalogError> {
        let scope = GrokCatalogScope::for_account(account)
            .map_err(|_| GrokCredentialCatalogError::InvalidCredentialData)?;
        if let Some(catalog) = self
            .cache
            .read(&scope)
            .await
            .map_err(|_| GrokCredentialCatalogError::Cache)?
        {
            return Ok(catalog);
        }
        self.refresh_account_catalog(account.id()).await
    }

    /// 实时刷新指定账号所属套餐的模型集合，并覆盖可重建 cache
    pub async fn refresh_account_catalog(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<GrokPlanCatalog, GrokCredentialCatalogError> {
        let candidates = self
            .repository
            .list_loaded_for_provider()
            .await
            .map_err(|_| GrokCredentialCatalogError::Store)?;
        let mut groups = match catalog_candidates_by_scope(candidates) {
            Ok(groups) => groups,
            // 全部候选都被调度列表过滤时按空组处理，让下方目标账号补回继续生效
            Err(GrokCredentialCatalogError::NoEligibleCredential) => BTreeMap::new(),
            Err(error) => return Err(error),
        };
        let scope = groups.iter().find_map(|(scope, candidates)| {
            candidates
                .iter()
                .any(|candidate| candidate.account.id() == account_id)
                .then(|| scope.clone())
        });
        let mut pinned_candidate = None;
        let scope = match scope {
            Some(scope) => scope,
            None => {
                // 常规调度列表不含停用账号；管理端按账号查询模型要对停用账号返回
                // 真实上游结果
                // 按同一 revision 加载账号与凭据，避免并发更新套餐时
                // 把新凭据的目录写入旧套餐 cache
                let account = self
                    .repository
                    .account_by_id(account_id)
                    .await
                    .map_err(|_| GrokCredentialCatalogError::Store)?
                    .ok_or(GrokCredentialCatalogError::NoEligibleCredential)?;
                let loaded = self
                    .repository
                    .load(account_id, account.revision())
                    .await
                    .map_err(|_| GrokCredentialCatalogError::Store)?;
                let scope = GrokCatalogScope::for_account(&loaded.account)
                    .map_err(|_| GrokCredentialCatalogError::InvalidCredentialData)?;
                pinned_candidate = Some(loaded);
                scope
            }
        };
        let mut candidates = groups.remove(&scope).unwrap_or_default();
        if !candidates
            .iter()
            .any(|candidate| candidate.account.id() == account_id)
        {
            let loaded = match pinned_candidate {
                Some(loaded) => loaded,
                None => self
                    .repository
                    .load_current(account_id)
                    .await
                    .map_err(|_| GrokCredentialCatalogError::Store)?,
            };
            candidates.push(loaded);
        }
        candidates.sort_by(|left, right| {
            let left_preferred = left.account.id() == account_id;
            let right_preferred = right.account.id() == account_id;
            right_preferred
                .cmp(&left_preferred)
                .then_with(|| left.account.id().cmp(right.account.id()))
        });
        self.refresh_scope_catalog(scope, candidates).await
    }

    /// 读取当前账号所属套餐的目录 cache，不触发上游请求
    pub async fn read_account_catalog(
        &self,
        account: &ProviderAccount,
    ) -> Result<Option<GrokPlanCatalog>, GrokCredentialCatalogError> {
        let scope = GrokCatalogScope::for_account(account)
            .map_err(|_| GrokCredentialCatalogError::InvalidCredentialData)?;
        self.cache
            .read(&scope)
            .await
            .map_err(|_| GrokCredentialCatalogError::Cache)
    }

    /// 快照同时冻结套餐目录及其成员；来源变化也要求重新发布
    pub async fn query_models(
        &self,
    ) -> Result<Vec<SourcedCatalogModel>, GrokCredentialCatalogError> {
        let candidates = self
            .repository
            .list_loaded_for_provider()
            .await
            .map_err(|_| GrokCredentialCatalogError::Store)?;
        let groups = catalog_candidates_by_scope(candidates)?;
        // 单个套餐失败只跳过该套餐，不阻断其他套餐的目录同步
        let mut fetched = Vec::with_capacity(groups.len());
        let mut last_error = None;
        for (scope, candidates) in groups {
            let accounts = candidates
                .iter()
                .map(|candidate| candidate.account.id().clone())
                .collect();
            match self.fetch_scope_catalog(scope.clone(), candidates).await {
                Ok(catalog) => fetched.push((catalog, accounts)),
                Err(error) => {
                    tracing::warn!(
                        scope = scope.as_str(),
                        error = %error,
                        "skipping failed xAI plan catalog scope"
                    );
                    last_error = Some(error);
                }
            }
        }
        if fetched.is_empty() {
            return Err(last_error.unwrap_or(GrokCredentialCatalogError::NoEligibleCredential));
        }
        let models = strict_model_union(&fetched)?;
        let observed_at = Utc::now();
        for (fetched, _) in fetched {
            let scope = fetched.scope.clone();
            if self
                .cache
                .replace(GrokPlanCatalog {
                    scope: fetched.scope,
                    observed_at,
                    seed: fetched.seed,
                })
                .await
                .is_err()
            {
                // cache 是可重建 TTL 数据，单套餐写入失败不阻断目录发布
                tracing::warn!(
                    scope = scope.as_str(),
                    "failed to cache xAI plan catalog scope"
                );
            }
        }
        self.publish_models(&models);

        Ok(models)
    }

    async fn refresh_scope_catalog(
        &self,
        scope: GrokCatalogScope,
        candidates: Vec<LoadedGrokCredential>,
    ) -> Result<GrokPlanCatalog, GrokCredentialCatalogError> {
        let fetched = self.fetch_scope_catalog(scope.clone(), candidates).await?;
        let catalog = GrokPlanCatalog::new(scope, Utc::now(), fetched.seed);
        self.cache
            .replace(catalog.clone())
            .await
            .map_err(|_| GrokCredentialCatalogError::Cache)?;
        Ok(catalog)
    }

    async fn fetch_scope_catalog(
        &self,
        scope: GrokCatalogScope,
        candidates: Vec<LoadedGrokCredential>,
    ) -> Result<FetchedCredentialCatalog, GrokCredentialCatalogError> {
        let mut last_error = None;
        let profile = XaiWireProfileState::new(
            self.wire_profile
                .resolve_global_request_profile(self.runtime_policy.as_ref())
                .await
                .map_err(GrokCredentialCatalogError::Profile)?,
        );
        for candidate in candidates.into_iter().take(MAX_CATALOG_FETCH_ATTEMPTS) {
            match fetch_candidate_catalog(
                Arc::clone(&self.client),
                scope.clone(),
                candidate,
                profile.clone(),
            )
            .await
            {
                Ok(catalog) => return Ok(catalog),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or(GrokCredentialCatalogError::NoEligibleCredential))
    }

    fn publish_models(&self, models: &[SourcedCatalogModel]) {
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
    scope: GrokCatalogScope,
    snapshot: GrokModelCatalogSnapshot,
    seed: GrokCredentialCatalogSeed,
}

fn catalog_candidates_by_scope(
    candidates: Vec<LoadedGrokCredential>,
) -> Result<BTreeMap<GrokCatalogScope, Vec<LoadedGrokCredential>>, GrokCredentialCatalogError> {
    let mut groups = BTreeMap::<GrokCatalogScope, Vec<LoadedGrokCredential>>::new();
    for candidate in candidates.into_iter().filter(eligible_catalog_candidate) {
        let scope = GrokCatalogScope::for_account(&candidate.account)
            .map_err(|_| GrokCredentialCatalogError::InvalidCredentialData)?;
        groups.entry(scope).or_default().push(candidate);
    }
    for candidates in groups.values_mut() {
        candidates.sort_by(|left, right| left.account.id().cmp(right.account.id()));
    }
    if groups.is_empty() {
        return Err(GrokCredentialCatalogError::NoEligibleCredential);
    }
    Ok(groups)
}

fn eligible_catalog_candidate(candidate: &LoadedGrokCredential) -> bool {
    let account = &candidate.account;
    let now = SystemTime::now();
    account.enabled()
        && account
            .access_token_expires_at()
            .is_some_and(|expires_at| expires_at > now)
        && candidate
            .refresh_token_expires_at
            .is_none_or(|expires_at| expires_at > Utc::now())
        && matches!(
            account.credential_state(),
            CredentialState::Unknown | CredentialState::Ready
        )
}

async fn fetch_candidate_catalog(
    client: Arc<GrokModelCatalogClient>,
    scope: GrokCatalogScope,
    candidate: LoadedGrokCredential,
    wire_profile: XaiWireProfileState,
) -> Result<FetchedCredentialCatalog, GrokCredentialCatalogError> {
    let upstream_user_id = candidate
        .account
        .upstream_user_id()
        .ok_or(GrokCredentialCatalogError::InvalidCredentialData)?;
    let session = GrokModelCatalogSession::new(
        candidate.access_token,
        SecretValue::new(upstream_user_id),
        candidate
            .account
            .email()
            .map(|value| SecretValue::new(value.to_owned())),
        wire_profile,
    )
    .map_err(|_| GrokCredentialCatalogError::InvalidCredentialData)?
    .with_outbound_proxy(candidate.account.outbound_proxy().cloned());
    let snapshot = client
        .fetch(&session)
        .await
        .map_err(|_| GrokCredentialCatalogError::Upstream)?;
    let seed = GrokCredentialCatalogSeed::from_snapshot(&snapshot)?;
    Ok(FetchedCredentialCatalog {
        scope,
        snapshot,
        seed,
    })
}

fn strict_model_union(
    fetched: &[(FetchedCredentialCatalog, BTreeSet<ProviderAccountId>)],
) -> Result<Vec<SourcedCatalogModel>, GrokCredentialCatalogError> {
    let mut union = BTreeMap::<String, SourcedCatalogModel>::new();
    for (credential, accounts) in fetched {
        for model in credential.snapshot.models() {
            let slug = model.request_model().as_str().to_owned();
            match union.entry(slug) {
                Entry::Vacant(entry) => {
                    entry.insert((model.clone(), accounts.clone()));
                }
                Entry::Occupied(mut entry) => {
                    let (existing, sources) = entry.get_mut();
                    if existing != model {
                        return Err(GrokCredentialCatalogError::ConflictingModelFacts);
                    }
                    sources.extend(accounts.iter().cloned());
                }
            }
        }
    }
    if union.is_empty() {
        return Err(GrokCredentialCatalogError::Upstream);
    }
    Ok(union.into_values().collect())
}
