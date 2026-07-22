//! Codex model entitlement 的实时查询与可重建进程内 cache；不落 PostgreSQL。

use std::collections::{BTreeMap, btree_map::Entry};
use std::fmt;
use std::sync::{Arc, Mutex, RwLock};
use std::time::SystemTime;

use gateway_core::engine::credential::{
    AccountAvailability, CredentialRevision, ProviderAccount, ProviderAccountId,
};
use gateway_core::engine::provider::ProviderCatalogGeneration;
use secrecy::ExposeSecret;
use thiserror::Error;
use tokio::sync::Notify;
use uuid::Uuid;

use super::repository::{CodexCredentialRepository, CredentialRepositoryError};
use crate::provider::OFFICIAL_CODEX_BASE_URL;
use crate::transport::profile::CodexWireProfileState;
use crate::transport::{CodexBackendClient, CodexCatalogModel, CodexRequestContext};

const MAX_RESPONSE_ETAG_BYTES: usize = 256;

#[derive(Clone)]
pub struct CodexCredentialCatalogSnapshot {
    observed_at: SystemTime,
    models: Vec<CodexCatalogModel>,
    account_models: BTreeMap<ProviderAccountId, CodexAccountEntitlement>,
}

#[derive(Clone, PartialEq, Eq)]
struct CodexAccountEntitlement {
    revision: CredentialRevision,
    models: Vec<String>,
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

    #[must_use]
    pub fn account_models(
        &self,
        account_id: &ProviderAccountId,
        revision: CredentialRevision,
    ) -> Option<&[String]> {
        self.account_models
            .get(account_id)
            .filter(|entry| entry.revision == revision)
            .map(|entry| entry.models.as_slice())
    }
}

impl fmt::Debug for CodexCredentialCatalogSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexCredentialCatalogSnapshot")
            .field("observed_at", &self.observed_at)
            .field("model_count", &self.models.len())
            .field("account_count", &self.account_models.len())
            .finish()
    }
}

#[derive(Debug, Error)]
pub enum CodexCredentialCatalogError {
    #[error("Codex model catalog has no eligible account")]
    NoEligibleCredential,
    #[error("Codex model catalog account data is invalid")]
    InvalidCredentialData,
    #[error("Codex model catalog upstream query failed")]
    Upstream,
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
    cache: Arc<RwLock<CatalogCacheState>>,
    etags: Arc<Mutex<CatalogEtagState>>,
    etag_notification: Arc<Notify>,
}

