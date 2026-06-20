//! 配额刷新任务。

use std::{collections::HashMap, sync::Arc, time::Duration};

use codex_proxy_adapters::{
    codex::client::{CodexBackendClient, CodexClientError, CodexRequestContext},
    sqlite::accounts::{SqliteAccountStore, SqliteAccountStoreError},
};
use codex_proxy_core::{
    accounts::{
        model::AccountStatus,
        ports::{AccountStore, AccountStoreError},
    },
    serving::quota::{
        quota_from_usage, quota_snapshot_limit_reached, quota_snapshot_limit_window_seconds,
        quota_snapshot_reset_at,
    },
};
use thiserror::Error;
use tokio::time::{interval, sleep, Instant};
use tracing::{debug, info, warn};

use super::coordinator::SchedulerHandle;

const DEFAULT_INTERVAL_SECS: u64 = 15 * 60;
const MIN_REFRESH_INTERVAL_SECS: u64 = 30 * 60;
const DEFAULT_REQUEST_SPACING_SECS: u64 = 3;

/// 主动配额刷新任务。
pub struct QuotaRefreshTask {
    store: SqliteAccountStore,
    codex: Arc<CodexBackendClient>,
    interval_secs: u64,
    min_refresh_interval_secs: u64,
    request_spacing: Duration,
    installation_id: Option<String>,
}

impl QuotaRefreshTask {
    /// 构造默认配额刷新任务。
    pub fn new(store: SqliteAccountStore, codex: Arc<CodexBackendClient>) -> Self {
        Self {
            store,
            codex,
            interval_secs: DEFAULT_INTERVAL_SECS,
            min_refresh_interval_secs: MIN_REFRESH_INTERVAL_SECS,
            request_spacing: Self::default_request_spacing(),
            installation_id: None,
        }
    }

    /// 返回旧调度器用于错开连续 quota 请求的默认间隔。
    pub fn default_request_spacing() -> Duration {
        Duration::from_secs(DEFAULT_REQUEST_SPACING_SECS)
    }

    /// 使用自定义间隔构造配额刷新任务。
    pub fn with_intervals(
        store: SqliteAccountStore,
        codex: Arc<CodexBackendClient>,
        interval_secs: u64,
        min_refresh_interval_secs: u64,
    ) -> Self {
        Self {
            store,
            codex,
            interval_secs,
            min_refresh_interval_secs,
            request_spacing: Self::default_request_spacing(),
            installation_id: None,
        }
    }

