//! 模型目录刷新服务。

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, RwLock},
};

use thiserror::Error;
use tokio::sync::watch;

use crate::{
    settings::SettingsSnapshot,
    upstream::openai::transport::{CodexModelCatalogClient, CodexModelCatalogRequest},
};

use super::{
    catalog::ModelCatalog,
    store::{ModelSnapshotStore, ModelSnapshotStoreError},
    types::{ModelConfig, ModelPlanSnapshot},
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
    #[error("failed to store model snapshot: {source}")]
    StoreSnapshot {
        #[source]
        source: ModelSnapshotStoreError,
    },
    /// 刷新后重新读取快照失败。
    #[error("failed to load model snapshots: {source}")]
    LoadSnapshots {
        #[source]
        source: ModelSnapshotStoreError,
    },
    /// 所有计划都刷新失败。
    #[error("all model refresh plans failed")]
    AllPlansFailed(ModelRefreshResult),
}

/// 模型到可用计划的映射。
pub type ModelPlanAllowlist = BTreeMap<String, Vec<String>>;

/// 模型调度约束快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelPlanRouting {
    /// 模型到允许订阅计划的映射。
    pub allowlist: ModelPlanAllowlist,
    /// 已成功拉取过模型列表的订阅计划。
    pub fetched_plan_types: BTreeSet<String>,
}

/// 模型刷新用的计划账号。
#[derive(Debug, Clone)]
pub struct ModelRefreshPlanAccount {
    /// 订阅计划类型。
    pub plan_type: String,
    /// 用于访问模型目录的令牌。
    pub access_token: String,
    /// 上游账号标识。
    pub account_id: Option<String>,
}

/// 模型目录服务。
#[derive(Clone)]
pub struct ModelService {
    config: Arc<RwLock<ModelConfig>>,
    snapshots: Arc<RwLock<Vec<ModelPlanSnapshot>>>,
    catalog: Arc<RwLock<ModelCatalog>>,
    store: Option<Arc<dyn ModelSnapshotStore>>,
    upstream_client: Option<Arc<dyn CodexModelCatalogClient>>,
}

impl ModelService {
    /// 构造模型服务。
    pub fn new(
        config: ModelConfig,
        store: Option<Arc<dyn ModelSnapshotStore>>,
        upstream_client: Option<Arc<dyn CodexModelCatalogClient>>,
    ) -> Self {
        let catalog = ModelCatalog::from_config(&config);
        Self {
            config: Arc::new(RwLock::new(config)),
            snapshots: Arc::new(RwLock::new(Vec::new())),
            catalog: Arc::new(RwLock::new(catalog)),
            store,
            upstream_client,
        }
    }

    /// 更新模型服务配置。
    pub fn update_config(&self, config: ModelConfig) {
        *self
            .config
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = config;
        self.rebuild_catalog_from_memory();
    }

    /// 持续接收运行时设置并更新模型别名。
    pub async fn subscribe_settings(
        self: Arc<Self>,
        mut receiver: watch::Receiver<SettingsSnapshot>,
    ) {
        while receiver.changed().await.is_ok() {
            let settings = receiver.borrow_and_update().clone();
            self.update_config(ModelConfig {
                model_aliases: settings.model_aliases,
            });
        }
    }

    /// 从快照存储加载运行时内存模型目录。
    pub async fn reload_from_store(&self) -> Result<(), ModelServiceError> {
        let Some(store) = self.store.as_ref() else {
            self.replace_snapshots(Vec::new());
            return Ok(());
        };
        let snapshots = store
            .list_plan_snapshots()
            .await
            .map_err(map_load_snapshots_error)?;
        self.replace_snapshots(snapshots);
        Ok(())
    }

    /// 构造当前对外暴露的模型目录。
    pub async fn catalog(&self) -> ModelCatalog {
        self.cached_catalog()
    }

    /// 使用运行时 installation id 刷新活跃账号对应的后端模型目录。
    pub async fn refresh_backend_models_with_installation_id(
        &self,
        plan_accounts: &[ModelRefreshPlanAccount],
        request_id: &str,
        installation_id: Option<&str>,
    ) -> Result<ModelRefreshResult, ModelServiceError> {
        let store = self
            .store
            .as_ref()
            .ok_or(ModelServiceError::SnapshotStoreUnavailable)?;
        let upstream_client = self
            .upstream_client
            .as_ref()
            .ok_or(ModelServiceError::UpstreamClientUnavailable)?;

        if plan_accounts.is_empty() {
            return Err(ModelServiceError::NoAccounts);
        }

        let mut result = ModelRefreshResult {
            refreshed_plans: 0,
            model_count: 0,
            failed_plans: 0,
        };

        for plan_account in plan_accounts {
            let request = CodexModelCatalogRequest {
                access_token: &plan_account.access_token,
                account_id: plan_account.account_id.as_deref(),
                request_id,
                installation_id,
                plan_type: &plan_account.plan_type,
            };

            let entries = match upstream_client.fetch_models(&request).await {
                Ok(entries) if !entries.is_empty() => entries,
                Ok(_) => {
                    result.failed_plans += 1;
                    continue;
                }
                Err(error) => {
                    tracing::warn!(error = %error, plan_type = %plan_account.plan_type, "刷新后端模型失败");
                    result.failed_plans += 1;
                    continue;
                }
            };

            let snapshot =
                ModelPlanSnapshot::from_backend_values(plan_account.plan_type.clone(), entries);
            if snapshot.models.is_empty() {
                result.failed_plans += 1;
                continue;
            }
            result.model_count += snapshot.models.len();
            store
                .replace_plan_snapshot(&snapshot)
                .await
                .map_err(map_store_snapshot_error)?;
            result.refreshed_plans += 1;
        }

        if result.refreshed_plans == 0 {
            return Err(ModelServiceError::AllPlansFailed(result));
        }

        self.reload_from_store().await?;

        Ok(result)
    }

    /// 读取当前缓存的 model -> plans allowlist。
    pub async fn model_plan_allowlist(&self) -> Result<ModelPlanAllowlist, ModelServiceError> {
        Ok(self.model_plan_routing().await?.allowlist)
    }

    /// 读取当前缓存的模型调度约束。
    pub async fn model_plan_routing(&self) -> Result<ModelPlanRouting, ModelServiceError> {
        let catalog = self.cached_catalog();
        Ok(ModelPlanRouting {
            allowlist: catalog.model_plan_allowlist(),
            fetched_plan_types: catalog.fetched_plan_types(),
        })
    }

    fn cached_catalog(&self) -> ModelCatalog {
        self.catalog
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn replace_snapshots(&self, snapshots: Vec<ModelPlanSnapshot>) {
        *self
            .snapshots
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = snapshots;
        self.rebuild_catalog_from_memory();
    }

    fn rebuild_catalog_from_memory(&self) {
        let config = self.config_snapshot();
        let snapshots = self
            .snapshots
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let catalog = if snapshots.is_empty() {
            ModelCatalog::from_config(&config)
        } else {
            ModelCatalog::from_config_and_snapshots(&config, &snapshots)
        };
        *self
            .catalog
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = catalog;
    }

    fn config_snapshot(&self) -> ModelConfig {
        self.config
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

fn map_store_snapshot_error(source: ModelSnapshotStoreError) -> ModelServiceError {
    ModelServiceError::StoreSnapshot { source }
}

fn map_load_snapshots_error(source: ModelSnapshotStoreError) -> ModelServiceError {
    ModelServiceError::LoadSnapshots { source }
}
