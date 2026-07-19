//! Codex model entitlement 的实时查询与进程内 TTL cache；不落 PostgreSQL。

use std::collections::{BTreeMap, btree_map::Entry};
use std::fmt;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime};

use gateway_core::engine::credential::{AccountAvailability, ProviderAccount, ProviderAccountId};
use gateway_core::routing::{ProviderInstance, ProviderInstanceId};
use secrecy::ExposeSecret;
use thiserror::Error;
use uuid::Uuid;

use super::repository::{CodexCredentialRepository, CredentialRepositoryError};
use crate::provider::{CodexEndpointPolicy, CodexProviderInstanceConfig};
use crate::transport::profile::CodexWireProfileState;
use crate::transport::{
    CodexBackendClient, CodexCatalogModel, CodexRequestContext, build_reqwest_client,
};

const DEFAULT_CATALOG_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Clone)]
pub struct CodexCredentialCatalogSnapshot {
    provider_instance_id: ProviderInstanceId,
    observed_at: SystemTime,
    expires_at: Instant,
    models: Vec<CodexCatalogModel>,
    account_models: BTreeMap<String, Vec<String>>,
}

impl CodexCredentialCatalogSnapshot {
    #[must_use]
    pub const fn provider_instance_id(&self) -> &ProviderInstanceId {
        &self.provider_instance_id
    }

    #[must_use]
    pub const fn observed_at(&self) -> SystemTime {
        self.observed_at
    }

    #[must_use]
    pub fn models(&self) -> &[CodexCatalogModel] {
        &self.models
    }

    #[must_use]
    pub fn account_models(&self, account_id: &str) -> Option<&[String]> {
        self.account_models.get(account_id).map(Vec::as_slice)
    }

    #[must_use]
    pub fn is_fresh(&self) -> bool {
        Instant::now() < self.expires_at
    }
}

impl fmt::Debug for CodexCredentialCatalogSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexCredentialCatalogSnapshot")
            .field("provider_instance_id", &self.provider_instance_id)
            .field("observed_at", &self.observed_at)
            .field("model_count", &self.models.len())
            .field("account_count", &self.account_models.len())
            .finish()
    }
}

#[derive(Debug, Error)]
pub enum CodexCredentialCatalogError {
    #[error("Codex model catalog instance is invalid")]
    InvalidInstance,
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
    endpoint_policy: CodexEndpointPolicy,
    ttl: Duration,
    cache: Arc<RwLock<BTreeMap<ProviderInstanceId, CodexCredentialCatalogSnapshot>>>,
}

impl CodexCredentialCatalogService {
    pub fn new(
        repository: CodexCredentialRepository,
        profile: CodexWireProfileState,
    ) -> Result<Self, CodexCredentialCatalogError> {
        let http = build_reqwest_client().map_err(|_| CodexCredentialCatalogError::Upstream)?;
        Ok(Self::new_with_endpoint_policy(
            repository,
            profile,
            http,
            CodexEndpointPolicy::Official,
            DEFAULT_CATALOG_TTL,
        ))
    }

