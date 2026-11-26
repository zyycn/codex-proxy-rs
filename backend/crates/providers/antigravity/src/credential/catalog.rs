//! 将资源服务确认的项目与模型绑定到授权注册和账号凭据版本

use crate::{
    credential::{CredentialService, oauth::error},
    transport::{Catalog, CloudCodeClient, Model, unsent},
};
use gateway_admin::{
    model::provider_credentials::{ProviderModel, ProviderModels},
    ports::provider::{ProviderAdminError, ProviderAdminErrorKind as AdminKind},
};
use gateway_core::{
    account::{AccountCatalogObservation, ProviderAccountId, StoredAccountCatalog},
    error::{ProviderError, ProviderErrorKind as Kind},
    operation::OperationKind,
    routing::{
        AccountModelMetadata, ModelCapabilities, ProviderModelCapabilities, UpstreamModelId,
    },
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime},
};

pub(crate) struct CatalogService {
    credentials: Arc<CredentialService>,
    kind: gateway_core::routing::ProviderKind,
    accounts: Arc<dyn gateway_core::account::ProviderAccountStore>,
    registration: String,
    pub(crate) origin: url::Url,
    pub(crate) generation: AtomicU64,
    observed:
        Mutex<BTreeMap<ProviderAccountId, (gateway_core::account::CredentialRevision, SystemTime)>>,
    refresh_cursor: AtomicUsize,
}

pub(crate) struct RefreshCatalog {
    observation: AccountCatalogObservation,
    document: bytes::Bytes,
}

impl CatalogService {
    pub(crate) async fn capture_refresh_catalogs(
        &self,
        candidates: &[gateway_core::account::LoadedCredential],
    ) -> Result<BTreeMap<ProviderAccountId, RefreshCatalog>, ProviderAdminError> {
        let candidates: BTreeMap<_, _> = candidates
            .iter()
            .map(|loaded| (loaded.account.id(), &loaded.account))
            .collect();
        let stored = self
            .accounts
            .load_account_catalogs(&self.kind)
            .await
            .map_err(|e| error(AdminKind::Unavailable).with_source(e))?;
        let mut snapshots = BTreeMap::new();
        for catalog in stored {
            for observation in catalog.accounts {
                if candidates
                    .get(&observation.account_id)
                    .is_some_and(|account| {
                        account.revision() == observation.revision
                            && account.plan_type() == observation.plan.as_deref()
                            && observation.etag.as_ref() == Some(&self.registration)
                    })
                {
                    snapshots.insert(
                        observation.account_id.clone(),
                        RefreshCatalog {
                            observation,
                            document: catalog.document.clone(),
                        },
                    );
                }
            }
        }
        Ok(snapshots)
    }

    pub(crate) async fn after_credential_refresh(
        &self,
        account_id: &ProviderAccountId,
        revision: gateway_core::account::CredentialRevision,
        snapshot: Option<RefreshCatalog>,
    ) -> Result<(), ProviderAdminError> {
        if let Some(mut snapshot) = snapshot {
            // OAuth 续期已核对同一身份；沿用观测时间，存储仍校验新 revision 和原套餐
            snapshot.observation.revision = revision;
            if self
                .accounts
                .save_account_catalog(&snapshot.observation, &snapshot.document)
                .await
                .map_err(|e| error(AdminKind::Unavailable).with_source(e))?
            {
                self.invalidate();
                return Ok(());
            }
        }
        // 401 过期账号不会进入旧目录读取；新 AT 提交后立即尝试恢复，失败再由目录任务重试
        self.account_models(account_id, false).await.map(|_| ())
    }

    pub(crate) fn new(
        kind: gateway_core::routing::ProviderKind,
        accounts: Arc<dyn gateway_core::account::ProviderAccountStore>,
        credentials: Arc<CredentialService>,
        registration: String,
        origin: url::Url,
    ) -> Self {
        Self {
            kind,
            accounts,
            credentials,
            registration,
            origin,
            generation: AtomicU64::new(0),
            observed: Mutex::default(),
            refresh_cursor: AtomicUsize::new(0),
        }
    }

    pub(crate) fn invalidate(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
    }
    pub(crate) async fn catalogs(
        &self,
    ) -> Result<Vec<(Vec<AccountCatalogObservation>, Catalog)>, ProviderError> {
        let registration = &self.registration;
        let docs = self
            .accounts
            .load_account_catalogs(&self.kind)
            .await
            .map_err(|e| unsent(Kind::ProviderInfrastructureUnavailable).with_source(e))?;
        let mut result = Vec::new();
        for StoredAccountCatalog { accounts, document } in docs {
            let accounts: Vec<_> = accounts
                .into_iter()
                .filter(|a| a.etag.as_ref() == Some(registration))
                .collect();
            if !accounts.is_empty() {
                let catalog = serde_json::from_slice(&document)
                    .map_err(|e| unsent(Kind::ProviderInfrastructureUnavailable).with_source(e))?;
                result.push((accounts, catalog));
            }
        }
        let observations = result
            .iter()
            .flat_map(|(accounts, _)| accounts.iter())
            .map(|a| (a.account_id.clone(), (a.revision, a.observed_at)))
            .collect();
        let mut observed = self
            .observed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *observed != observations {
            *observed = observations;
            self.invalidate();
        }
        Ok(result)
    }

