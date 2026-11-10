//! OpenAI 账号模型目录的聚合、缓存与调度资格

use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};
use std::fmt;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use futures::{StreamExt, stream};
use gateway_core::account::{
    CredentialRevision, CredentialState, OpaqueProviderData, ProviderAccount, ProviderAccountId,
};
use gateway_core::policy::ClientApiKeyId;
use gateway_core::provider_ports::{
    ProviderCatalogCacheKey, ProviderCatalogCachePort, ProviderCatalogScope,
};
use gateway_core::routing::{
    FrozenAccountScope, ModelPresentation, ProviderCatalogGeneration, ProviderKind,
    ProviderModelContent, ProviderModelDescriptor, UpstreamModelId,
};
use secrecy::ExposeSecret;
use thiserror::Error;
use tokio::sync::{Notify, OnceCell};
use tokio::time::Instant;
use uuid::Uuid;

use super::repository::{CodexCredentialRepository, CredentialRepositoryError};
use crate::transport::profile::{CodexWireProfile, CodexWireProfileState};
use crate::transport::{CodexBackendClient, CodexCatalogModel, CodexRequestContext};

const MAX_RESPONSE_ETAG_BYTES: usize = 256;
const ACCOUNT_CATALOG_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
// 调度证据覆盖默认 15 分钟刷新周期及抖动，长期未刷新时恢复上游验证
const SCHEDULING_CATALOG_MAX_AGE: Duration = Duration::from_secs(30 * 60);
const MAX_ACCOUNT_CATALOG_MODELS: usize = 2_048;
const MAX_CLIENT_CATALOGS: usize = 32;
// 保持原先 32 份、每份 1 MiB 的原生正文缓存预算，不把缓存容量变成上游目录合同
const MAX_CLIENT_CATALOG_CACHE_BYTES: usize = 32 * 1024 * 1024;
const MODEL_CATALOG_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_CONCURRENT_CATALOGS: usize = 4;
const CAPABILITY_LOOKUP_BUDGET: Duration = Duration::from_millis(500);
const CLIENT_CATALOG_ERROR_TTL: Duration = Duration::from_secs(5);

/// 同一套餐也可能按账号开放模型，换凭据或套餐后必须重新取证
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CodexCatalogScope {
    account_id: ProviderAccountId,
    revision: CredentialRevision,
    plan: Option<String>,
}

impl CodexCatalogScope {
    #[must_use]
    pub fn for_account(account: &ProviderAccount) -> Self {
        Self {
            account_id: account.id().clone(),
            revision: account.revision(),
            plan: account
                .plan_type()
                .map(str::trim)
                .filter(|plan| !plan.is_empty())
                .map(str::to_ascii_lowercase),
        }
    }

    /// Redis 作用域长度有上限，不直接拼接可变长度的账号与套餐文本
    pub fn cache_scope(&self) -> Result<ProviderCatalogScope, CodexCredentialCatalogError> {
        use sha2::{Digest, Sha256};
        let mut digest = Sha256::new();
        digest.update(self.account_id.as_str());
        digest.update([0]);
        digest.update(self.plan.as_deref().unwrap_or_default());
        ProviderCatalogScope::new(format!(
            "account:{}:{}",
            hex::encode(digest.finalize()),
            self.revision.get()
        ))
        .map_err(|_| CodexCredentialCatalogError::Cache)
    }
}

/// Redis TTL cache 中一条可重建的账号模型目录
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexAccountCatalog {
    scope: ProviderCatalogScope,
    observed_at: DateTime<Utc>,
    models: Vec<String>,
}

