//! OpenAI 账号模型目录的聚合、缓存与调度资格

mod refresh;

use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};
use std::fmt;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use futures::{StreamExt, stream};
use gateway_core::account::AccountCatalogObservation;
use gateway_core::account::{
    CredentialRevision, CredentialState, OpaqueProviderData, ProviderAccount, ProviderAccountId,
};
use gateway_core::provider_ports::{
    ProviderCatalogCacheKey, ProviderCatalogCachePort, ProviderCatalogScope,
};
use gateway_core::routing::{
    FrozenAccountScope, ModelPresentation, ProviderCatalogGeneration, ProviderKind,
    ProviderModelContent, ProviderModelDescriptor, UpstreamModelId,
};
use secrecy::ExposeSecret;
use thiserror::Error;
use tokio::sync::Notify;

use uuid::Uuid;

use super::repository::{CodexCredentialRepository, CredentialRepositoryError};
use crate::transport::profile::CodexWireProfileState;
use crate::transport::{CodexBackendClient, CodexCatalogModel, CodexRequestContext};

const MAX_RESPONSE_ETAG_BYTES: usize = 256;
const ACCOUNT_CATALOG_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
// 调度证据覆盖默认 15 分钟刷新周期及抖动，长期未刷新时恢复上游验证
const SCHEDULING_CATALOG_MAX_AGE: Duration = Duration::from_secs(30 * 60);
const MAX_ACCOUNT_CATALOG_MODELS: usize = 2_048;
const MODEL_CATALOG_TIMEOUT: Duration = Duration::from_secs(15);

const REFRESH_CONCURRENCY: usize = 32;
// 同时到期或导入大量账号时也限制起始速率，不能把连接并发上限当作请求速率上限
const REFRESH_SPACING: Duration = Duration::from_millis(10);
// 小账号池直接在内存复核，避免为少数账号增加数据库联表
const MIN_CATALOG_PUSHDOWN: usize = 128;
const PUBLICATION_BATCH: usize = 128;

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

struct CatalogContent {
    models: Vec<CodexCatalogModel>,
    ids: Vec<String>,
    digest: [u8; 32],
}

impl CatalogContent {
    fn new(models: Vec<CodexCatalogModel>, document: &[u8]) -> Self {
        use sha2::Digest;
        Self {
            digest: sha2::Sha256::digest(document).into(),
            ids: model_ids(&models),
            models,
        }
    }
}

#[derive(Clone)]
struct AccountEntry {
    observed_at: SystemTime,
    content: Arc<CatalogContent>,
    invalidated: Arc<std::sync::atomic::AtomicBool>,
    etag: Option<String>,
}

type Entries = BTreeMap<CodexCatalogScope, AccountEntry>;

pub struct CodexCredentialCatalogSnapshot {
    models: Vec<CodexCatalogModel>,
    entries: Entries,
    sources: Vec<(Arc<CatalogContent>, Vec<ProviderAccountId>)>,
}

impl CodexCredentialCatalogSnapshot {
    fn new(entries: Entries) -> Self {
        let mut models: Vec<CodexCatalogModel> = Vec::new();
        let mut indices = BTreeMap::<&str, usize>::new();
        let mut sources = BTreeMap::<_, (Arc<CatalogContent>, Vec<ProviderAccountId>)>::new();
        for (scope, entry) in &entries {
            let (_, members) = sources
                .entry(entry.content.digest)
                .or_insert_with(|| (entry.content.clone(), Vec::new()));
            members.push(scope.account_id.clone());
            if members.len() > 1 {
                continue;
            }
            for model in &entry.content.models {
                match indices.entry(model.request_model().as_str()) {
                    Entry::Vacant(slot) => {
                        slot.insert(models.len());
                        models.push(model.clone());
                    }
                    Entry::Occupied(slot)
                        if model.document().protocol() == "codex"
                            && models[*slot.get()].document().protocol() != "codex" =>
                    {
                        models[*slot.get()] = model.clone()
                    }
                    _ => {}
                }
            }
        }
        Self {
            models,
            entries,
            sources: sources.into_values().collect(),
        }
    }

    #[must_use]
    pub fn models(&self) -> &[CodexCatalogModel] {
        &self.models
    }

    pub(super) fn excludes(&self, account: &ProviderAccount, model: &str, now: SystemTime) -> bool {
        self.entries
            .get(&CodexCatalogScope::for_account(account))
            .is_some_and(|e| {
                !e.invalidated.load(std::sync::atomic::Ordering::Relaxed)
                    && now
                        .duration_since(e.observed_at)
                        .is_ok_and(|age| age < SCHEDULING_CATALOG_MAX_AGE)
                    && !e.content.ids.iter().any(|id| id == model)
            })
    }

    pub fn account_models(&self, account: &ProviderAccount) -> Option<&[String]> {
        self.entries
            .get(&CodexCatalogScope::for_account(account))
            .map(|e| e.content.ids.as_slice())
    }