    pub(crate) async fn capabilities(
        &self,
    ) -> Result<Vec<ProviderModelCapabilities>, ProviderError> {
        let mut members =
            BTreeMap::<UpstreamModelId, BTreeMap<ProviderAccountId, AccountModelMetadata>>::new();
        let mut groups = BTreeMap::new();
        let mut search_models = BTreeMap::new();
        let mut catalogs = self.catalogs().await?;
        // 多个账号的目录有交集时，按稳定来源顺序合并展示名和档位顺序
        catalogs.sort_by(|(left, _), (right, _)| {
            left.first()
                .map(|a| &a.account_id)
                .cmp(&right.first().map(|a| &a.account_id))
        });
        for (accounts, catalog) in catalogs {
            super::model_groups::collect(&mut groups, &catalog, &accounts);
            if let Some(search_model) = catalog
                .web_search_models
                .first()
                .and_then(|id| UpstreamModelId::new(id).ok())
            {
                search_models.extend(
                    accounts
                        .iter()
                        .map(|a| (a.account_id.clone(), search_model.clone())),
                );
            }
            for (key, model) in catalog.models {
                let id = UpstreamModelId::new(key).map_err(|_| unsent(Kind::Protocol))?;
                let metadata = account_metadata(model);
                members.entry(id).or_default().extend(
                    accounts
                        .iter()
                        .map(|a| (a.account_id.clone(), metadata.clone())),
                );
            }
        }
        let capabilities = ModelCapabilities::new(
            [OperationKind::Generate, OperationKind::CountTokens].into(),
            None,
        )
        .with_dedicated_web_search(search_models)
        .with_generation_protocol(gateway_protocol::genai::GENAI_PROTOCOL)
        .with_translated_generation_protocols([
            gateway_protocol::openai::OPENAI_PROTOCOL,
            gateway_protocol::anthropic::ANTHROPIC_PROTOCOL,
        ])
        .with_upstream_feature_validation();
        let mut models = BTreeMap::new();
        for (id, metadata) in members {
            let accounts = metadata.keys().cloned().collect();
            let model = ProviderModelCapabilities::new(
                id.clone(),
                capabilities.clone().with_account_metadata(metadata),
            );
            models.insert(id, model.with_catalog_accounts(accounts));
        }
        // 上游已有同名实体时保留原生型号，不让其他账号的分组覆盖它
        for (id, group) in groups {
            models.entry(id.clone()).or_insert_with(|| {
                let accounts = group
                    .variants
                    .values()
                    .flat_map(|v| v.accounts.iter().cloned())
                    .collect();
                group
                    .into_model(id, capabilities.clone())
                    .with_catalog_accounts(accounts)
            });
        }
        Ok(models.into_values().collect())
    }

    pub(crate) async fn account_models(
        &self,
        id: &ProviderAccountId,
        refresh: bool,
    ) -> Result<ProviderModels, ProviderAdminError> {
        let current = self.credentials.current(id).await?;
        if !refresh {
            for (accounts, cat) in self
                .catalogs()
                .await
                .map_err(|e| error(AdminKind::Unavailable).with_source(e))?
            {
                if let Some(a) = accounts
                    .iter()
                    .find(|a| &a.account_id == id && a.revision == current.account.revision())
                {
                    return admin_models(cat, a.observed_at);
                }
            }
        }
        let material = self
            .credentials
            .material(&current)
            .map_err(|e| error(AdminKind::CredentialRefreshRequired).with_source(e))?;
        let profile = self.credentials.request_profile().await?;
        let client = CloudCodeClient::new(&self.origin, current.account.outbound_proxy(), &profile)
            .map_err(|e| error(AdminKind::Unavailable).with_source(e))?;
        let observed_at = SystemTime::now();
        let catalog = client
            .catalog(&material.access_token)
            .await
            .map_err(|e| match e.kind() {
                Kind::Unauthorized => error(AdminKind::CredentialRefreshRequired).with_source(e),
                _ => error(AdminKind::Unavailable)
                    .with_public_message("Antigravity 项目或模型目录查询失败")
                    .with_source(e),
            })?;
        self.publish(
            &current.account,
            current.account.plan_type().map(str::to_owned),
            catalog,
            observed_at,
        )
        .await
    }