impl CodexCredentialCatalogService {
    pub fn new(
        repository: CodexCredentialRepository,
        profile: CodexWireProfileState,
        http: reqwest::Client,
    ) -> Self {
        Self {
            repository,
            profile,
            http,
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

    /// 读取单账号当前仍新鲜的模型 entitlement；不触发网络。
    pub fn cached_account_models(
        &self,
        account_id: &ProviderAccountId,
        revision: CredentialRevision,
    ) -> Result<Option<Vec<String>>, CodexCredentialCatalogError> {
        Ok(self.cached()?.and_then(|snapshot| {
            snapshot
                .account_models(account_id, revision)
                .map(<[String]>::to_vec)
        }))
    }

    pub fn observed_model_support(
        &self,
        account_id: &ProviderAccountId,
        revision: CredentialRevision,
        model: &str,
    ) -> Result<Option<bool>, CodexCredentialCatalogError> {
        Ok(self.cached()?.and_then(|snapshot| {
            snapshot
                .account_models(account_id, revision)
                .map(|models| models.iter().any(|candidate| candidate == model))
        }))
    }

    /// 只刷新指定账号的 realtime catalog，并以本地 revision 原子合并账号事实。
    pub async fn synchronize_account(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<Vec<String>, CodexCredentialCatalogError> {
        let account = self
            .repository
            .store()
            .get_account(account_id)
            .await
            .map_err(|_| CodexCredentialCatalogError::InvalidCredentialData)?
            .filter(|account| account.provider().as_str() == "openai")
            .ok_or(CodexCredentialCatalogError::NoEligibleCredential)?;
        let client = CodexBackendClient::new(
            self.http.clone(),
            OFFICIAL_CODEX_BASE_URL,
            self.profile.clone(),
        );
        let cache_revision = self.cache_revision()?;
        let fetched = self.fetch_account_models(&client, &account).await?;
        let entitlement = fetched
            .models
            .iter()
            .map(|model| model.request_model().as_str().to_owned())
            .collect::<Vec<_>>();
        self.replace_account_cache(
            cache_revision,
            account_id,
            account.revision(),
            fetched.models,
            entitlement.clone(),
        )?;
        self.record_applied_catalog_etags(fetched.etag)?;
        Ok(entitlement)
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
        let now = SystemTime::now();
        let accounts = accounts
            .into_iter()
            .filter(|account| eligible_catalog_account(account, now))
            .collect::<Vec<_>>();
        if accounts.is_empty() {
            return Err(CodexCredentialCatalogError::NoEligibleCredential);
        }
        let client = CodexBackendClient::new(
            self.http.clone(),
            OFFICIAL_CODEX_BASE_URL,
            self.profile.clone(),
        );
        let mut union = BTreeMap::<String, CodexCatalogModel>::new();
        let mut union_order = Vec::new();
        let mut account_models = BTreeMap::new();
        let mut etags = Vec::new();
        for account in accounts {
            let fetched = self.fetch_account_models(&client, &account).await?;
            let mut entitlement = Vec::with_capacity(fetched.models.len());
            for model in &fetched.models {
                let id = model.request_model().as_str().to_owned();
                entitlement.push(id.clone());
                match union.entry(id) {
                    Entry::Vacant(entry) => {
                        union_order.push(entry.key().clone());
                        entry.insert(model.clone());
                    }
                    Entry::Occupied(entry) if entry.get() == model => {}
                    Entry::Occupied(_) => {
                        return Err(CodexCredentialCatalogError::ConflictingModelFacts);
                    }
                }
            }
            account_models.insert(
                account.id().clone(),
                CodexAccountEntitlement {
                    revision: account.revision(),
                    models: entitlement,
                },
            );
            etags.extend(fetched.etag);
        }
        let observed_at = SystemTime::now();
        let snapshot = CodexCredentialCatalogSnapshot {
            observed_at,
            models: union_order
                .into_iter()
                .filter_map(|id| union.remove(&id))
                .collect(),
            account_models,
        };
        Ok(FetchedCatalog { snapshot, etags })
    }

    async fn fetch_account_models(
        &self,
        client: &CodexBackendClient,
        account: &ProviderAccount,
    ) -> Result<FetchedAccountModels, CodexCredentialCatalogError> {
        let runtime = self.repository.load_runtime_credential(account).await?;
        let request_id = format!("catalog_{}", Uuid::now_v7().simple());
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
        let snapshot = client
            .fetch_models_with_context(context)
            .await
            .map_err(|_| CodexCredentialCatalogError::Upstream)?;
        Ok(FetchedAccountModels {
            models: snapshot.models().to_vec(),
            etag: snapshot.etag().map(str::to_owned),
        })
    }

    fn replace_account_cache(
        &self,
        expected_cache_revision: u64,
        account_id: &ProviderAccountId,
        revision: CredentialRevision,
        models: Vec<CodexCatalogModel>,
        entitlement: Vec<String>,
    ) -> Result<(), CodexCredentialCatalogError> {
        let mut cache = self
            .cache
            .write()
            .map_err(|_| CodexCredentialCatalogError::Cache)?;
        if cache.revision != expected_cache_revision {
            return Err(CodexCredentialCatalogError::ConcurrentUpdate);
        }
        let mut union = BTreeMap::<String, CodexCatalogModel>::new();
        let mut union_order = Vec::new();
        let mut account_models = BTreeMap::new();
        if let Some(existing) = cache.snapshot.as_ref() {
            for model in &existing.models {
                let id = model.request_model().as_str().to_owned();
                union_order.push(id.clone());
                union.insert(id, model.clone());
            }
            account_models = existing.account_models.clone();
        }
        for model in models {
            let id = model.request_model().as_str().to_owned();
            match union.entry(id) {
                Entry::Vacant(entry) => {
                    union_order.push(entry.key().clone());
                    entry.insert(model);
                }
                Entry::Occupied(entry) if entry.get() == &model => {}
                Entry::Occupied(_) => {
                    return Err(CodexCredentialCatalogError::ConflictingModelFacts);
                }
            }
        }
        account_models.insert(
            account_id.clone(),
            CodexAccountEntitlement {
                revision,
                models: entitlement,
            },
        );
        union.retain(|model, _| {
            account_models
                .values()
                .any(|entitlement| entitlement.models.iter().any(|item| item == model))
        });
        union_order.retain(|id| union.contains_key(id));
        let snapshot = CodexCredentialCatalogSnapshot {
            observed_at: SystemTime::now(),
            models: union_order
                .into_iter()
                .filter_map(|id| union.remove(&id))
                .collect(),
            account_models,
        };
        let changed = cache
            .snapshot
            .as_ref()
            .is_none_or(|existing| !same_catalog(existing, &snapshot));
        cache.snapshot = Some(snapshot);
        cache.revision = cache.revision.saturating_add(1);
        if changed {
            cache.generation = cache.generation.saturating_add(1);
        }
        Ok(())
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

    /// 忽略当前 cache，按 ETag 变化强制生成一份完整新快照。
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

fn same_catalog(
    left: &CodexCredentialCatalogSnapshot,
    right: &CodexCredentialCatalogSnapshot,
) -> bool {
    left.models == right.models && left.account_models == right.account_models
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