impl CodexAccountCatalog {
    #[must_use]
    pub const fn new(
        scope: ProviderCatalogScope,
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
    pub const fn scope(&self) -> &ProviderCatalogScope {
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
    scope_catalogs: BTreeMap<CodexCatalogScope, Vec<String>>,
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

    pub fn account_models(&self, account: &ProviderAccount) -> Option<&[String]> {
        self.scope_catalogs
            .get(&CodexCatalogScope::for_account(account))
            .map(Vec::as_slice)
    }

    /// 将 Provider 私有目录作用域投影为模型来源账号，供 Core 结合冻结政策过滤
    #[must_use]
    pub fn model_catalog_accounts(&self) -> BTreeMap<&str, BTreeSet<ProviderAccountId>> {
        let mut accounts = BTreeMap::<&str, BTreeSet<ProviderAccountId>>::new();
        for (scope, models) in &self.scope_catalogs {
            for model in models {
                accounts
                    .entry(model.as_str())
                    .or_default()
                    .insert(scope.account_id.clone());
            }
        }
        accounts
    }
}

impl fmt::Debug for CodexCredentialCatalogSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexCredentialCatalogSnapshot")
            .field("observed_at", &self.observed_at)
            .field("model_count", &self.models.len())
            .field("scope_count", &self.scope_catalogs.len())
            .finish()
    }
}