    pub(crate) async fn publish(
        &self,
        account: &gateway_core::account::ProviderAccount,
        plan: Option<String>,
        mut catalog: Catalog,
        observed_at: SystemTime,
    ) -> Result<ProviderModels, ProviderAdminError> {
        // 模型目录只拥有能力事实，易变的额度由独立快照保存
        for model in catalog.models.values_mut() {
            model.quota = None;
        }
        let observed = AccountCatalogObservation {
            account_id: account.id().clone(),
            revision: account.revision(),
            plan,
            etag: Some(self.registration.clone()),
            observed_at,
        };
        let body = serde_json::to_vec(&catalog).map_err(|_| error(AdminKind::Internal))?;
        if !self
            .accounts
            .save_account_catalog(&observed, &body)
            .await
            .map_err(|e| error(AdminKind::Unavailable).with_source(e))?
        {
            return Err(error(AdminKind::Conflict));
        }
        self.generation.fetch_add(1, Ordering::Relaxed);
        admin_models(catalog, observed_at)
    }
}

fn account_metadata(model: Model) -> AccountModelMetadata {
    let description = model
        .description
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let tag = model
        .tag_description
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let description = match (description, tag) {
        (Some(text), Some(tag)) if text != tag => Some(format!("{text} {tag}")),
        _ => description.or(tag).map(str::to_owned),
    };
    // AGY 只声明一个上下文窗口，默认值与可覆盖上限都取 maxTokens
    let context_window = model.context_window.filter(|n| *n > 0);
    AccountModelMetadata {
        description,
        context_window_tokens: context_window,
        max_context_window_tokens: context_window,
        max_output_tokens: model.max_output_tokens.filter(|n| *n > 0),
        image_input: model.supports_images == Some(true),
    }
}

fn admin_models(
    catalog: Catalog,
    observed: SystemTime,
) -> Result<ProviderModels, ProviderAdminError> {
    let models = catalog
        .models
        .into_iter()
        .map(|(id, m)| {
            Ok(ProviderModel {
                name: if m.name.is_empty() {
                    id.clone()
                } else {
                    m.name
                },
                id: UpstreamModelId::new(id).map_err(|_| error(AdminKind::Invalid))?,
            })
        })
        .collect::<Result<_, _>>()?;
    Ok(ProviderModels {
        models,
        observed_at: Some(observed.into()),
    })
}

pub(crate) struct CatalogTask(pub(crate) Arc<CatalogService>);
impl gateway_core::task::ScheduledTask for CatalogTask {
    fn run_cycle(
        &self,
        context: gateway_core::task::WorkerCycleContext,
    ) -> futures::future::BoxFuture<'_, Result<(), gateway_core::task::WorkerTaskError>> {
        Box::pin(async move { self.0.reconcile(context.cancellation()).await })
    }
}
impl CatalogService {
    pub(crate) async fn reconcile(
        &self,
        cancellation: &gateway_core::lifecycle::CancellationToken,
    ) -> Result<(), gateway_core::task::WorkerTaskError> {
        use futures::{StreamExt, stream};
        use gateway_core::task::WorkerTaskError;
        let catalogs = self
            .catalogs()
            .await
            .map_err(|e| WorkerTaskError::safe("读取 Antigravity 目录失败").with_source(e))?;
        let observations: BTreeMap<_, _> = catalogs
            .into_iter()
            .flat_map(|(accounts, _)| accounts)
            .map(|a| (a.account_id.clone(), a))
            .collect();
        let mut due = self
            .accounts
            .list_for_provider(&self.kind, false)
            .await
            .map_err(|e| WorkerTaskError::safe("读取 Antigravity 账号失败").with_source(e))?;
        due.retain(|account| {
            account.authentication_kind() == "oauth"
                && observations.get(account.id()).is_none_or(|observation| {
                    observation.revision != account.revision()
                        || observation.observed_at.elapsed().unwrap_or_default()
                            >= Duration::from_secs(3600)
                })
        });
        if due.is_empty() {
            return Ok(());
        }
        due.sort_by(|a, b| a.id().cmp(b.id()));
        // 固定大小轮转批次，持续失败的账号不能阻塞后面的目录修复
        let offset = self.refresh_cursor.fetch_add(32, Ordering::Relaxed) % due.len();
        due.rotate_left(offset);
        due.truncate(32);
        let mut updates=stream::iter(due).map(|account|async move {
            tokio::select! {biased;_=cancellation.cancelled()=>Ok(()),result=self.account_models(account.id(),true)=>result.map(|_|())}
        }).buffer_unordered(4);
        let mut first_error = None;
        while let Some(result) = updates.next().await {
            if let Err(error) = result
                && !matches!(
                    error.kind(),
                    AdminKind::Conflict | AdminKind::CredentialRefreshRequired
                )
            {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), |error| {
            Err(WorkerTaskError::safe("部分 Antigravity 模型目录刷新失败").with_source(error))
        })
    }
}
