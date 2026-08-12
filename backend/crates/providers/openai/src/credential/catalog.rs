//! Codex 账号 entitlement 的进程内快照，以及套餐级 Redis 模型目录 cache。

use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};
use std::fmt;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use gateway_core::engine::credential::{
    CredentialState, OpaqueProviderData, ProviderAccount, ProviderAccountId,
};
use gateway_core::engine::provider::ProviderCatalogGeneration;
use gateway_core::provider_ports::{
    ProviderCatalogCacheKey, ProviderCatalogCachePort, ProviderCatalogScope,
};
use gateway_core::routing::{ProviderKind, UpstreamModelId};
use secrecy::ExposeSecret;
use thiserror::Error;
use tokio::sync::Notify;
use uuid::Uuid;

use super::agent_identity::CodexAgentIdentityTaskService;
use super::repository::{CodexCredentialRepository, CredentialRepositoryError};
use crate::transport::profile::CodexWireProfileState;
use crate::transport::{CodexBackendClient, CodexCatalogModel, CodexRequestContext};

const MAX_RESPONSE_ETAG_BYTES: usize = 256;
const PLAN_CATALOG_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_CATALOG_FETCH_ATTEMPTS: usize = 3;
const MAX_PLAN_CATALOG_MODELS: usize = 2_048;

/// OpenAI 以套餐划分的模型目录作用域。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CodexCatalogScope(ProviderCatalogScope);

impl CodexCatalogScope {
    /// 从账号已验证的套餐事实构造目录作用域。
    pub fn for_account(account: &ProviderAccount) -> Result<Self, CodexCredentialCatalogError> {
        let plan = account
            .plan_type()
            .map(str::trim)
            .filter(|plan| !plan.is_empty())
            .map(str::to_ascii_lowercase)
            .unwrap_or_else(|| "unknown".to_owned());
        ProviderCatalogScope::new(format!("plan:{plan}"))
            .map(Self)
            .map_err(|_| CodexCredentialCatalogError::InvalidCredentialData)
    }

    /// 返回稳定的 Provider-owned 作用域。
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// Redis TTL cache 中一条可重建的套餐模型目录。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexPlanCatalog {
    scope: CodexCatalogScope,
    observed_at: DateTime<Utc>,
    models: Vec<String>,
}

impl CodexPlanCatalog {
    #[must_use]
    pub const fn new(
        scope: CodexCatalogScope,
        observed_at: DateTime<Utc>,
        models: Vec<String>,
    ) -> Self {
        Self {
            scope,
            observed_at,
            models,
        }
    }

    #[must_use]
    pub const fn scope(&self) -> &CodexCatalogScope {
        &self.scope
    }

    #[must_use]
    pub const fn observed_at(&self) -> DateTime<Utc> {
        self.observed_at
    }

    #[must_use]
    pub fn models(&self) -> &[String] {
        &self.models
    }
}

#[derive(Clone)]
pub struct CodexCredentialCatalogSnapshot {
    observed_at: SystemTime,
    models: Vec<CodexCatalogModel>,
    scope_models: BTreeMap<CodexCatalogScope, Vec<String>>,
}

impl CodexCredentialCatalogSnapshot {
    #[must_use]
    pub const fn observed_at(&self) -> SystemTime {
        self.observed_at
    }

    #[must_use]
    pub fn models(&self) -> &[CodexCatalogModel] {
        &self.models
    }

    pub fn account_models(
        &self,
        account: &ProviderAccount,
    ) -> Result<Option<&[String]>, CodexCredentialCatalogError> {
        let scope = CodexCatalogScope::for_account(account)?;
        Ok(self.scope_models.get(&scope).map(Vec::as_slice))
    }
}

impl fmt::Debug for CodexCredentialCatalogSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexCredentialCatalogSnapshot")
            .field("observed_at", &self.observed_at)
            .field("model_count", &self.models.len())
            .field("scope_count", &self.scope_models.len())
            .finish()
    }
}

#[derive(Debug, Error)]
pub enum CodexCredentialCatalogError {
    #[error("Codex model catalog has no eligible account")]
    NoEligibleCredential,
    #[error("Codex model catalog account data is invalid")]
    InvalidCredentialData,
    #[error("Codex model catalog upstream query failed: {detail}")]
    Upstream { detail: String },
    #[error("Codex model catalog contains conflicting account facts")]
    ConflictingModelFacts,
    #[error("Codex model catalog cache is unavailable")]
    Cache,
    #[error("Codex model catalog ETag is invalid")]
    InvalidEtag,
    #[error("Codex model catalog changed during refresh")]
    ConcurrentUpdate,
}

