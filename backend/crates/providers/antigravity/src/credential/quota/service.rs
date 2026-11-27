//! AGY 额度刷新、持久观测与选号投影，凭据版本和观察时间由 Store 隔离

use super::document::Document;
use crate::{
    credential::catalog::CatalogService,
    credential::{CredentialService, oauth::error},
    transport::CloudCodeClient,
};
use futures::{StreamExt, stream};
use gateway_admin::{
    model::provider_credentials::ProviderQuota,
    ports::provider::{ProviderAdminError, ProviderAdminErrorKind as Kind},
};
use gateway_core::{
    account::{
        AccountQuotaSignals, OpaqueProviderData, ProviderAccount, ProviderAccountId,
        ProviderAccountStore, QuotaObservation, QuotaWriteOutcome,
    },
    lifecycle::CancellationToken,
    routing::ProviderKind,
    task::{ScheduledTask, WorkerCycleContext, WorkerTaskError},
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime},
};

pub(crate) struct QuotaService {
    kind: ProviderKind,
    accounts: Arc<dyn ProviderAccountStore>,
    credentials: Arc<CredentialService>,
    catalog: Arc<CatalogService>,
    origin: url::Url,
    cursor: AtomicUsize,
}

impl QuotaService {
    pub(crate) async fn peer_model_facts(
        &self,
        accounts: &[gateway_admin::model::accounts::AccountRecord],
    ) -> Result<
        BTreeMap<String, gateway_admin::ports::provider::ProviderPeerAccountModels>,
        ProviderAdminError,
    > {
        let authorized: BTreeMap<_, _> = accounts
            .iter()
            .filter(|a| a.provider_kind == self.kind && a.authentication_kind == "oauth")
            .map(|a| (a.id.as_str(), a))
            .collect();
        let ids = authorized
            .keys()
            .map(|id| ProviderAccountId::new(*id).map_err(|_| error(Kind::Invalid)))
            .collect::<Result<Vec<_>, _>>()?;
        let quotas: BTreeMap<_, _> = self
            .accounts
            .get_quotas(&ids)
            .await
            .map_err(unavailable)?
            .into_iter()
            .map(|q| (q.account_id.clone(), q))
            .collect();
        let catalogs = self
            .catalog
            .catalogs()
            .await
            .map_err(|e| error(Kind::Unavailable).with_source(e))?;
        let mut facts = BTreeMap::new();
        let now = SystemTime::now();
        for (owners, catalog) in catalogs {
            for owner in owners {
                let Some(account) = authorized
                    .get(owner.account_id.as_str())
                    .filter(|a| a.credential_revision.get() == owner.revision.get())
                else {
                    continue;
                };
                let observation = quotas
                    .get(&owner.account_id)
                    .filter(|q| q.expected_revision == owner.revision);
                let document = observation.map(parse).transpose()?;
                let models = catalog
                    .models
                    .keys()
                    .map(|model| {
                        let quota = observation.zip(document.as_ref()).and_then(|(o, d)| {
                            d.model_observation(
                                model,
                                account.auto_use_quota_credits,
                                o.observed_at,
                                now,
                            )
                        });
                        (model.clone(), quota)
                    })
                    .collect();
                facts.insert(
                    owner.account_id.as_str().to_owned(),
                    gateway_admin::ports::provider::ProviderPeerAccountModels {
                        credential_revision: owner.revision,
                        models,
                    },
                );
            }
        }
        Ok(facts)
    }
    pub(crate) fn new(
        kind: ProviderKind,
        accounts: Arc<dyn ProviderAccountStore>,
        credentials: Arc<CredentialService>,
        catalog: Arc<CatalogService>,
        origin: url::Url,
    ) -> Self {
        Self {
            kind,
            accounts,
            credentials,
            catalog,
            origin,
            cursor: AtomicUsize::new(0),
        }
    }