#[derive(Debug, Clone, Error)]
pub enum CodexCredentialCatalogError {
    #[error("Codex model catalog has no eligible account")]
    NoEligibleCredential,
    #[error("Codex model catalog account data is invalid")]
    InvalidCredentialData,
    #[error("Codex model catalog upstream query failed: {detail}")]
    Upstream { detail: String },
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

#[derive(PartialEq, Eq)]
struct ClientCatalogKey {
    client_key_id: Option<ClientApiKeyId>,
    account_id: ProviderAccountId,
    revision: CredentialRevision,
    plan: Option<String>,
    upstream_account_id: Option<String>,
    client_version: String,
    profile: CodexWireProfile,
}

type ClientCatalogResult = Result<Vec<ProviderModelDescriptor>, CodexCredentialCatalogError>;
type AccountModelsResult = Result<Arc<[CodexCatalogModel]>, CodexCredentialCatalogError>;

struct CachedClientCatalog {
    // 负缓存从请求结束计时，不能让一次慢失败耗尽后续请求的退避窗口
    completed_at: Instant,
    result: AccountModelsResult,
}

impl CachedClientCatalog {
    fn native_bytes(&self) -> usize {
        self.result.as_ref().map_or(0, |models| {
            models
                .iter()
                .map(|model| model.document().body().len())
                .sum()
        })
    }
}

struct ClientCatalogEntry {
    key: ClientCatalogKey,
    created_at: Instant,
    value: Arc<OnceCell<CachedClientCatalog>>,
}

impl ClientCatalogEntry {
    fn is_fresh(&self) -> bool {
        let Some(value) = self.value.get() else {
            // 最后一位读者取消后 OnceCell 不再初始化，不能继续占用缓存名额
            return Arc::strong_count(&self.value) > 1
                && self.created_at.elapsed() < ACCOUNT_CATALOG_CACHE_TTL;
        };
        let ttl = if value.result.is_err() {
            CLIENT_CATALOG_ERROR_TTL
        } else {
            ACCOUNT_CATALOG_CACHE_TTL
        };
        value.completed_at.elapsed() < ttl
    }
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
    account_catalog_cache: Arc<dyn ProviderCatalogCachePort>,
    cache: Arc<RwLock<CatalogCacheState>>,
    client_catalogs: Arc<Mutex<Vec<ClientCatalogEntry>>>,
    background_capabilities: Arc<tokio::sync::Semaphore>,
    etags: Arc<Mutex<CatalogEtagState>>,
    etag_notification: Arc<Notify>,
}

impl CodexCredentialCatalogService {
    pub fn new(
        repository: CodexCredentialRepository,
        profile: CodexWireProfileState,
        http: reqwest::Client,
        base_url: String,
        account_catalog_cache: Arc<dyn ProviderCatalogCachePort>,
    ) -> Self {
        Self {
            repository,
            profile,
            http,
            base_url,
            account_catalog_cache,
            cache: Arc::new(RwLock::new(CatalogCacheState::default())),
            client_catalogs: Arc::new(Mutex::new(Vec::new())),
            background_capabilities: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_CATALOGS)),
            etags: Arc::new(Mutex::new(CatalogEtagState::default())),
            etag_notification: Arc::new(Notify::new()),
        }
    }

    /// 聚合 Key 范围内各账号允许的模型，保留同名模型的首选完整对象
    /// 同一账号/凭据版本/客户端版本的并发读取合并；原生正文仅保留在有界进程缓存
    pub async fn client_model_catalog(
        &self,
        client_key_id: &ClientApiKeyId,
        scope: &FrozenAccountScope,
        client_version: &str,
    ) -> ClientCatalogResult {
        let now = SystemTime::now();
        let provider = gateway_core::routing::ProviderKind::new("openai")
            .map_err(|_| CodexCredentialCatalogError::InvalidCredentialData)?;
        let request_profile = match scope.request_profile(&provider) {
            Some(configuration) => {
                crate::transport::profile::identity::RequestProfileSelection::parse(configuration)
                    .and_then(|selection| selection.resolve(&self.profile))
                    .map_err(|_| CodexCredentialCatalogError::InvalidCredentialData)?
            }
            None => self.profile.snapshot(),
        };
        let account_ids = scope
            .allowed_account_ids()
            .filter(|id| scope.account_provider(id) == Some(&provider))
            .cloned()
            .collect::<Vec<_>>();
        let mut accounts = self.repository.list_for_accounts(&account_ids).await?;
        accounts.retain(|account| {
            scope.allows_current_account(account) && eligible_catalog_account(account, now)
        });
        accounts.sort_by(|left, right| left.id().cmp(right.id()));
        let (oauth, api): (Vec<_>, Vec<_>) = accounts
            .into_iter()
            .enumerate()
            .filter(|(_, account)| {
                matches!(
                    account.authentication_kind(),
                    super::CODEX_AUTHENTICATION_KIND_OAUTH
                        | super::CODEX_AUTHENTICATION_KIND_API_KEY
                )
            })
            .partition(|(_, account)| {
                account.authentication_kind() == super::CODEX_AUTHENTICATION_KIND_OAUTH
            });
        let read = |(index, account): (usize, ProviderAccount)| {
            let profile = request_profile.clone();
            async move {
                let result = self
                    .cached_client_account_catalog(client_key_id, &account, client_version, profile)
                    .await;
                (index, account, result)
            }
        };
        // 两类上游独立限并发，共用一个期限，慢上游不占用另一类账号的读取名额
        let mut catalogs = stream::select(
            stream::iter(oauth)
                .map(read)
                .buffer_unordered(MAX_CONCURRENT_CATALOGS),
            stream::iter(api)
                .map(read)
                .buffer_unordered(MAX_CONCURRENT_CATALOGS),
        );
        let deadline = Instant::now() + MODEL_CATALOG_TIMEOUT;
        let mut selected = BTreeMap::new();
        let mut available = false;
        let mut last_error = CodexCredentialCatalogError::NoEligibleCredential;
        loop {
            let (index, account, models) =
                match tokio::time::timeout_at(deadline, catalogs.next()).await {
                    Ok(Some((index, account, Ok(models)))) => (index, account, models),
                    Ok(Some((_, _, Err(error)))) => {
                        last_error = error;
                        continue;
                    }
                    Ok(None) => break,
                    Err(_) => {
                        last_error = CodexCredentialCatalogError::Upstream {
                            detail: "model catalog timed out".to_owned(),
                        };
                        break;
                    }
                };
            available = true;
            let api = account.authentication_kind() == super::CODEX_AUTHENTICATION_KIND_API_KEY;
            for (position, model) in models.into_iter().enumerate() {
                if !scope.allows_model(account.id(), model.model.as_str()) {
                    continue;
                }
                // OAuth、API 原生、普通 ID 依次优先；响应快慢不改变来源或条目顺序
                let order = (
                    api,
                    matches!(model.content, ProviderModelContent::Adapted(_)),
                    index,
                    position,
                );
                match selected.entry(model.model.clone()) {
                    Entry::Vacant(entry) => {
                        entry.insert((order, model));
                    }
                    Entry::Occupied(mut entry) if order < entry.get().0 => {
                        entry.insert((order, model));
                    }
                    Entry::Occupied(_) => {}
                }
            }
        }
        if !available {
            return Err(last_error);
        }
        let mut models: Vec<_> = selected.into_values().collect();
        models.sort_unstable_by_key(|(order, _)| *order);
        Ok(models.into_iter().map(|(_, model)| model).collect())
    }

    async fn cached_client_account_catalog(
        &self,
        client_key_id: &ClientApiKeyId,
        account: &ProviderAccount,
        client_version: &str,
        profile: CodexWireProfile,
    ) -> ClientCatalogResult {
        self.cached_account_catalog(account, Some((client_key_id, client_version)), profile)
            .await
            .map(|models| {
                models
                    .iter()
                    .map(|model| ProviderModelDescriptor {
                        model: model.request_model().clone(),
                        content: if model.document().protocol() == "codex" {
                            ProviderModelContent::Native(model.document().clone())
                        } else {
                            let model_id = model.request_model().as_str();
                            ProviderModelContent::Adapted(
                                ModelPresentation::new(Some(model_id.to_owned()), None)
                                    .with_agent_tools(true, false)
                                    .with_hidden(is_specialized_api_model(model_id)),
                            )
                        },
                    })
                    .collect()
            })
    }

    /// 在既有账号、revision 隔离及负缓存边界内读取 Messages 转换能力
    pub(crate) async fn messages_model_capabilities(
        &self,
        account: &ProviderAccount,
        model: &UpstreamModelId,
    ) -> Result<
        Option<gateway_protocol::anthropic::MessagesModelCapabilities>,
        CodexCredentialCatalogError,
    > {
        let service = self.clone();
        let account = account.clone();
        let mut lookup = Box::pin(async move {
            service
                .cached_account_catalog(&account, None, service.profile.snapshot())
                .await
        });
        let models = match tokio::time::timeout(CAPABILITY_LOOKUP_BUDGET, &mut lookup).await {
            Ok(result) => result?,
            Err(_) => {
                // 目录不阻塞本轮生成；保留同一查询及负缓存，慢上游完成后供后续轮次使用
                if let Ok(permit) = self.background_capabilities.clone().try_acquire_owned() {
                    tokio::spawn(async move {
                        let _permit = permit;
                        let _ = lookup.await;
                    });
                }
                return Ok(None);
            }
        };
        Ok(models
            .iter()
            .find(|entry| entry.request_model() == model)
            .and_then(CodexCatalogModel::messages_capabilities)
            .cloned())
    }

    async fn cached_account_catalog(
        &self,
        account: &ProviderAccount,
        identity: Option<(&ClientApiKeyId, &str)>,
        profile: CodexWireProfile,
    ) -> AccountModelsResult {
        let key = ClientCatalogKey {
            // 无种子时沿用账号级目录缓存，启用后按 Key 隔离出站身份
            client_key_id: profile
                .installation_seed
                .as_ref()
                .and(identity.map(|(key, _)| key.clone())),
            account_id: account.id().clone(),
            revision: account.revision(),
            plan: account.plan_type().map(str::to_owned),
            upstream_account_id: account.upstream_account_id().map(str::to_owned),
            client_version: identity.map_or("", |(_, version)| version).to_owned(),
            profile: profile.clone(),
        };
        let value = {
            let mut cache = self
                .client_catalogs
                .lock()
                .map_err(|_| CodexCredentialCatalogError::Cache)?;
            cache.retain(ClientCatalogEntry::is_fresh);
            if let Some(entry) = cache.iter().find(|entry| entry.key == key) {
                Arc::clone(&entry.value)
            } else {
                // 不驱逐正在读取的目录，避免并发请求绕过缓存容量限制制造额外出站
                if cache.len() >= MAX_CLIENT_CATALOGS {
                    let Some(index) = cache.iter().position(|entry| entry.value.initialized())
                    else {
                        return Err(CodexCredentialCatalogError::Cache);
                    };
                    cache.remove(index);
                }
                let value = Arc::new(OnceCell::new());
                cache.push(ClientCatalogEntry {
                    key,
                    created_at: Instant::now(),
                    value: Arc::clone(&value),
                });
                value
            }
        };
        let result = value
            .get_or_init(|| async {
                // 冻结画像，确保实际请求与缓存身份一致；不传递客户端 Bearer 给上游
                let client = CodexBackendClient::new(
                    self.http.clone(),
                    self.base_url.clone(),
                    CodexWireProfileState::new(profile),
                );
                let result = self
                    .fetch_account_models(&client, account, identity)
                    .await
                    .map(|fetched| Arc::from(fetched.models));
                CachedClientCatalog {
                    completed_at: Instant::now(),
                    result,
                }
            })
            .await;
        // 完成中的 OnceCell 仍由并发读者持有；移出缓存只影响后续查询，不丢弃本次成功结果
        let mut cache = self
            .client_catalogs
            .lock()
            .map_err(|_| CodexCredentialCatalogError::Cache)?;
        if result.native_bytes() > MAX_CLIENT_CATALOG_CACHE_BYTES {
            cache.retain(|entry| !Arc::ptr_eq(&entry.value, &value));
        } else {
            let mut bytes: usize = cache
                .iter()
                .filter_map(|entry| entry.value.get())
                .map(CachedClientCatalog::native_bytes)
                .sum();
            while bytes > MAX_CLIENT_CATALOG_CACHE_BYTES {
                let Some(index) = cache.iter().position(|entry| entry.value.initialized()) else {
                    break;
                };
                let entry = cache.remove(index);
                bytes -= entry
                    .value
                    .get()
                    .map_or(0, CachedClientCatalog::native_bytes);
            }
        }
        result.result.clone()
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

    /// 读取账号当前凭据对应的模型目录；不触发网络
    pub fn cached_account_models(
        &self,
        account: &ProviderAccount,
    ) -> Result<Option<Vec<String>>, CodexCredentialCatalogError> {
        let Some(snapshot) = self.cached()? else {
            return Ok(None);
        };
        Ok(snapshot.account_models(account).map(<[String]>::to_vec))
    }

    /// 有效目录约束账号模型资格，没有该账号的有效目录时交由上游判断
    pub(crate) fn scheduling_exclusions(
        &self,
        accounts: &[ProviderAccount],
        model: &str,
        now: SystemTime,
    ) -> BTreeSet<ProviderAccountId> {
        let Ok(cache) = self.cache.read() else {
            return BTreeSet::new();
        };
        let Some(snapshot) = cache.snapshot.as_ref().filter(|snapshot| {
            now.duration_since(snapshot.observed_at)
                .is_ok_and(|age| age < SCHEDULING_CATALOG_MAX_AGE)
        }) else {
            return BTreeSet::new();
        };
        accounts
            .iter()
            .filter(|account| {
                snapshot
                    .account_models(account)
                    .is_some_and(|models| !models.iter().any(|id| id == model))
            })
            .map(|account| account.id().clone())
            .collect()
    }

    /// 优先读取账号目录缓存；缺失时只查询该账号
    pub async fn cached_or_refresh_account_catalog(
        &self,
        account: &ProviderAccount,
    ) -> Result<CodexAccountCatalog, CodexCredentialCatalogError> {
        let scope = CodexCatalogScope::for_account(account).cache_scope()?;
        if let Some(catalog) = self.read_cached_catalog(&scope).await? {
            return Ok(catalog);
        }
        self.refresh_account_catalog(account.id()).await
    }

    /// 实时刷新指定账号的模型集合，包括管理员显式查询的停用账号
    pub async fn refresh_account_catalog(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<CodexAccountCatalog, CodexCredentialCatalogError> {
        let account = self
            .repository
            .store()
            .get_account(account_id)
            .await
            .map_err(|_| CodexCredentialCatalogError::InvalidCredentialData)?
            .filter(|account| account.provider().as_str() == "openai")
            .ok_or(CodexCredentialCatalogError::NoEligibleCredential)?;
        let scope = CodexCatalogScope::for_account(&account).cache_scope()?;
        let client = CodexBackendClient::new(
            self.http.clone(),
            self.base_url.clone(),
            self.profile.clone(),
        );
        let fetched = self.fetch_account_models(&client, &account, None).await?;
        let catalog = CodexAccountCatalog::new(scope, Utc::now(), model_ids(&fetched.models));
        self.replace_account_catalog(&catalog).await?;
        Ok(catalog)
    }

    /// 读取目标账号的原生目录条目；供管理端按账号导出目录文件使用
    ///
    /// 按账号直接请求上游，避免其他账号的同名模型替换目标账号的原生对象；
    /// 停用账号也不在常规快照候选中，仍允许管理员显式导出
    /// 返回顺序沿用上游目录顺序，条目内容保持上游原样，不做别名改写或字段裁剪
    pub async fn account_catalog_documents(
        &self,
        account: &ProviderAccount,
    ) -> Result<(Vec<CodexCatalogModel>, SystemTime), CodexCredentialCatalogError> {
        let client = CodexBackendClient::new(
            self.http.clone(),
            self.base_url.clone(),
            self.profile.clone(),
        );
        let fetched = self.fetch_account_models(&client, account, None).await?;
        if fetched.models.is_empty() {
            return Err(CodexCredentialCatalogError::NoEligibleCredential);
        }
        Ok((fetched.models, SystemTime::now()))
    }

    /// 读取当前账号的目录缓存，不触发上游请求
    pub async fn read_account_catalog(
        &self,
        account: &ProviderAccount,
    ) -> Result<Option<CodexAccountCatalog>, CodexCredentialCatalogError> {
        self.read_cached_catalog(&CodexCatalogScope::for_account(account).cache_scope()?)
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
        let mut accounts = self.repository.list_for_provider().await?;
        let now = SystemTime::now();
        accounts.retain(|account| eligible_catalog_account(account, now));
        if accounts.is_empty() {
            return Err(CodexCredentialCatalogError::NoEligibleCredential);
        }
        accounts.sort_by(|a, b| a.id().cmp(b.id()));
        let client = CodexBackendClient::new(
            self.http.clone(),
            self.base_url.clone(),
            self.profile.clone(),
        );
        let mut union = BTreeMap::<String, CodexCatalogModel>::new();
        let mut union_order = Vec::new();
        let mut scope_catalogs = BTreeMap::new();
        let mut etags = Vec::new();
        let mut last_error = None;
        let client = &client;
        let mut catalogs = stream::iter(accounts)
            .map(|account| async move {
                let result = self.fetch_account_models(client, &account, None).await;
                (account, result)
            })
            .buffered(MAX_CONCURRENT_CATALOGS);
        while let Some((account, result)) = catalogs.next().await {
            let fetched = match result {
                Ok(fetched) => fetched,
                Err(error) => {
                    last_error = Some(error);
                    continue;
                }
            };
            let scope = CodexCatalogScope::for_account(&account);
            let entitlement = model_ids(&fetched.models);
            self.replace_account_catalog(&CodexAccountCatalog::new(
                scope.cache_scope()?,
                Utc::now(),
                entitlement.clone(),
            ))
            .await?;
            for model in fetched.models {
                let id = model.request_model().as_str().to_owned();
                match union.entry(id) {
                    Entry::Vacant(entry) => {
                        union_order.push(entry.key().clone());
                        entry.insert(model);
                    }
                    Entry::Occupied(mut entry) => {
                        // 原生目录含完整能力证据，同名 API ID 不能覆盖它，也不能跨来源拼字段
                        if model.document().protocol() == "codex"
                            && entry.get().document().protocol() != "codex"
                        {
                            entry.insert(model);
                        }
                    }
                }
            }
            scope_catalogs.insert(scope, entitlement);
            etags.extend(fetched.etag);
        }
        if union.is_empty()
            && let Some(error) = last_error
        {
            return Err(error);
        }
        let observed_at = SystemTime::now();
        let snapshot = CodexCredentialCatalogSnapshot {
            observed_at,
            models: union_order
                .into_iter()
                .filter_map(|id| union.remove(&id))
                .collect(),
            scope_catalogs,
        };
        Ok(FetchedCatalog { snapshot, etags })
    }

    async fn replace_account_catalog(
        &self,
        catalog: &CodexAccountCatalog,
    ) -> Result<(), CodexCredentialCatalogError> {
        let key = self.account_catalog_key(catalog.scope())?;
        let document = encode_account_catalog(catalog);
        self.account_catalog_cache
            .replace(&key, &document, ACCOUNT_CATALOG_CACHE_TTL)
            .await
            .map_err(|_| CodexCredentialCatalogError::Cache)
    }

    async fn read_cached_catalog(
        &self,
        scope: &ProviderCatalogScope,
    ) -> Result<Option<CodexAccountCatalog>, CodexCredentialCatalogError> {
        let key = self.account_catalog_key(scope)?;
        self.account_catalog_cache
            .read(&key)
            .await
            .map_err(|_| CodexCredentialCatalogError::Cache)?
            .map(|document| decode_account_catalog(scope, document))
            .transpose()
    }

    fn account_catalog_key(
        &self,
        scope: &ProviderCatalogScope,
    ) -> Result<ProviderCatalogCacheKey, CodexCredentialCatalogError> {
        let provider_kind =
            ProviderKind::new("openai").map_err(|_| CodexCredentialCatalogError::Cache)?;
        Ok(ProviderCatalogCacheKey::new(provider_kind, scope.clone()))
    }

    async fn fetch_account_models(
        &self,
        client: &CodexBackendClient,
        account: &ProviderAccount,
        client_identity: Option<(&ClientApiKeyId, &str)>,
    ) -> Result<FetchedAccountModels, CodexCredentialCatalogError> {
        tokio::time::timeout(MODEL_CATALOG_TIMEOUT, async {
            let credential = self
                .repository
                .load_runtime_credential(account)
                .await
                .map_err(|_| CodexCredentialCatalogError::InvalidCredentialData)?;
            let installation_id = client_identity.map(|(key_id, _)| {
                client.profile_state().effective_installation_id(
                    key_id,
                    account.id(),
                    &credential.installation_id,
                )
            });
            let client_version = client_identity.map(|(_, version)| version);
            let request_id = format!("catalog_{}", Uuid::now_v7().simple());
            let authorization = credential
                .authentication
                .authorization_header()
                .map_err(|_| CodexCredentialCatalogError::InvalidCredentialData)?;
            let mut client = client
                .for_account(account)
                .map_err(|error| CodexCredentialCatalogError::Upstream {
                    detail: error.to_string(),
                })?
                .with_responses_api_base_url(credential.authentication.responses_api_base_url());
            if let super::CodexRuntimeAuthentication::ApiKey(auth) = &credential.authentication
                && auth.configuration.protocol == super::ApiKeyProtocol::Messages
            {
                client = client.with_messages_api_base_url(&auth.configuration.base_url);
            }
            let result = client
                .fetch_models_with_context(
                    CodexRequestContext::auxiliary(
                        authorization.expose_secret(),
                        account.upstream_account_id(),
                        &request_id,
                        installation_id.as_deref(),
                    ),
                    client_version,
                )
                .await;
            let snapshot = result.map_err(|error| CodexCredentialCatalogError::Upstream {
                detail: error.to_string(),
            })?;
            Ok(FetchedAccountModels {
                models: snapshot.models().to_vec(),
                etag: snapshot.etag().map(str::to_owned),
            })
        })
        .await
        .map_err(|_| CodexCredentialCatalogError::Upstream {
            detail: "model catalog timed out".to_owned(),
        })?
    }

    pub fn invalidate(&self) -> Result<(), CodexCredentialCatalogError> {
        let mut cache = self
            .cache
            .write()
            .map_err(|_| CodexCredentialCatalogError::Cache)?;
        if cache.snapshot.take().is_some() {
            cache.generation = cache.generation.saturating_add(1);
        }
        cache.revision = cache.revision.saturating_add(1);
        drop(cache);
        self.invalidate_client_catalogs()?;
        Ok(())
    }

    fn invalidate_client_catalogs(&self) -> Result<(), CodexCredentialCatalogError> {
        self.client_catalogs
            .lock()
            .map_err(|_| CodexCredentialCatalogError::Cache)?
            .clear();
        Ok(())
    }

    /// 记录普通 Responses 响应声明的目录版本；相同版本只触发一次
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
            self.invalidate_client_catalogs()?;
            self.etag_notification.notify_one();
        }
        Ok(changed)
    }

    /// 等待并认领一次需要强制刷新的 Provider 目录
    pub async fn wait_for_etag_refresh(&self) {
        loop {
            if self.begin_pending_etag_refresh() {
                return;
            }
            self.etag_notification.notified().await;
        }
    }

    /// 立即刷新所有账号目录，但不接管 ETag daemon 已认领的刷新状态
    ///
    /// 周期 worker 与 ETag daemon 可以同时请求目录；只有后者可以完成
    /// `inflight` ETag 的状态转换，避免周期刷新错误地确认另一个请求的版本
    pub async fn refresh_catalogs(
        &self,
    ) -> Result<CodexCredentialCatalogSnapshot, CodexCredentialCatalogError> {
        self.refresh_inner().await
    }

    /// 忽略当前 cache，按已认领的 ETag 变化强制生成一份完整新快照
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
        if changed {
            self.invalidate_client_catalogs()?;
        }
        self.record_applied_catalog_etags(fetched.etags)?;
        Ok(snapshot)
    }
}