#[derive(Default)]
struct CatalogEtagState {
    applied: Option<String>,
    pending: Option<String>,
    inflight: Option<String>,
}

#[derive(Default)]
struct CatalogCacheState {
    revision: u64,
    generation: u64,
    snapshot: Option<CodexCredentialCatalogSnapshot>,
}

struct FetchedAccountModels {
    models: Vec<CodexCatalogModel>,
    etag: Option<String>,
}

struct FetchedCatalog {
    snapshot: CodexCredentialCatalogSnapshot,
    etags: Vec<String>,
}

impl From<CredentialRepositoryError> for CodexCredentialCatalogError {
    fn from(_: CredentialRepositoryError) -> Self {
        Self::InvalidCredentialData
    }
}

#[derive(Clone)]
pub struct CodexCredentialCatalogService {
    repository: CodexCredentialRepository,
    profile: CodexWireProfileState,
    http: reqwest::Client,
    base_url: String,
    agent_identity: Arc<CodexAgentIdentityTaskService>,
    plan_catalog_cache: Arc<dyn ProviderCatalogCachePort>,
    cache: Arc<RwLock<CatalogCacheState>>,
    etags: Arc<Mutex<CatalogEtagState>>,
    etag_notification: Arc<Notify>,
}

impl CodexCredentialCatalogService {
    pub fn new(
        repository: CodexCredentialRepository,
        profile: CodexWireProfileState,
        http: reqwest::Client,
        base_url: String,
        agent_identity: Arc<CodexAgentIdentityTaskService>,
        plan_catalog_cache: Arc<dyn ProviderCatalogCachePort>,
    ) -> Self {
        Self {
            repository,
            profile,
            http,
            base_url,
            agent_identity,
            plan_catalog_cache,
            cache: Arc::new(RwLock::new(CatalogCacheState::default())),
            etags: Arc::new(Mutex::new(CatalogEtagState::default())),
            etag_notification: Arc::new(Notify::new()),
        }
    }

    #[must_use]
    pub fn catalog_generation(&self) -> ProviderCatalogGeneration {
        let cache = self
            .cache
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ProviderCatalogGeneration::new(cache.generation)
    }

    pub fn cached(
        &self,
    ) -> Result<Option<CodexCredentialCatalogSnapshot>, CodexCredentialCatalogError> {
        Ok(self
            .cache
            .read()
            .map_err(|_| CodexCredentialCatalogError::Cache)?
            .snapshot
            .as_ref()
            .cloned())
    }

    /// 读取账号当前已验证套餐的模型 entitlement；不触发网络。
    pub fn cached_account_models(
        &self,
        account: &ProviderAccount,
    ) -> Result<Option<Vec<String>>, CodexCredentialCatalogError> {
        let Some(snapshot) = self.cached()? else {
            return Ok(None);
        };
        snapshot
            .account_models(account)
            .map(|models| models.map(<[String]>::to_vec))
    }

    pub fn observed_model_support(
        &self,
        account: &ProviderAccount,
        model: &str,
    ) -> Result<Option<bool>, CodexCredentialCatalogError> {
        let Some(snapshot) = self.cached()? else {
            return Ok(None);
        };
        Ok(Some(snapshot.account_models(account)?.is_some_and(
            |models| models.iter().any(|candidate| candidate == model),
        )))
    }

    /// 优先读取套餐目录 cache；缺失时才用当前账号所属套餐的有限候选集实时填充。
    pub async fn cached_or_refresh_account_catalog(
        &self,
        account: &ProviderAccount,
    ) -> Result<CodexPlanCatalog, CodexCredentialCatalogError> {
        let scope = CodexCatalogScope::for_account(account)?;
        if let Some(catalog) = self.read_plan_catalog(&scope).await? {
            return Ok(catalog);
        }
        self.refresh_account_catalog(account.id()).await
    }

