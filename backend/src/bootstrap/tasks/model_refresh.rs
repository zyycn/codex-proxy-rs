//! 模型刷新任务。

use std::{sync::Arc, time::Duration};

use chrono::Utc;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tracing::{info, warn};

use crate::fleet::pool::AccountPoolService;
use crate::infra::identity::AccountPseudonymizer;
use crate::models::service::{
    ModelRefreshPlanAccount, ModelRefreshResult, ModelService, ModelServiceError,
};

use super::coordinator::SchedulerHandle;

/// 模型刷新任务接线器。
pub struct ModelRefreshTask {
    model_service: Arc<ModelService>,
    account_pool: Arc<AccountPoolService>,
    initial_delay_ms: u64,
    retry_delay_ms: u64,
    max_retries: u32,
    account_pseudonymizer: Arc<AccountPseudonymizer>,
}

const INITIAL_DELAY_MS: u64 = 1000;
const RETRY_DELAY_MS: u64 = 10_000;
const MAX_RETRIES: u32 = 12;

impl ModelRefreshTask {
    /// 构造默认任务。
    pub fn new(
        model_service: Arc<ModelService>,
        account_pool: Arc<AccountPoolService>,
        account_pseudonymizer: Arc<AccountPseudonymizer>,
    ) -> Self {
        Self {
            model_service,
            account_pool,
            initial_delay_ms: INITIAL_DELAY_MS,
            retry_delay_ms: RETRY_DELAY_MS,
            max_retries: MAX_RETRIES,
            account_pseudonymizer,
        }
    }

    /// 启动一次性首次刷新任务。
    pub fn start(self) -> SchedulerHandle {
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);
        let handle = tokio::spawn(async move {
            self.run_initial_refresh(&mut shutdown_rx).await;
        });
        SchedulerHandle::new(shutdown_tx, handle)
    }

    /// 在 Responses 响应声明模型目录 ETag 变化时异步刷新。
    pub async fn run_etag_refreshes(self) {
        let mut etag_changes = self.model_service.subscribe_models_etag_changes();
        while etag_changes.changed().await.is_ok() {
            // 合并同一批并发响应产生的重复通知。
            sleep(Duration::from_millis(250)).await;
            etag_changes.borrow_and_update();
            match self.refresh_once().await {
                Ok(result) => info!(
                    refreshed_plans = result.refreshed_plans,
                    model_count = result.model_count,
                    "模型 ETag 变化刷新完成"
                ),
                Err(ModelServiceError::NoAccounts) => {
                    info!("模型 ETag 变化刷新跳过：没有可用账号");
                }
                Err(error) => warn!(error = ?error, "模型 ETag 变化刷新失败"),
            }
        }
    }

    /// 执行一次模型刷新并同步账号池模型计划约束。
    pub async fn refresh_once(&self) -> Result<ModelRefreshResult, ModelServiceError> {
        let request_id = uuid::Uuid::new_v4().to_string();
        let selected_accounts = self.account_pool.distinct_plan_accounts(Utc::now()).await;
        let plan_accounts = selected_accounts
            .iter()
            .map(|selected| ModelRefreshPlanAccount {
                plan_type: selected.plan_type.clone(),
                access_token: selected.account.access_token.clone(),
                account_id: selected.account.account_id.clone(),
                installation_id: self
                    .account_pseudonymizer
                    .installation_id(&selected.account.id),
            })
            .collect::<Vec<_>>();

        let result = self
            .model_service
            .refresh_backend_models(&plan_accounts, &request_id)
            .await;
        for selected in selected_accounts {
            selected.release().await;
        }

        let result = result?;
        let routing = self.model_service.model_plan_routing().await?;
        self.account_pool
            .apply_model_plan_routing(routing.allowlist, routing.fetched_plan_types)
            .await;
        Ok(result)
    }

    async fn run_initial_refresh(&self, shutdown_rx: &mut mpsc::Receiver<()>) {
        tokio::select! {
            _ = shutdown_rx.recv() => {
                info!("模型刷新任务在首次拉取前关闭");
                return;
            }
            () = sleep(Duration::from_millis(self.initial_delay_ms)) => {}
        }

        let mut attempt = 0;
        while attempt < self.max_retries {
            match self.refresh_once().await {
                Ok(result) => {
                    info!(
                        refreshed_plans = result.refreshed_plans,
                        model_count = result.model_count,
                        "首次模型刷新成功"
                    );
                    return;
                }
                Err(ModelServiceError::NoAccounts) => {
                    info!("首次模型刷新跳过：没有可用账户，后续由 ETag 或手动操作触发");
                    return;
                }
                Err(error) => {
                    attempt += 1;
                    warn!(
                        attempt,
                        max_retries = self.max_retries,
                        error = ?error,
                        "首次模型刷新失败"
                    );

                    if attempt < self.max_retries {
                        tokio::select! {
                            _ = shutdown_rx.recv() => {
                                info!("模型刷新任务在重试等待期间关闭");
                                return;
                            }
                            () = sleep(Duration::from_millis(self.retry_delay_ms)) => {}
                        }
                    }
                }
            }
        }
        warn!("模型刷新任务首次尝试全部失败，等待 ETag 或手动操作再次触发");
    }
}