    /// 设置连续 quota 请求之间的错峰间隔。
    pub fn with_request_spacing(mut self, request_spacing: Duration) -> Self {
        self.request_spacing = request_spacing;
        self
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
            let mut ticker = interval(Duration::from_secs(self.interval_secs));
            let mut last_refreshed = HashMap::new();
            info!(interval_secs = self.interval_secs, "quota 刷新任务已启动");

            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        match self.refresh_locked_accounts(&mut last_refreshed).await {
                            Ok(summary) if summary.refreshed > 0 => {
                                info!(
                                    refreshed = summary.refreshed,
                                    failed = summary.failed,
                                    "quota 刷新任务完成"
                                );
                            }
                            Ok(summary) => {
                                debug!(
                                    candidates = summary.candidates,
                                    skipped_recent = summary.skipped_recent,
                                    failed = summary.failed,
                                    "没有需要刷新的 quota 锁定或待验证账号"
                                );
                            }
                            Err(error) => {
                                warn!(error = %error, "quota 刷新任务失败");
                            }
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        info!("quota 刷新任务已关闭");
                        break;
                    }
                }
            }
        });

        SchedulerHandle::new(shutdown_tx)
    }

    /// 执行一次配额锁定或待验证账号刷新。
    pub async fn refresh_locked_accounts_once(
        &self,
    ) -> QuotaRefreshTaskResult<QuotaRefreshSummary> {
        let mut last_refreshed = HashMap::new();
        self.refresh_locked_accounts(&mut last_refreshed).await
    }

    async fn refresh_locked_accounts(
        &self,
        last_refreshed: &mut HashMap<String, Instant>,
    ) -> QuotaRefreshTaskResult<QuotaRefreshSummary> {
        let accounts = self
            .store
            .list_pool_accounts()
            .await
            .map_err(QuotaRefreshTaskError::AccountStore)?;
        let mut summary = QuotaRefreshSummary {
            scanned: accounts.len(),
            ..QuotaRefreshSummary::default()
        };
        let min_interval = Duration::from_secs(self.min_refresh_interval_secs);
        let now = Instant::now();

        let mut candidates = accounts
            .into_iter()
            .filter(|account| {
                account.status == AccountStatus::Active
                    && (account.quota_limit_reached || account.quota_verify_required)
            })
            .peekable();

        while let Some(account) = candidates.next() {
            summary.candidates += 1;
            if last_refreshed
                .get(&account.id)
                .is_some_and(|last| now.duration_since(*last) < min_interval)
            {
                summary.skipped_recent += 1;
                continue;
            }
            last_refreshed.insert(account.id.clone(), now);

            let request_id = uuid::Uuid::new_v4().to_string();
            match self
                .codex
                .fetch_usage(CodexRequestContext {
                    access_token: &account.access_token,
                    account_id: account.account_id.as_deref(),
                    request_id: &request_id,
                    turn_state: None,
                    turn_metadata: None,
                    beta_features: None,
                    include_timing_metrics: None,
                    version: None,
                    codex_window_id: None,
                    parent_thread_id: None,
                    cookie_header: None,
                    installation_id: self.installation_id.as_deref(),
                    session_id: None,
                })
                .await
            {
                Ok(raw) => {
                    let quota = quota_from_usage(&raw);
                    let limit_reached = quota_snapshot_limit_reached(&quota);
                    let reset_at = quota_snapshot_reset_at(&quota);
                    if self
                        .store
                        .apply_quota_snapshot(
                            &account.id,
                            &quota.to_string(),
                            limit_reached,
                            limit_reached.then_some(reset_at).flatten(),
                        )
                        .await?
                    {
                        summary.refreshed += 1;
                    } else {
                        summary.missing += 1;
                    }
                    if let Some(reset_at) = reset_at {
                        self.store
                            .sync_rate_limit_window(
                                &account.id,
                                reset_at,
                                quota_snapshot_limit_window_seconds(&quota),
                            )
                            .await?;
                    }
                }
                Err(error) => {
                    warn!(
                        account_id = %account.id,
                        error = %error,
                        "quota usage 拉取失败"
                    );
                    summary.failed += 1;
                }
            }

            if self.request_spacing > Duration::ZERO && candidates.peek().is_some() {
                sleep(self.request_spacing).await;
            }
        }

        Ok(summary)
    }
}

/// 单次配额刷新摘要。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QuotaRefreshSummary {
    /// 扫描账号数。
    pub scanned: usize,
    /// 配额锁定或待验证候选账号数。
    pub candidates: usize,
    /// 成功刷新账号数。
    pub refreshed: usize,
    /// 最近刷新过而跳过的账号数。
    pub skipped_recent: usize,
    /// 刷新期间账号消失数量。
    pub missing: usize,
    /// 上游失败账号数。
    pub failed: usize,
}

/// 配额刷新任务错误。
#[derive(Debug, Error)]
pub enum QuotaRefreshTaskError {
    /// 账号读取失败。
    #[error("failed to list accounts for quota refresh: {0}")]
    AccountStore(#[from] AccountStoreError),
    /// 账号写入失败。
    #[error("failed to persist quota refresh result: {0}")]
    Store(#[from] SqliteAccountStoreError),
    /// 上游 usage 请求失败。
    #[error("failed to fetch quota usage: {0}")]
    Codex(#[from] CodexClientError),
}

/// 配额刷新任务结果。
pub type QuotaRefreshTaskResult<T> = Result<T, QuotaRefreshTaskError>;