    pub(crate) async fn read(
        &self,
        id: &ProviderAccountId,
    ) -> Result<ProviderQuota, ProviderAdminError> {
        let account = self
            .accounts
            .get_account(id)
            .await
            .map_err(unavailable)?
            .filter(|a| a.provider() == &self.kind)
            .ok_or_else(|| error(Kind::NotFound))?;
        let observations = self
            .accounts
            .get_quotas(std::slice::from_ref(id))
            .await
            .map_err(unavailable)?;
        if let Some(observation) = observations
            .into_iter()
            .find(|o| &o.account_id == id && o.expected_revision == account.revision())
        {
            let doc = parse(&observation)?;
            return Ok(doc.project(observation.observed_at));
        }
        Ok(ProviderQuota::default())
    }

    pub(crate) async fn refresh(&self, id: &ProviderAccountId) -> Result<(), ProviderAdminError> {
        let current = self.credentials.current(id).await?;
        let material = self
            .credentials
            .material(&current)
            .map_err(|e| error(Kind::CredentialRefreshRequired).with_source(e))?;
        let profile = self.credentials.request_profile().await?;
        let client = CloudCodeClient::new(&self.origin, current.account.outbound_proxy(), &profile)
            .map_err(upstream)?;
        // 用查询开始时间隔离交错返回的旧结果，失败不覆盖已有快照
        let observed_at = SystemTime::now();
        let info = client
            .account_info(&material.access_token)
            .await
            .map_err(upstream)?;
        let (catalog, summary) = tokio::try_join!(
            client.catalog_for_project(&material.access_token, info.project.clone()),
            client.quota_summary(&material.access_token, &info.project),
        )
        .map_err(upstream)?;
        let (tier, plan_description) = match info.paid_tier {
            Some(paid) => (Some(paid.tier), paid.description),
            None => (info.current_tier, None),
        };
        let doc = Document {
            tier,
            plan_description,
            bucket_models: super::document::bucket_models(&summary, &catalog),
            summary,
            models: catalog
                .models
                .iter()
                .map(|(id, model)| {
                    (
                        id.clone(),
                        model
                            .quota
                            .clone()
                            .unwrap_or(crate::transport::account::ModelQuota {
                                remaining_fraction: None,
                                reset_time: None,
                            }),
                    )
                })
                .collect(),
        };
        let plan = doc.plan_type();
        let quota = serde_json::to_value(&doc).map_err(|e| error(Kind::Internal).with_source(e))?;
        let observation = QuotaObservation {
            quota_pause: None,
            expected_connection: Some(gateway_core::account::UpstreamConnection::for_account(
                &current.account,
            )),
            account_id: id.clone(),
            expected_revision: current.account.revision(),
            quota: OpaqueProviderData::new(
                quota
                    .as_object()
                    .cloned()
                    .ok_or_else(|| error(Kind::Internal))?,
            ),
            plan_type: plan.clone(),
            observed_at,
            state: doc.access_with_credits(
                doc.access(observed_at),
                current.account.auto_use_quota_credits(),
                observed_at,
            ),
        };
        if self
            .accounts
            .compare_and_swap_quota(observation)
            .await
            .map_err(unavailable)?
            == QuotaWriteOutcome::Conflict
        {
            return Err(error(Kind::Conflict));
        }
        // 套餐与目录来自同次查询，保存当前套餐的目录避免新套餐短暂失去路由资格
        self.catalog
            .publish(
                &current.account,
                plan.or_else(|| current.account.plan_type().map(str::to_owned)),
                catalog,
                observed_at,
            )
            .await?;
        Ok(())
    }

    pub(crate) async fn scheduling(
        &self,
        accounts: &[ProviderAccount],
        model: &str,
    ) -> Result<BTreeMap<ProviderAccountId, (bool, Option<AccountQuotaSignals>)>, ProviderAdminError>
    {
        let owners: BTreeMap<_, _> = accounts.iter().map(|a| (a.id().clone(), a)).collect();
        let ids: Vec<_> = owners.keys().cloned().collect();
        let observations = self.accounts.get_quotas(&ids).await.map_err(unavailable)?;
        let now = SystemTime::now();
        observations
            .into_iter()
            .filter_map(|o| {
                owners
                    .get(&o.account_id)
                    .filter(|a| a.revision() == o.expected_revision)
                    .map(|account| (o, account))
            })
            .map(|(o, account)| {
                let result = parse(&o)?.model_signals(
                    model,
                    account.auto_use_quota_credits(),
                    o.observed_at,
                    now,
                );
                Ok((o.account_id, result))
            })
            .collect()
    }

