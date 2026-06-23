//! 模型刷新任务。

use std::{sync::Arc, time::Duration};

use tokio::time::{interval, sleep};
use tracing::{info, warn};

use crate::upstream::accounts::model::Account;
use crate::upstream::accounts::store::AccountStore;
use crate::upstream::models::{ModelRefreshResult, ModelService, ModelServiceError};

use super::coordinator::SchedulerHandle;

/// 模型刷新任务接线器。
pub struct ModelRefreshTask {
    model_service: Arc<ModelService>,
    account_store: Arc<dyn AccountStore>,
    refresh_interval_secs: u64,
    initial_delay_ms: u64,
    retry_delay_ms: u64,
    max_retries: u32,
    installation_id: Option<String>,
}

const DEFAULT_REFRESH_INTERVAL_SECS: u64 = 3600;
const INITIAL_DELAY_MS: u64 = 1000;
const RETRY_DELAY_MS: u64 = 10_000;
const MAX_RETRIES: u32 = 12;

impl ModelRefreshTask {
    /// 构造默认任务。
    pub fn new(model_service: Arc<ModelService>, account_store: Arc<dyn AccountStore>) -> Self {
        Self {
            model_service,
            account_store,
            refresh_interval_secs: DEFAULT_REFRESH_INTERVAL_SECS,
            initial_delay_ms: INITIAL_DELAY_MS,
            retry_delay_ms: RETRY_DELAY_MS,
            max_retries: MAX_RETRIES,
            installation_id: None,
        }
    }

    /// 设置 Codex installation id。
    pub fn with_installation_id(mut self, installation_id: Option<String>) -> Self {
        self.installation_id = installation_id.filter(|id| !id.trim().is_empty());
        self
    }

    /// 启动后台刷新任务。
    pub fn start(self) -> SchedulerHandle {
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel(1);

        tokio::spawn(async move {
            info!("模型刷新任务已启动");

            sleep(Duration::from_millis(self.initial_delay_ms)).await;

            let mut attempt = 0;
            let mut has_fetched_once = false;

            while attempt < self.max_retries {
                tokio::select! {
                    _ = shutdown_rx.recv() => {
                        info!("模型刷新任务在首次拉取前关闭");
                        return;
                    }
                    _ = async {} => {}
                }

                match self.refresh_once().await {
                    Ok(result) => {
                        info!(
                            refreshed_plans = result.refreshed_plans,
                            model_count = result.model_count,
                            "首次模型刷新成功"
                        );
                        has_fetched_once = true;
                        break;
                    }
                    Err(error) => {
                        attempt += 1;
                        if matches!(error, ModelServiceError::NoAccounts) {
                            warn!("首次模型刷新跳过：没有可用账户，将进入周期重试");
                            break;
                        }

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
                                _ = sleep(Duration::from_millis(self.retry_delay_ms)) => {}
                            }
                        }
                    }
                }
            }

            if !has_fetched_once {
                warn!("模型刷新任务首次尝试全部失败，切换到周期刷新");
            }

            let mut ticker = interval(Duration::from_secs(self.refresh_interval_secs));
            ticker.tick().await;

            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        if let Err(error) = self.refresh_once().await {
                            warn!(error = ?error, "刷新模型列表失败");
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        info!("模型刷新任务正在关闭");
                        break;
                    }
                }
            }
        });

        SchedulerHandle::new(shutdown_tx)
    }

    async fn refresh_once(&self) -> Result<ModelRefreshResult, ModelServiceError> {
        let request_id = uuid::Uuid::new_v4().to_string();
        let accounts = self.load_accounts().await?;
        self.model_service
            .refresh_backend_models_with_installation_id(
                &accounts,
                &request_id,
                self.installation_id.as_deref(),
            )
            .await
    }

    async fn load_accounts(&self) -> Result<Vec<Account>, ModelServiceError> {
        self.account_store
            .list_pool_accounts()
            .await
            .map_err(|error| {
                tracing::warn!(error = %error, "加载账号列表失败");
                ModelServiceError::NoAccounts
            })
    }
}
