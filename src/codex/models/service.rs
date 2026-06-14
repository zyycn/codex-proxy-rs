use std::sync::Arc;

use tokio::sync::Mutex;

use crate::{
    codex::accounts::{
        model::{Account, AccountStatus},
        pool::AccountPool,
        repository::AccountRepository,
    },
    codex::gateway::fingerprint::model::Fingerprint,
    codex::gateway::transport::http_client::{
        build_reqwest_client, CodexBackendClient, CodexRequestContext,
    },
    codex::models::{
        catalog::{ModelCatalog, ModelPlanSnapshot},
        repository::ModelSnapshotRepository,
    },
    config::AppConfig,
};

#[derive(Clone)]
pub struct ModelService {
    config: Arc<AppConfig>,
    snapshot_repository: Option<ModelSnapshotRepository>,
    account_repository: Option<AccountRepository>,
    account_pool: Arc<Mutex<AccountPool>>,
}

#[derive(Debug, Clone, Copy)]
pub struct ModelRefreshResult {
    pub refreshed_plans: usize,
    pub model_count: usize,
    pub failed_plans: usize,
}

#[derive(Debug)]
pub enum ModelServiceError {
    AccountRepositoryUnavailable,
    ModelRepositoryUnavailable,
    ListAccounts,
    NoAccounts,
    BuildClient,
    StoreSnapshot,
    AllPlansFailed(ModelRefreshResult),
    LoadSnapshots,
}

impl ModelService {
    pub fn new(
        config: Arc<AppConfig>,
        snapshot_repository: Option<ModelSnapshotRepository>,
        account_repository: Option<AccountRepository>,
        account_pool: Arc<Mutex<AccountPool>>,
    ) -> Self {
        Self {
            config,
            snapshot_repository,
            account_repository,
            account_pool,
        }
    }

    pub async fn catalog(&self) -> ModelCatalog {
        let Some(repo) = self.snapshot_repository.as_ref() else {
            return ModelCatalog::from_config(&self.config.model);
        };
        match repo.list_plan_snapshots().await {
            Ok(snapshots) if !snapshots.is_empty() => {
                ModelCatalog::from_config_and_snapshots(&self.config.model, &snapshots)
            }
            Ok(_) => ModelCatalog::from_config(&self.config.model),
            Err(error) => {
                // Cached model snapshots extend the catalog; fallback keeps proxy routes available.
                tracing::warn!(error = %error, "加载缓存模型快照失败");
                ModelCatalog::from_config(&self.config.model)
            }
        }
    }

    pub async fn refresh_backend_models(
        &self,
        request_id: &str,
    ) -> Result<ModelRefreshResult, ModelServiceError> {
        let account_repo = self
            .account_repository
            .as_ref()
            .ok_or(ModelServiceError::AccountRepositoryUnavailable)?;
        let model_repo = self
            .snapshot_repository
            .as_ref()
            .ok_or(ModelServiceError::ModelRepositoryUnavailable)?;

        let accounts = account_repo
            .list_pool_accounts()
            .await
            .map_err(|_| ModelServiceError::ListAccounts)?;
        let plan_accounts = distinct_active_plan_accounts(accounts);
        if plan_accounts.is_empty() {
            return Err(ModelServiceError::NoAccounts);
        }

        let client = build_reqwest_client(self.config.tls.force_http11)
            .map(|client| {
                CodexBackendClient::new(
                    client,
                    self.config.api.base_url.clone(),
                    Fingerprint::default_codex_desktop(),
                )
            })
            .map_err(|_| ModelServiceError::BuildClient)?;

        let mut result = ModelRefreshResult {
            refreshed_plans: 0,
            model_count: 0,
            failed_plans: 0,
        };
        for (plan_type, account) in plan_accounts {
            let context = CodexRequestContext {
                access_token: &account.access_token,
                account_id: account.account_id.as_deref(),
                request_id,
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
            };
            let entries = match client.fetch_models(context).await {
                Ok(entries) if !entries.is_empty() => entries,
                Ok(_) => {
                    result.failed_plans += 1;
                    continue;
                }
                Err(error) => {
                    tracing::warn!(error = %error, plan_type, "刷新后端模型失败");
                    result.failed_plans += 1;
                    continue;
                }
            };
            let snapshot = ModelPlanSnapshot::from_backend_entries(plan_type, entries);
            result.model_count += snapshot.models.len();
            model_repo
                .replace_plan_snapshot(&snapshot)
                .await
                .map_err(|_| ModelServiceError::StoreSnapshot)?;
            result.refreshed_plans += 1;
        }

        if result.refreshed_plans == 0 {
            return Err(ModelServiceError::AllPlansFailed(result));
        }

        let snapshots = model_repo
            .list_plan_snapshots()
            .await
            .map_err(|_| ModelServiceError::LoadSnapshots)?;
        let allowlist = ModelCatalog::from_config_and_snapshots(&self.config.model, &snapshots)
            .model_plan_allowlist();
        self.account_pool
            .lock()
            .await
            .set_model_plan_allowlist(allowlist);

        Ok(result)
    }
}

fn distinct_active_plan_accounts(accounts: Vec<Account>) -> Vec<(String, Account)> {
    let mut by_plan = std::collections::BTreeMap::new();
    for account in accounts {
        if account.status != AccountStatus::Active {
            continue;
        }
        let plan_type = account
            .plan_type
            .as_deref()
            .unwrap_or("unknown")
            .to_string();
        by_plan.entry(plan_type).or_insert(account);
    }
    by_plan.into_iter().collect()
}