    pub(crate) async fn reconcile(
        &self,
        cancel: &CancellationToken,
    ) -> Result<(), WorkerTaskError> {
        let accounts = self
            .accounts
            .list_for_provider(&self.kind, false)
            .await
            .map_err(|e| WorkerTaskError::safe("读取 Antigravity 账号失败").with_source(e))?;
        let ids: Vec<_> = accounts.iter().map(|a| a.id().clone()).collect();
        let mut observations: BTreeMap<_, _> = self
            .accounts
            .get_quota_versions(&ids)
            .await
            .map_err(|e| WorkerTaskError::safe("读取 Antigravity 额度版本失败").with_source(e))?
            .into_iter()
            .map(|o| (o.account_id.clone(), o))
            .collect();
        let now = SystemTime::now();
        let interval = Duration::from_secs(300);
        let stale_before = now - interval;
        let mut due = Vec::new();
        let mut stale = Vec::new();
        for account in accounts
            .iter()
            .filter(|a| a.enabled() && a.authentication_kind() == "oauth")
        {
            let snapshot = observations
                .remove(account.id())
                .filter(|s| s.credential_revision == account.revision());
            match snapshot {
                None => due.push(account.id().clone()),
                Some(_) if account.quota().exhaustion_refresh_due(now, interval) => {
                    due.push(account.id().clone())
                }
                Some(s) if !account.quota().is_exhausted() && s.observed_at <= stale_before => {
                    stale.push(s)
                }
                Some(_) => {}
            }
        }
        let mut outcome = Ok(());
        if !stale.is_empty() {
            // 用量读取失败时，独立执行已选出的首查和耗尽复查
            match self.accounts.accounts_used_since_quota(&stale).await {
                Ok(active) => due.extend(active),
                Err(e) => {
                    outcome =
                        Err(WorkerTaskError::safe("读取 Antigravity 账号使用记录失败")
                            .with_source(e))
                }
            }
        }
        if due.is_empty() {
            return outcome;
        }
        due.sort_unstable();
        let offset = self.cursor.fetch_add(32, Ordering::Relaxed) % due.len();
        due.rotate_left(offset);
        due.truncate(32);
        let mut results = stream::iter(due)
            .map(|id| async move {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => Ok(()),
                    result = self.refresh(&id) => result,
                }
            })
            .buffer_unordered(4);
        while let Some(result) = results.next().await {
            if let Err(e) = result
                && !matches!(e.kind(), Kind::Conflict | Kind::CredentialRefreshRequired)
                && outcome.is_ok()
            {
                outcome =
                    Err(WorkerTaskError::safe("部分 Antigravity 额度刷新失败").with_source(e));
            }
        }
        outcome
    }
}

fn parse(observation: &QuotaObservation) -> Result<Document, ProviderAdminError> {
    serde_json::from_value(serde_json::Value::Object(
        observation.quota.expose_to_provider().clone(),
    ))
    .map_err(|e| error(Kind::Internal).with_source(e))
}
fn unavailable(e: gateway_core::error::StoreError) -> ProviderAdminError {
    error(Kind::Unavailable).with_source(e)
}
fn upstream(e: gateway_core::error::ProviderError) -> ProviderAdminError {
    if e.kind() == gateway_core::error::ProviderErrorKind::Unauthorized {
        error(Kind::CredentialRefreshRequired).with_source(e)
    } else {
        error(Kind::Unavailable)
            .with_public_message("Antigravity 套餐或额度查询失败")
            .with_source(e)
    }
}

pub(crate) struct QuotaTask(pub(crate) Arc<QuotaService>);
impl ScheduledTask for QuotaTask {
    fn run_cycle(
        &self,
        context: WorkerCycleContext,
    ) -> futures::future::BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move { self.0.reconcile(context.cancellation()).await })
    }
}