    /// 实时刷新指定账号所属套餐的模型集合，并覆盖可重建 Redis cache。
    pub async fn refresh_account_catalog(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<CodexPlanCatalog, CodexCredentialCatalogError> {
        let account = self
            .repository
            .store()
            .get_account(account_id)
            .await
            .map_err(|_| CodexCredentialCatalogError::InvalidCredentialData)?
            .filter(|account| account.provider().as_str() == "openai")
            .ok_or(CodexCredentialCatalogError::NoEligibleCredential)?;
        let scope = CodexCatalogScope::for_account(&account)?;
        let mut candidates =
            catalog_candidates_by_scope(self.repository.list_for_provider().await?)?
                .remove(&scope)
                .unwrap_or_default();
        if candidates.is_empty() {
            return Err(CodexCredentialCatalogError::NoEligibleCredential);
        }
        candidates.sort_by(|left, right| {
            let left_preferred = left.id() == account_id;
            let right_preferred = right.id() == account_id;
            right_preferred
                .cmp(&left_preferred)
                .then_with(|| left.id().cmp(right.id()))
        });
        let client = CodexBackendClient::new(
            self.http.clone(),
            self.base_url.clone(),
            self.profile.clone(),
        );
        let fetched = self.fetch_scope_models(&client, candidates).await?;
        let catalog = CodexPlanCatalog::new(scope, Utc::now(), model_ids(&fetched.models));
        self.replace_plan_catalog(&catalog).await?;
        Ok(catalog)
    }

    /// 读取当前账号所属套餐的目录 cache，不触发上游请求。
    pub async fn read_account_catalog(
        &self,
        account: &ProviderAccount,
    ) -> Result<Option<CodexPlanCatalog>, CodexCredentialCatalogError> {
        self.read_plan_catalog(&CodexCatalogScope::for_account(account)?)
            .await
    }

    pub async fn synchronize(
        &self,
    ) -> Result<CodexCredentialCatalogSnapshot, CodexCredentialCatalogError> {
        if let Some(cached) = self.cached()? {
            return Ok(cached);
        }
        let cache_revision = self.cache_revision()?;
        let fetched = self.fetch_catalog().await?;
        self.commit_catalog(cache_revision, fetched)
    }

    async fn fetch_catalog(&self) -> Result<FetchedCatalog, CodexCredentialCatalogError> {
        let accounts = self.repository.list_for_provider().await?;
        let groups = catalog_candidates_by_scope(accounts)?;
        let client = CodexBackendClient::new(
            self.http.clone(),
            self.base_url.clone(),
            self.profile.clone(),
        );
        let mut union = BTreeMap::<String, CodexCatalogModel>::new();
        let mut union_order = Vec::new();
        let mut scope_models = BTreeMap::new();
        let mut etags = Vec::new();
        for (scope, candidates) in groups {
            let fetched = self.fetch_scope_models(&client, candidates).await?;
            let entitlement = model_ids(&fetched.models);
            self.replace_plan_catalog(&CodexPlanCatalog::new(
                scope.clone(),
                Utc::now(),
                entitlement.clone(),
            ))
            .await?;
            for model in &fetched.models {
                let id = model.request_model().as_str().to_owned();
                match union.entry(id) {
                    Entry::Vacant(entry) => {
                        union_order.push(entry.key().clone());
                        entry.insert(model.clone());
                    }
                    // 展示与元数据允许跨套餐漂移，union 取首见值；只有路由
                    // 事实冲突才让整轮刷新失败。
                    Entry::Occupied(entry) if conflicting_routing_facts(entry.get(), model) => {
                        return Err(CodexCredentialCatalogError::ConflictingModelFacts);
                    }
                    Entry::Occupied(_) => {}
                }
            }
            scope_models.insert(scope, entitlement);
            etags.extend(fetched.etag);
        }
        let observed_at = SystemTime::now();
        let snapshot = CodexCredentialCatalogSnapshot {
            observed_at,
            models: union_order
                .into_iter()
                .filter_map(|id| union.remove(&id))
                .collect(),
            scope_models,
        };
        Ok(FetchedCatalog { snapshot, etags })
    }

    async fn fetch_scope_models(
        &self,
        client: &CodexBackendClient,
        candidates: Vec<ProviderAccount>,
    ) -> Result<FetchedAccountModels, CodexCredentialCatalogError> {
        let mut last_error = None;
        for account in candidates.into_iter().take(MAX_CATALOG_FETCH_ATTEMPTS) {
            match self.fetch_account_models(client, &account).await {
                Ok(fetched) => return Ok(fetched),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or(CodexCredentialCatalogError::NoEligibleCredential))
    }

    async fn replace_plan_catalog(
        &self,
        catalog: &CodexPlanCatalog,
    ) -> Result<(), CodexCredentialCatalogError> {
        let key = self.plan_catalog_key(catalog.scope())?;
        let document = encode_plan_catalog(catalog);
        self.plan_catalog_cache
            .replace(&key, &document, PLAN_CATALOG_CACHE_TTL)
            .await
            .map_err(|_| CodexCredentialCatalogError::Cache)
    }

    async fn read_plan_catalog(
        &self,
        scope: &CodexCatalogScope,
    ) -> Result<Option<CodexPlanCatalog>, CodexCredentialCatalogError> {
        let key = self.plan_catalog_key(scope)?;
        self.plan_catalog_cache
            .read(&key)
            .await
            .map_err(|_| CodexCredentialCatalogError::Cache)?
            .map(|document| decode_plan_catalog(scope, document))
            .transpose()
    }

    fn plan_catalog_key(
        &self,
        scope: &CodexCatalogScope,
    ) -> Result<ProviderCatalogCacheKey, CodexCredentialCatalogError> {
        let provider_kind =
            ProviderKind::new("openai").map_err(|_| CodexCredentialCatalogError::Cache)?;
        Ok(ProviderCatalogCacheKey::new(provider_kind, scope.0.clone()))
    }

    async fn fetch_account_models(
        &self,
        client: &CodexBackendClient,
        account: &ProviderAccount,
    ) -> Result<FetchedAccountModels, CodexCredentialCatalogError> {
        let mut prepared = self
            .agent_identity
            .prepare(account)
            .await
            .map_err(|_| CodexCredentialCatalogError::InvalidCredentialData)?;
        let mut request_id = format!("catalog_{}", Uuid::now_v7().simple());
        let mut authorization = prepared
            .credential
            .authentication
            .authorization_header(chrono::Utc::now())
            .map_err(|_| CodexCredentialCatalogError::InvalidCredentialData)?;
        let mut result = client
            .fetch_models_with_context(CodexRequestContext::auxiliary(
                authorization.expose_secret(),
                prepared.account.upstream_account_id(),
                &request_id,
                None,
            ))
            .await;
        if let Err(error) = &result
            && let Some(recovered) = self
                .agent_identity
                .recover_after_rejected_task(
                    prepared.account.id(),
                    &prepared.credential.authentication,
                    error,
                )
                .await
                .map_err(|_| CodexCredentialCatalogError::InvalidCredentialData)?
        {
            prepared = recovered;
            request_id = format!("catalog_{}", Uuid::now_v7().simple());
            authorization = prepared
                .credential
                .authentication
                .authorization_header(chrono::Utc::now())
                .map_err(|_| CodexCredentialCatalogError::InvalidCredentialData)?;
            result = client
                .fetch_models_with_context(CodexRequestContext::auxiliary(
                    authorization.expose_secret(),
                    prepared.account.upstream_account_id(),
                    &request_id,
                    None,
                ))
                .await;
        }
        let snapshot = result.map_err(|error| CodexCredentialCatalogError::Upstream {
            detail: error.to_string(),
        })?;
        Ok(FetchedAccountModels {
            models: snapshot.models().to_vec(),
            etag: snapshot.etag().map(str::to_owned),
        })
    }

    pub fn invalidate(&self) -> Result<(), CodexCredentialCatalogError> {
        let mut cache = self
            .cache
            .write()
            .map_err(|_| CodexCredentialCatalogError::Cache)?;
        if cache.snapshot.take().is_some() {
            cache.revision = cache.revision.saturating_add(1);
            cache.generation = cache.generation.saturating_add(1);
        }
        Ok(())
    }

    /// 记录普通 Responses 响应声明的目录版本；相同版本只触发一次。
    pub fn observe_response_etag(&self, etag: &str) -> Result<bool, CodexCredentialCatalogError> {
        validate_response_etag(etag)?;
        let changed = {
            let mut state = self
                .etags
                .lock()
                .map_err(|_| CodexCredentialCatalogError::Cache)?;
            let already_observed = state.applied.as_deref() == Some(etag)
                || state.pending.as_deref() == Some(etag)
                || state.inflight.as_deref() == Some(etag);
            if already_observed {
                false
            } else {
                state.pending = Some(etag.to_owned());
                true
            }
        };
        if changed {
            self.etag_notification.notify_one();
        }
        Ok(changed)
    }

    /// 等待并认领一次需要强制刷新的 Provider 目录。
    pub async fn wait_for_etag_refresh(&self) {
        loop {
            if self.begin_pending_etag_refresh() {
                return;
            }
            self.etag_notification.notified().await;
        }
    }

    /// 立即刷新所有套餐目录，但不接管 ETag daemon 已认领的刷新状态。
    ///
    /// 周期 worker 与 ETag daemon 可以同时请求目录；只有后者可以完成
    /// `inflight` ETag 的状态转换，避免周期刷新错误地确认另一个请求的版本。
    pub async fn refresh_catalogs(
        &self,
    ) -> Result<CodexCredentialCatalogSnapshot, CodexCredentialCatalogError> {
        self.refresh_inner().await
    }

    /// 忽略当前 cache，按已认领的 ETag 变化强制生成一份完整新快照。
    pub async fn refresh(
        &self,
    ) -> Result<CodexCredentialCatalogSnapshot, CodexCredentialCatalogError> {
        let result = self.refresh_inner().await;
        self.finish_etag_refresh(result.is_ok())?;
        result
    }

    async fn refresh_inner(
        &self,
    ) -> Result<CodexCredentialCatalogSnapshot, CodexCredentialCatalogError> {
        let cache_revision = self.cache_revision()?;
        let fetched = self.fetch_catalog().await?;
        self.commit_catalog(cache_revision, fetched)
    }

    fn record_applied_catalog_etags(
        &self,
        etags: impl IntoIterator<Item = String>,
    ) -> Result<(), CodexCredentialCatalogError> {
        let mut distinct = etags.into_iter().collect::<std::collections::BTreeSet<_>>();
        if distinct.len() != 1 {
            return Ok(());
        }
        let Some(etag) = distinct.pop_first() else {
            return Ok(());
        };
        validate_response_etag(&etag)?;
        self.etags
            .lock()
            .map_err(|_| CodexCredentialCatalogError::Cache)?
            .applied = Some(etag);
        Ok(())
    }

    fn begin_pending_etag_refresh(&self) -> bool {
        let Ok(mut state) = self.etags.lock() else {
            return false;
        };
        let Some(pending) = state.pending.take() else {
            return false;
        };
        state.inflight = Some(pending);
        true
    }

    fn finish_etag_refresh(&self, succeeded: bool) -> Result<(), CodexCredentialCatalogError> {
        let should_retry = {
            let mut state = self
                .etags
                .lock()
                .map_err(|_| CodexCredentialCatalogError::Cache)?;
            let Some(etag) = state.inflight.take() else {
                return Ok(());
            };
            if succeeded {
                state.applied = Some(etag);
                false
            } else {
                if state.pending.is_none() {
                    state.pending = Some(etag);
                }
                true
            }
        };
        if should_retry {
            self.etag_notification.notify_one();
        }
        Ok(())
    }

    fn cache_revision(&self) -> Result<u64, CodexCredentialCatalogError> {
        self.cache
            .read()
            .map(|cache| cache.revision)
            .map_err(|_| CodexCredentialCatalogError::Cache)
    }

    fn commit_catalog(
        &self,
        expected_cache_revision: u64,
        fetched: FetchedCatalog,
    ) -> Result<CodexCredentialCatalogSnapshot, CodexCredentialCatalogError> {
        let snapshot = fetched.snapshot;
        let mut cache = self
            .cache
            .write()
            .map_err(|_| CodexCredentialCatalogError::Cache)?;
        if cache.revision != expected_cache_revision {
            return Err(CodexCredentialCatalogError::ConcurrentUpdate);
        }
        let changed = cache
            .snapshot
            .as_ref()
            .is_none_or(|existing| !same_catalog(existing, &snapshot));
        cache.snapshot = Some(snapshot.clone());
        cache.revision = cache.revision.saturating_add(1);
        if changed {
            cache.generation = cache.generation.saturating_add(1);
        }
        drop(cache);
        self.record_applied_catalog_etags(fetched.etags)?;
        Ok(snapshot)
    }
}

fn model_ids(models: &[CodexCatalogModel]) -> Vec<String> {
    models
        .iter()
        .map(|model| model.request_model().as_str().to_owned())
        .collect()
}

/// 路由正确性只依赖 Responses API 支持证据（决定 Generate operation 是否
/// 编译进 `ModelCapabilities`）；display_name/limits/metadata 等展示事实
/// 跨套餐漂移不构成冲突。
fn conflicting_routing_facts(existing: &CodexCatalogModel, candidate: &CodexCatalogModel) -> bool {
    existing.capabilities().responses_api() != candidate.capabilities().responses_api()
}

fn catalog_candidates_by_scope(
    accounts: Vec<ProviderAccount>,
) -> Result<BTreeMap<CodexCatalogScope, Vec<ProviderAccount>>, CodexCredentialCatalogError> {
    let now = SystemTime::now();
    let mut groups = BTreeMap::<CodexCatalogScope, Vec<ProviderAccount>>::new();
    for account in accounts
        .into_iter()
        .filter(|account| eligible_catalog_account(account, now))
    {
        let scope = CodexCatalogScope::for_account(&account)?;
        groups.entry(scope).or_default().push(account);
    }
    for candidates in groups.values_mut() {
        candidates.sort_by(|left, right| left.id().cmp(right.id()));
    }
    if groups.is_empty() {
        return Err(CodexCredentialCatalogError::NoEligibleCredential);
    }
    Ok(groups)
}

fn encode_plan_catalog(catalog: &CodexPlanCatalog) -> OpaqueProviderData {
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
    document.insert(
        "models".to_owned(),
        serde_json::Value::Array(
            catalog
                .models()
                .iter()
                .cloned()
                .map(serde_json::Value::String)
                .collect(),
        ),
    );
    OpaqueProviderData::new(document)
}

fn decode_plan_catalog(
    scope: &CodexCatalogScope,
    document: OpaqueProviderData,
) -> Result<CodexPlanCatalog, CodexCredentialCatalogError> {
    let mut fields = document.into_inner();
    if fields.remove("version").and_then(|value| value.as_u64()) != Some(1) {
        return Err(CodexCredentialCatalogError::Cache);
    }
    if fields
        .remove("scope")
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .as_deref()
        != Some(scope.as_str())
    {
        return Err(CodexCredentialCatalogError::Cache);
    }
    let observed_at = fields
        .remove("observedAt")
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .and_then(|value| DateTime::parse_from_rfc3339(&value).ok())
        .map(|value| value.with_timezone(&Utc))
        .ok_or(CodexCredentialCatalogError::Cache)?;
    let values = fields
        .remove("models")
        .and_then(|value| value.as_array().cloned())
        .filter(|models| !models.is_empty() && models.len() <= MAX_PLAN_CATALOG_MODELS)
        .ok_or(CodexCredentialCatalogError::Cache)?;
    if !fields.is_empty() {
        return Err(CodexCredentialCatalogError::Cache);
    }
    let mut seen = BTreeSet::new();
    let mut models = Vec::with_capacity(values.len());
    for value in values {
        let value = value
            .as_str()
            .map(ToOwned::to_owned)
            .ok_or(CodexCredentialCatalogError::Cache)?;
        let model = UpstreamModelId::new(value).map_err(|_| CodexCredentialCatalogError::Cache)?;
        if !seen.insert(model.as_str().to_owned()) {
            return Err(CodexCredentialCatalogError::Cache);
        }
        models.push(model.as_str().to_owned());
    }
    Ok(CodexPlanCatalog::new(scope.clone(), observed_at, models))
}

fn same_catalog(
    left: &CodexCredentialCatalogSnapshot,
    right: &CodexCredentialCatalogSnapshot,
) -> bool {
    left.models == right.models && left.scope_models == right.scope_models
}

fn validate_response_etag(etag: &str) -> Result<(), CodexCredentialCatalogError> {
    if etag.is_empty() || etag.len() > MAX_RESPONSE_ETAG_BYTES || etag.chars().any(char::is_control)
    {
        return Err(CodexCredentialCatalogError::InvalidEtag);
    }
    Ok(())
}

fn eligible_catalog_account(account: &ProviderAccount, now: SystemTime) -> bool {
    account.enabled()
        && account
            .access_token_expires_at()
            .is_none_or(|expires_at| expires_at > now)
        && matches!(
            account.credential_state(),
            CredentialState::Unknown | CredentialState::Ready
        )
}