fn is_specialized_api_model(model: &str) -> bool {
    // 普通目录不提供 Codex 可见性；仅按已知的专用模型 ID 隐藏，保留未知 ID 和直接调用
    // 依据 https://developers.openai.com/api/docs/models 与 Codex 的语音通话模型
    matches!(
        model,
        "dall-e-2"
            | "dall-e-3"
            | "chatgpt-image-latest"
            | "gpt-image-1"
            | "gpt-image-1-mini"
            | "gpt-image-1.5"
            | "gpt-image-1.5-2025-12-16"
            | "gpt-image-2"
            | "gpt-image-2-2026-04-21"
            | "gpt-image-2.5-flare"
            | "gpt-image-2.5-flare-2026-09-08"
            | "gpt-image-2.5-sunburst"
            | "gpt-image-2.5-sunburst-2026-09-08"
            | "gpt-image-2.6-flare"
            | "gpt-image-2.6-sunburst"
            | "gpt-live-1"
            | "gpt-live-1-codex"
            | "gpt-live-transcribe"
            | "gpt-realtime"
            | "gpt-realtime-2025-08-28"
            | "gpt-realtime-1.5"
            | "gpt-realtime-2"
            | "gpt-realtime-2.1"
            | "gpt-realtime-2.1-mini"
            | "gpt-realtime-mini"
            | "gpt-realtime-mini-2025-10-06"
            | "gpt-realtime-mini-2025-12-15"
            | "gpt-realtime-translate"
            | "gpt-realtime-whisper"
            | "gpt-4o-realtime-preview"
            | "gpt-4o-realtime-preview-2024-10-01"
            | "gpt-4o-realtime-preview-2024-12-17"
            | "gpt-4o-realtime-preview-2025-06-03"
            | "gpt-4o-mini-realtime-preview"
            | "gpt-4o-mini-realtime-preview-2024-12-17"
            | "tts-1"
            | "tts-1-hd"
            | "gpt-4o-mini-tts"
            | "gpt-4o-mini-tts-2025-03-20"
            | "gpt-4o-mini-tts-2025-12-15"
    )
}

fn model_ids(models: &[CodexCatalogModel]) -> Vec<String> {
    models
        .iter()
        .map(|model| model.request_model().as_str().to_owned())
        .collect()
}

fn encode_account_catalog(catalog: &CodexAccountCatalog) -> OpaqueProviderData {
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

fn decode_account_catalog(
    scope: &ProviderCatalogScope,
    document: OpaqueProviderData,
) -> Result<CodexAccountCatalog, CodexCredentialCatalogError> {
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
        .filter(|models| !models.is_empty() && models.len() <= MAX_ACCOUNT_CATALOG_MODELS)
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
    Ok(CodexAccountCatalog::new(scope.clone(), observed_at, models))
}

fn same_catalog(
    left: &CodexCredentialCatalogSnapshot,
    right: &CodexCredentialCatalogSnapshot,
) -> bool {
    left.models == right.models && left.scope_catalogs == right.scope_catalogs
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
