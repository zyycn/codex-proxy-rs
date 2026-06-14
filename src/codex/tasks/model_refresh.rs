use std::sync::Arc;
use std::time::Duration;
use tokio::time::{interval, sleep};
use tracing::{info, warn};

use crate::{codex::models::service::ModelService, runtime::tasks::types::SchedulerHandle};

/// 模型刷新调度器 - 定期从 Codex 后端刷新模型列表
///
/// 功能：
/// - 启动后1秒进行首次刷新
/// - 首次刷新失败时每10秒重试，最多12次
/// - 首次成功后，每1小时刷新一次
/// - 为每个计划类型刷新一次模型列表
pub struct ModelRefresher {
    model_service: Arc<ModelService>,
    refresh_interval_secs: u64,
    initial_delay_ms: u64,
    retry_delay_ms: u64,
    max_retries: u32,
}

const DEFAULT_REFRESH_INTERVAL_SECS: u64 = 3600; // 1小时
const INITIAL_DELAY_MS: u64 = 1000; // 1秒
const RETRY_DELAY_MS: u64 = 10_000; // 10秒
const MAX_RETRIES: u32 = 12;

impl ModelRefresher {
    pub fn new(model_service: Arc<ModelService>) -> Self {
        Self {
            model_service,
            refresh_interval_secs: DEFAULT_REFRESH_INTERVAL_SECS,
            initial_delay_ms: INITIAL_DELAY_MS,
            retry_delay_ms: RETRY_DELAY_MS,
            max_retries: MAX_RETRIES,
        }
    }

    /// 启动模型刷新调度器
    pub fn start(self) -> SchedulerHandle {
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel(1);

        tokio::spawn(async move {
            info!("模型刷新器已启动");

            // 初始延迟后开始首次刷新
            sleep(Duration::from_millis(self.initial_delay_ms)).await;

            // 尝试首次刷新，失败时重试
            let mut attempt = 0;
            let mut has_fetched_once = false;

            while attempt < self.max_retries {
                tokio::select! {
                    _ = shutdown_rx.recv() => {
                        info!("模型刷新器在首次拉取前关闭");
                        return;
                    }
                    _ = async {} => {}
                }

                let request_id = uuid::Uuid::new_v4().to_string();
                match self.model_service.refresh_backend_models(&request_id).await {
                    Ok(result) => {
                        info!(
                            refreshed_plans = result.refreshed_plans,
                            model_count = result.model_count,
                            "首次模型刷新成功"
                        );
                        has_fetched_once = true;
                        break;
                    }
                    Err(e) => {
                        attempt += 1;

                        // 如果是 NoAccounts 错误，跳过重试直接进入定期刷新
                        let is_no_accounts = matches!(
                            e,
                            crate::codex::models::service::ModelServiceError::NoAccounts
                        );

                        if is_no_accounts {
                            warn!("首次模型刷新跳过：没有可用账户，将进入周期重试");
                            break;
                        }

                        warn!(
                            attempt,
                            max_retries = self.max_retries,
                            error = ?e,
                            "首次模型刷新失败"
                        );

                        if attempt < self.max_retries {
                            tokio::select! {
                                _ = shutdown_rx.recv() => {
                                    info!("模型刷新器在重试等待期间关闭");
                                    return;
                                }
                                _ = sleep(Duration::from_millis(self.retry_delay_ms)) => {}
                            }
                        }
                    }
                }
            }

            if !has_fetched_once {
                warn!("模型刷新器首次尝试全部失败，切换到周期刷新");
            }

            // 定期刷新
            let mut ticker = interval(Duration::from_secs(self.refresh_interval_secs));
            ticker.tick().await; // 跳过第一次立即触发

            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        self.tick().await;
                    }
                    _ = shutdown_rx.recv() => {
                        info!("模型刷新器正在关闭");
                        break;
                    }
                }
            }
        });

        SchedulerHandle::new(shutdown_tx)
    }

    async fn tick(&self) {
        let request_id = uuid::Uuid::new_v4().to_string();
        match self.model_service.refresh_backend_models(&request_id).await {
            Ok(result) => {
                info!(
                    refreshed_plans = result.refreshed_plans,
                    model_count = result.model_count,
                    failed_plans = result.failed_plans,
                    "模型列表已刷新"
                );
            }
            Err(e) => {
                warn!(error = ?e, "刷新模型列表失败");
            }
        }
    }
}