    #[must_use]
    pub fn new_with_endpoint_policy(
        repository: CodexCredentialRepository,
        profile: CodexWireProfileState,
        http: reqwest::Client,
        endpoint_policy: CodexEndpointPolicy,
        ttl: Duration,
    ) -> Self {
        Self {
            repository,
            profile,
            http,
            endpoint_policy,
            ttl: ttl.max(Duration::from_secs(1)),
            cache: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    pub fn cached(
        &self,
        instance: &ProviderInstanceId,
    ) -> Result<Option<CodexCredentialCatalogSnapshot>, CodexCredentialCatalogError> {
        let guard = self
            .cache
            .read()
            .map_err(|_| CodexCredentialCatalogError::Cache)?;
        Ok(guard
            .get(instance)
            .filter(|value| value.is_fresh())
            .cloned())
    }

    /// 读取单账号当前仍新鲜的模型 entitlement；不触发网络。
    pub fn cached_account_models(
        &self,
        instance: &ProviderInstanceId,
        account_id: &ProviderAccountId,
    ) -> Result<Option<Vec<String>>, CodexCredentialCatalogError> {
        Ok(self.cached(instance)?.and_then(|snapshot| {
            snapshot
                .account_models(account_id.as_str())
                .map(<[String]>::to_vec)
        }))
    }

    /// 只刷新指定账号的 realtime catalog，并原子替换其进程内 TTL entry。
    pub async fn synchronize_account(
        &self,
        instance: &ProviderInstance,
        account_id: &ProviderAccountId,
    ) -> Result<Vec<String>, CodexCredentialCatalogError> {
        let config =
            CodexProviderInstanceConfig::from_snapshot_with_policy(instance, self.endpoint_policy)
                .map_err(|_| CodexCredentialCatalogError::InvalidInstance)?;
        let account = self
            .repository
            .store()
            .get_account(account_id)
            .await
            .map_err(|_| CodexCredentialCatalogError::InvalidCredentialData)?
            .filter(|account| {
                account.provider().as_str() == "openai" && account.instance() == config.id()
            })
            .ok_or(CodexCredentialCatalogError::NoEligibleCredential)?;
        let client = CodexBackendClient::new(
            self.http.clone(),
            config.base_url().as_str(),
            self.profile.clone(),
        );
        let models = self.fetch_account_models(&client, &account).await?;
        let entitlement = models
            .iter()
            .map(|model| model.request_model().as_str().to_owned())
            .collect::<Vec<_>>();
        self.replace_account_cache(config.id(), account_id, models, entitlement.clone())?;
        Ok(entitlement)
    }

    pub async fn synchronize_instance(
        &self,
        instance: &ProviderInstance,
    ) -> Result<CodexCredentialCatalogSnapshot, CodexCredentialCatalogError> {
        if let Some(cached) = self.cached(instance.id())? {
            return Ok(cached);
        }
        let config =
            CodexProviderInstanceConfig::from_snapshot_with_policy(instance, self.endpoint_policy)
                .map_err(|_| CodexCredentialCatalogError::InvalidInstance)?;
        let accounts = self.repository.list_for_instance(config.id()).await?;
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
            config.base_url().as_str(),
            self.profile.clone(),
        );
        let mut union = BTreeMap::<String, CodexCatalogModel>::new();
        let mut union_order = Vec::new();
        let mut account_models = BTreeMap::new();
        for account in accounts {
            let models = self.fetch_account_models(&client, &account).await?;
            let mut entitlement = Vec::with_capacity(models.len());
            for model in &models {
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
            account_models.insert(account.id().to_string(), entitlement);
        }
        let observed_at = SystemTime::now();
        let snapshot = CodexCredentialCatalogSnapshot {
            provider_instance_id: config.id().clone(),
            observed_at,
            expires_at: Instant::now() + self.ttl,
            models: union_order
                .into_iter()
                .filter_map(|id| union.remove(&id))
                .collect(),
            account_models,
        };
        self.cache
            .write()
            .map_err(|_| CodexCredentialCatalogError::Cache)?
            .insert(config.id().clone(), snapshot.clone());
        Ok(snapshot)
    }

    async fn fetch_account_models(
        &self,
        client: &CodexBackendClient,
        account: &ProviderAccount,
    ) -> Result<Vec<CodexCatalogModel>, CodexCredentialCatalogError> {
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
        client
            .fetch_models_with_context(context)
            .await
            .map(|snapshot| snapshot.models().to_vec())
            .map_err(|_| CodexCredentialCatalogError::Upstream)
    }

    fn replace_account_cache(
        &self,
        instance: &ProviderInstanceId,
        account_id: &ProviderAccountId,
        models: Vec<CodexCatalogModel>,
        entitlement: Vec<String>,
    ) -> Result<(), CodexCredentialCatalogError> {
        let mut cache = self
            .cache
            .write()
            .map_err(|_| CodexCredentialCatalogError::Cache)?;
        let mut union = BTreeMap::<String, CodexCatalogModel>::new();
        let mut union_order = Vec::new();
        let mut account_models = BTreeMap::new();
        if let Some(existing) = cache.get(instance).filter(|snapshot| snapshot.is_fresh()) {
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
        account_models.insert(account_id.to_string(), entitlement);
        union.retain(|model, _| {
            account_models
                .values()
                .any(|entitlement| entitlement.iter().any(|item| item == model))
        });
        union_order.retain(|id| union.contains_key(id));
        cache.insert(
            instance.clone(),
            CodexCredentialCatalogSnapshot {
                provider_instance_id: instance.clone(),
                observed_at: SystemTime::now(),
                expires_at: Instant::now() + self.ttl,
                models: union_order
                    .into_iter()
                    .filter_map(|id| union.remove(&id))
                    .collect(),
                account_models,
            },
        );
        Ok(())
    }

    pub fn invalidate(
        &self,
        instance: &ProviderInstanceId,
    ) -> Result<(), CodexCredentialCatalogError> {
        self.cache
            .write()
            .map_err(|_| CodexCredentialCatalogError::Cache)?
            .remove(instance);
        Ok(())
    }
}

fn eligible_catalog_account(account: &ProviderAccount, now: SystemTime) -> bool {
    account.enabled()
        && account.access_token_expires_at() > now
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
