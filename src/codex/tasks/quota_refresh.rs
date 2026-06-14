use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::time::{interval, sleep, Instant};
use tracing::{debug, info, warn};

use crate::{codex::accounts::service::AccountService, runtime::tasks::types::SchedulerHandle};

/// 主动配额刷新调度器
///
/// 定期扫描并刷新配额锁定（limit_reached）的账户，防止账户被"永久锁死"
///
/// 功能：
/// - 每15分钟扫描一次所有账户
/// - 识别配额锁定的账户
/// - 主动调用上游 getUsage() 刷新配额
/// - 每个账户最少30分钟刷新一次（防滥用）
pub struct QuotaRefresher {
    account_service: Arc<AccountService>,
    interval_secs: u64,
    min_refresh_interval_secs: u64,
}

const DEFAULT_INTERVAL_SECS: u64 = 15 * 60; // 15分钟
const MIN_REFRESH_INTERVAL_SECS: u64 = 30 * 60; // 30分钟

impl QuotaRefresher {
    pub fn new(account_service: Arc<AccountService>) -> Self {
        Self {
            account_service,
            interval_secs: DEFAULT_INTERVAL_SECS,
            min_refresh_interval_secs: MIN_REFRESH_INTERVAL_SECS,
        }
    }

    /// 启动配额刷新调度器
    pub fn start(self) -> SchedulerHandle {
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel(1);

        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(self.interval_secs));
            let mut last_refreshed: HashMap<String, Instant> = HashMap::new();

            info!(interval_secs = self.interval_secs, "quota 刷新器已启动");

            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        self.tick(&mut last_refreshed).await;
                    }
                    _ = shutdown_rx.recv() => {
                        info!("quota 刷新器正在关闭");
                        break;
                    }
                }
            }
        });

        SchedulerHandle::new(shutdown_tx)
    }

    async fn tick(&self, last_refreshed: &mut HashMap<String, Instant>) {
        let now = Instant::now();
        let min_interval = Duration::from_secs(self.min_refresh_interval_secs);

        // 获取所有配额锁定的账户
        let accounts_to_refresh = self.account_service.list_quota_locked_accounts().await;

        if accounts_to_refresh.is_empty() {
            debug!("未找到 quota 锁定账户");
            return;
        }

        info!(
            count = accounts_to_refresh.len(),
            "发现需要刷新 quota 的锁定账户"
        );

        for account_id in accounts_to_refresh {
            // 检查是否最近刷新过
            if let Some(&last_time) = last_refreshed.get(&account_id) {
                if now.duration_since(last_time) < min_interval {
                    debug!(
                        account_id = %account_id,
                        "账户最近已刷新，跳过 quota 刷新"
                    );
                    continue;
                }
            }

            info!(account_id = %account_id, "正在主动刷新 quota");

            // 记录刷新时间
            last_refreshed.insert(account_id.clone(), now);

            // 调用配额刷新（使用 health check 的 quota 方法）
            let request_id = uuid::Uuid::new_v4().to_string();
            match self
                .account_service
                .account_quota(&account_id, &request_id)
                .await
            {
                Ok(_) => {
                    info!(account_id = %account_id, "quota 刷新成功");
                }
                Err(e) => {
                    warn!(
                        account_id = %account_id,
                        error = ?e,
                        "quota 刷新失败"
                    );
                }
            }

            // 错开请求，避免突发
            sleep(Duration::from_millis(3000)).await;
        }
    }
}
