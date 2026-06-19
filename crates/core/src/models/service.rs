//! 模型目录服务用例。

use std::{collections::BTreeMap, sync::Arc};

use thiserror::Error;

use crate::{
    accounts::model::{Account, AccountStatus},
    gateway::ports::{CodexModelCatalogClient, CodexModelCatalogRequest},
    models::{
        catalog::ModelCatalog,
        model::{ModelConfig, ModelPlanSnapshot},
        ports::{ModelSnapshotStore, ModelSnapshotStoreError},
    },
};

/// 模型刷新摘要。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelRefreshResult {
    /// 成功刷新并写入的计划数。
    pub refreshed_plans: usize,
    /// 本次成功写入的模型数。
    pub model_count: usize,
    /// 刷新失败的计划数。
    pub failed_plans: usize,
}

/// 模型服务错误。
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ModelServiceError {
    /// 没有注入快照存储。
    #[error("model snapshot store is unavailable")]
    SnapshotStoreUnavailable,
    /// 没有注入上游模型客户端。
    #[error("model catalog client is unavailable")]
    UpstreamClientUnavailable,
    /// 没有可用账号。
    #[error("no active accounts available for model refresh")]
    NoAccounts,
    /// 快照写入失败。
    #[error("failed to store model snapshot")]
    StoreSnapshot,
    /// 刷新后重新读取快照失败。
    #[error("failed to load model snapshots")]
    LoadSnapshots,
    /// 所有计划都刷新失败。
    #[error("all model refresh plans failed")]
    AllPlansFailed(ModelRefreshResult),
}

/// 模型到可用计划的映射。
pub type ModelPlanAllowlist = BTreeMap<String, Vec<String>>;

/// 可共享更新的模型计划映射缓存。
pub type SharedModelPlanAllowlist = Arc<tokio::sync::Mutex<ModelPlanAllowlist>>;

/// 模型目录服务。
#[derive(Clone)]
pub struct ModelService {
    config: ModelConfig,
    snapshot_store: Option<Arc<dyn ModelSnapshotStore>>,
    upstream_client: Option<Arc<dyn CodexModelCatalogClient>>,
    model_plan_allowlist: Option<SharedModelPlanAllowlist>,
}

impl ModelService {
    /// 构造模型服务。
    pub fn new(
        config: ModelConfig,
        snapshot_store: Option<Arc<dyn ModelSnapshotStore>>,
        upstream_client: Option<Arc<dyn CodexModelCatalogClient>>,
        model_plan_allowlist: Option<SharedModelPlanAllowlist>,
    ) -> Self {
        Self {
            config,
            snapshot_store,
            upstream_client,
            model_plan_allowlist,
        }
    }

    /// 返回模型目录的静态配置。
    pub fn config(&self) -> &ModelConfig {
        &self.config
    }

    /// 构造当前对外暴露的模型目录。
    pub async fn catalog(&self) -> ModelCatalog {
        let Some(snapshot_store) = self.snapshot_store.as_ref() else {
            return ModelCatalog::from_config(&self.config);
        };

        match snapshot_store.list_plan_snapshots().await {
            Ok(snapshots) if !snapshots.is_empty() => {
                ModelCatalog::from_config_and_snapshots(&self.config, &snapshots)
            }
            Ok(_) => ModelCatalog::from_config(&self.config),
            Err(error) => {
                tracing::warn!(error = %error, "加载模型快照失败，回退到静态目录");
                ModelCatalog::from_config(&self.config)
            }
        }
    }

    /// 刷新活跃账号对应的后端模型目录。
    pub async fn refresh_backend_models(
        &self,
        accounts: &[Account],
        request_id: &str,
    ) -> Result<ModelRefreshResult, ModelServiceError> {
        self.refresh_backend_models_with_installation_id(accounts, request_id, None)
            .await
    }

    /// 使用运行时 installation id 刷新活跃账号对应的后端模型目录。
    pub async fn refresh_backend_models_with_installation_id(
        &self,
        accounts: &[Account],
        request_id: &str,
        installation_id: Option<&str>,
    ) -> Result<ModelRefreshResult, ModelServiceError> {
        let snapshot_store = self
            .snapshot_store
            .as_ref()
            .ok_or(ModelServiceError::SnapshotStoreUnavailable)?;
        let upstream_client = self
            .upstream_client
            .as_ref()
            .ok_or(ModelServiceError::UpstreamClientUnavailable)?;

        let plan_accounts = distinct_active_plan_accounts(accounts);
        if plan_accounts.is_empty() {
            return Err(ModelServiceError::NoAccounts);
        }

        let mut result = ModelRefreshResult {
            refreshed_plans: 0,
            model_count: 0,
            failed_plans: 0,
        };

        for (plan_type, account) in plan_accounts {
            let request = CodexModelCatalogRequest {
                access_token: &account.access_token,
                account_id: account.account_id.as_deref(),
                request_id,
                installation_id,
                plan_type: &plan_type,
            };

            let entries = match upstream_client.fetch_models(&request).await {
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
            snapshot_store
                .replace_plan_snapshot(&snapshot)
                .await
                .map_err(map_store_snapshot_error)?;
            result.refreshed_plans += 1;
        }

        if result.refreshed_plans == 0 {
            return Err(ModelServiceError::AllPlansFailed(result));
        }

        let allowlist = self.model_plan_allowlist_from_store().await?;
        if let Some(shared_allowlist) = &self.model_plan_allowlist {
            *shared_allowlist.lock().await = allowlist;
        }

        Ok(result)
    }

    /// 读取当前缓存的 model -> plans allowlist。
    pub async fn model_plan_allowlist(&self) -> Result<ModelPlanAllowlist, ModelServiceError> {
        self.model_plan_allowlist_from_store().await
    }

    async fn model_plan_allowlist_from_store(
        &self,
    ) -> Result<ModelPlanAllowlist, ModelServiceError> {
        let snapshot_store = self
            .snapshot_store
            .as_ref()
            .ok_or(ModelServiceError::SnapshotStoreUnavailable)?;
        let snapshots = snapshot_store
            .list_plan_snapshots()
            .await
            .map_err(map_load_snapshots_error)?;
        Ok(
            ModelCatalog::from_config_and_snapshots(&self.config, &snapshots)
                .model_plan_allowlist(),
        )
    }
}

fn distinct_active_plan_accounts(accounts: &[Account]) -> Vec<(String, Account)> {
    let mut by_plan = BTreeMap::new();

    for account in accounts {
        if account.status != AccountStatus::Active {
            continue;
        }

        let plan_type = account
            .plan_type
            .as_deref()
            .unwrap_or("unknown")
            .to_string();
        by_plan.entry(plan_type).or_insert_with(|| account.clone());
    }

    by_plan.into_iter().collect()
}

fn map_store_snapshot_error(_: ModelSnapshotStoreError) -> ModelServiceError {
    ModelServiceError::StoreSnapshot
}

fn map_load_snapshots_error(_: ModelSnapshotStoreError) -> ModelServiceError {
    ModelServiceError::LoadSnapshots
}