    #[must_use]
    pub fn model_catalog_accounts(&self) -> BTreeMap<&str, BTreeSet<ProviderAccountId>> {
        let mut accounts = BTreeMap::<&str, BTreeSet<ProviderAccountId>>::new();
        for (scope, entry) in &self.entries {
            for model in &entry.content.ids {
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
            .field("model_count", &self.models.len())
            .field("scope_count", &self.entries.len())
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
struct CatalogCacheState {
    restored: bool,
    revision: u64,
    generation: u64,
    snapshot: Option<Arc<CodexCredentialCatalogSnapshot>>,
    pending: BTreeMap<CodexCatalogScope, String>,
}

struct FetchedAccountModels {
    models: Vec<CodexCatalogModel>,
    document: bytes::Bytes,
    etag: Option<String>,
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
    refresh_notification: Arc<Notify>,
    refresh_lock: Arc<tokio::sync::Mutex<()>>,
    publication: Arc<std::sync::Mutex<()>>,
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
            refresh_notification: Arc::new(Notify::new()),
            refresh_lock: Arc::new(tokio::sync::Mutex::new(())),
            publication: Default::default(),
        }
    }

    /// 原生条目取自后台发现的账号目录；用当前账号版本和冻结的 Key 政策双重过滤
    pub async fn client_model_catalog(
        &self,
        scope: &FrozenAccountScope,
    ) -> Result<Vec<ProviderModelDescriptor>, CodexCredentialCatalogError> {
        let snapshot = self
            .cached()?
            .ok_or(CodexCredentialCatalogError::NoEligibleCredential)?;
        // 正文相同的账号只需核对每个模型在当前政策下的首选来源
        let mut candidates = BTreeSet::new();
        for (content, members) in &snapshot.sources {
            if let Some(id) = members.iter().find(|id| scope.allows(id)) {
                candidates.insert(id.clone());
            } else {
                continue;
            }
            for id in &content.ids {
                if let Some(account) = members
                    .iter()
                    .find(|account| scope.allows_model(account, id))
                {
                    candidates.insert(account.clone());
                }
            }
        }
        let ids = candidates.into_iter().collect::<Vec<_>>();
        let mut accounts = self.repository.list_for_accounts(&ids).await?;
        let now = SystemTime::now();
        // 代表账号刚被停用或轮换时，仅这一次扩大本地资格查询，仍不查询上游
        if accounts.len() != ids.len()
            || accounts.iter().any(|a| {
                !eligible_catalog_account(a, now)
                    || !snapshot
                        .entries
                        .contains_key(&CodexCatalogScope::for_account(a))
            })
        {
            let ids = snapshot
                .entries
                .keys()
                .filter(|s| scope.allows(&s.account_id))
                .map(|s| s.account_id.clone())
                .collect::<Vec<_>>();
            accounts = self.repository.list_for_accounts(&ids).await?;
        }
        let mut selected = BTreeMap::new();
        let mut available = false;
        for account in accounts.iter().filter(|a| eligible_catalog_account(a, now)) {
            let Some(entry) = snapshot
                .entries
                .get(&CodexCatalogScope::for_account(account))
            else {
                continue;
            };
            available = true;
            let api = account.authentication_kind() == super::CODEX_AUTHENTICATION_KIND_API_KEY;
            for (position, model) in entry.content.models.iter().enumerate() {
                if !scope.allows_model(account.id(), model.request_model().as_str()) {
                    continue;
                }
                let order = (
                    api,
                    model.document().protocol() != "codex",
                    account.id(),
                    position,
                );
                let slot = selected
                    .entry(model.request_model())
                    .or_insert((order, model));
                if order < slot.0 {
                    *slot = (order, model);
                }
            }
        }
        if !available {
            return Err(CodexCredentialCatalogError::NoEligibleCredential);
        }
        let mut models: Vec<_> = selected.into_values().collect();
        models.sort_unstable_by_key(|(order, _)| *order);
        Ok(models
            .into_iter()
            .map(|(_, model)| {
                let id = model.request_model().as_str();
                ProviderModelDescriptor {
                    model: model.request_model().clone(),
                    content: if model.document().protocol() == "codex" {
                        ProviderModelContent::Native(model.document().clone())
                    } else {
                        ProviderModelContent::Adapted(
                            ModelPresentation::new(Some(id.to_owned()), None)
                                .with_agent_tools(true, false)
                                .with_hidden(is_specialized_api_model(id)),
                        )
                    },
                }
            })
            .collect())
    }

    pub(crate) fn messages_model_capabilities(
        &self,
        account: &ProviderAccount,
        model: &UpstreamModelId,
    ) -> Option<gateway_protocol::anthropic::MessagesModelCapabilities> {
        let snapshot = self.cached().ok()??;
        snapshot
            .entries
            .get(&CodexCatalogScope::for_account(account))?
            .content
            .models
            .iter()
            .find(|entry| entry.request_model() == model)?
            .messages_capabilities()
            .cloned()
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
    ) -> Result<Option<Arc<CodexCredentialCatalogSnapshot>>, CodexCredentialCatalogError> {
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

    /// 大池中相同正文只传递一个摘要；存储只跳过有当前账号证据的来源
    pub(super) fn omissions(
        &self,
        model: &str,
        ids: &[ProviderAccountId],
        exempt: impl Fn(&ProviderAccountId) -> bool,
    ) -> Option<gateway_core::account::AccountCatalogOmissions> {
        if ids.len() < MIN_CATALOG_PUSHDOWN {
            return None;
        }
        let cache = self.cache.read().ok()?;
        let snapshot = cache.snapshot.as_ref()?.clone();
        let pending = cache
            .pending
            .keys()
            .map(|s| s.account_id.clone())
            .collect::<BTreeSet<_>>();
        drop(cache);
        let documents = snapshot
            .sources
            .iter()
            .filter(|(c, _)| !c.ids.iter().any(|id| id == model))
            .map(|(c, _)| c.digest)
            .collect::<Vec<_>>();
        if documents.is_empty() {
            return None;
        }
        let as_of = SystemTime::now();
        Some(gateway_core::account::AccountCatalogOmissions {
            documents,
            exceptions: ids
                .iter()
                .filter(|id| pending.contains(*id) || exempt(id))
                .cloned()
                .collect(),
            observed_after: as_of.checked_sub(SCHEDULING_CATALOG_MAX_AGE)?,
            as_of,
        })
    }

    /// 优先读取账号目录缓存；缺失时只查询该账号
    pub async fn cached_or_refresh_account_catalog(
        &self,
        account: &ProviderAccount,
    ) -> Result<CodexAccountCatalog, CodexCredentialCatalogError> {
        if let Some(catalog) = self.read_account_catalog(account).await? {
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
        let scope = CodexCatalogScope::for_account(&account);
        let client = CodexBackendClient::new(
            self.http.clone(),
            self.base_url.clone(),
            self.profile.clone(),
        );
        let revision = self.state_revision()?;
        let (catalog, entry) = self
            .fetch_observation(&client, &account, &scope, None)
            .await?;
        if eligible_catalog_account(&account, SystemTime::now())
            && let Err(error) = self.merge_observations(revision, vec![(scope, entry)])
        {
            if !matches!(error, CodexCredentialCatalogError::ConcurrentUpdate) {
                return Err(error);
            }
            self.schedule_refresh();
        }
        Ok(catalog)
    }

    async fn fetch_observation(
        &self,
        client: &CodexBackendClient,
        account: &ProviderAccount,
        scope: &CodexCatalogScope,
        etag: Option<String>,
    ) -> Result<(CodexAccountCatalog, AccountEntry), CodexCredentialCatalogError> {
        let observed_at = SystemTime::now();
        let fetched = self.fetch_account_models(client, account).await?;
        let observed = AccountCatalogObservation {
            account_id: account.id().clone(),
            revision: account.revision(),
            plan: account.plan_type().map(str::to_owned),
            observed_at,
            etag: etag.clone().or(fetched.etag),
        };
        if !self
            .repository
            .store()
            .save_account_catalog(&observed, &fetched.document)
            .await
            .map_err(|_| CodexCredentialCatalogError::Cache)?
        {
            return Err(CodexCredentialCatalogError::ConcurrentUpdate);
        }
        let content = Arc::new(CatalogContent::new(fetched.models, &fetched.document));
        // Redis 仅为管理端短期缓存，失败不能撤销已持久化的目录
        let catalog = CodexAccountCatalog::new(
            scope.cache_scope()?,
            observed_at.into(),
            content.ids.clone(),
        );
        if let Err(error) = self.replace_account_catalog(&catalog).await {
            tracing::warn!(%error,"OpenAI account catalog cache write failed");
        }
        Ok((
            catalog,
            AccountEntry {
                observed_at,
                content,
                etag: observed.etag,
                invalidated: Default::default(),
            },
        ))
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
        let fetched = self.fetch_account_models(&client, account).await?;
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
        let scope = CodexCatalogScope::for_account(account);
        let key = scope.cache_scope()?;
        if let Some(snapshot) = self.cached()?
            && let Some(entry) = snapshot.entries.get(&scope)
        {
            return Ok(Some(CodexAccountCatalog::new(
                key,
                entry.observed_at.into(),
                entry.content.ids.clone(),
            )));
        }
        self.read_cached_catalog(&key).await
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
    ) -> Result<FetchedAccountModels, CodexCredentialCatalogError> {
        tokio::time::timeout(MODEL_CATALOG_TIMEOUT, async {
            let credential = self
                .repository
                .load_runtime_credential(account)
                .await
                .map_err(|_| CodexCredentialCatalogError::InvalidCredentialData)?;
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
                        None,
                    ),
                    None,
                )
                .await;
            let snapshot = result.map_err(|error| CodexCredentialCatalogError::Upstream {
                detail: error.to_string(),
            })?;
            let (models, document, etag) = snapshot.into_parts();
            Ok(FetchedAccountModels {
                models,
                document,
                etag,
            })
        })
        .await
        .map_err(|_| CodexCredentialCatalogError::Upstream {
            detail: "model catalog timed out".to_owned(),
        })?
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
