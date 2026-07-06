//! 运行时 quota refresh 业务服务。

use std::{collections::HashMap, sync::Arc, time::Duration};

use chrono::Utc;
use thiserror::Error;
use tokio::time::{Instant, sleep};
use tracing::warn;

use crate::upstream::accounts::{
    model::AccountStatus,
    pool::RuntimeAccountPoolService,
    quota::{
        quota_from_usage, quota_snapshot_limit_reached, quota_snapshot_limit_window_seconds,
        quota_snapshot_reset_at,
    },
    store::{AccountStore, AccountStoreError, SqliteAccountStore, SqliteAccountStoreError},
};
use crate::upstream::{
    accounts::cookies::SqliteCookieStore,
    transport::{CodexBackendClient, CodexClientError, CodexRequestContext},
};

const MIN_REFRESH_INTERVAL_SECS: u64 = 30 * 60;
const DEFAULT_REQUEST_SPACING_SECS: u64 = 3;
const COOLDOWN_REFRESH_GRACE_SECS: i64 = 30;

/// 运行时 quota refresh 服务。
pub struct RuntimeQuotaRefreshService {
    store: SqliteAccountStore,
    codex: Arc<CodexBackendClient>,
    min_refresh_interval_secs: u64,
    request_spacing: Duration,
    installation_id: Option<String>,
    cookie_store: Option<SqliteCookieStore>,
    account_pool: Option<Arc<RuntimeAccountPoolService>>,
}

impl RuntimeQuotaRefreshService {
    /// 构造默认 quota refresh 服务。
    pub fn new(store: SqliteAccountStore, codex: Arc<CodexBackendClient>) -> Self {
        Self {
            store,
            codex,
            min_refresh_interval_secs: MIN_REFRESH_INTERVAL_SECS,
            request_spacing: Self::default_request_spacing(),
            installation_id: None,
            cookie_store: None,
            account_pool: None,
        }
    }

    fn default_request_spacing() -> Duration {
        Duration::from_secs(DEFAULT_REQUEST_SPACING_SECS)
    }

    /// 使用自定义最小刷新间隔构造 quota refresh 服务。
    pub fn with_min_refresh_interval_secs(
        store: SqliteAccountStore,
        codex: Arc<CodexBackendClient>,
        min_refresh_interval_secs: u64,
    ) -> Self {
        Self {
            store,
            codex,
            min_refresh_interval_secs,
            request_spacing: Self::default_request_spacing(),
            installation_id: None,
            cookie_store: None,
            account_pool: None,
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

    /// 设置 usage 请求可复用的账号 Cookie 存储。
    pub fn with_cookie_store(mut self, cookie_store: SqliteCookieStore) -> Self {
        self.cookie_store = Some(cookie_store);
        self
    }

    /// 设置运行时账号池，用于刷新后同步内存调度状态。
    pub fn with_account_pool(mut self, account_pool: Arc<RuntimeAccountPoolService>) -> Self {
        self.account_pool = Some(account_pool);
        self
    }

    /// 执行一次配额锁定或待验证账号刷新，复用调用方传入的最近刷新记录。
    pub async fn refresh_locked_accounts(
        &self,
        last_refreshed: &mut HashMap<String, Instant>,
    ) -> QuotaRefreshServiceResult<QuotaRefreshSummary> {
        let accounts = self
            .store
            .list_pool_accounts()
            .await
            .map_err(QuotaRefreshServiceError::AccountStore)?;
        let mut summary = QuotaRefreshSummary {
            scanned: accounts.len(),
            ..QuotaRefreshSummary::default()
        };
        let min_interval = Duration::from_secs(self.min_refresh_interval_secs);
        let now = Instant::now();
        let now_utc = Utc::now();

        let mut candidates = accounts
            .into_iter()
            .filter(|account| {
                account.status == AccountStatus::QuotaExhausted
                    || (account.status == AccountStatus::Active
                        && (account.quota_limit_reached || account.quota_verify_required))
            })
            .peekable();

        while let Some(account) = candidates.next() {
            summary.candidates += 1;
            let cooldown_ready = match account.quota_cooldown_until {
                Some(cooldown_until) if !quota_cooldown_ready(cooldown_until, now_utc) => {
                    summary.skipped_cooldown += 1;
                    continue;
                }
                Some(_) => true,
                None => false,
            };
            if !cooldown_ready
                && last_refreshed
                    .get(&account.id)
                    .is_some_and(|last| now.duration_since(*last) < min_interval)
            {
                summary.skipped_recent += 1;
                continue;
            }
            last_refreshed.insert(account.id.clone(), now);

            let request_id = uuid::Uuid::new_v4().to_string();
            let cookie_header = self.usage_cookie_header(&account.id).await;
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
                    cookie_header: cookie_header.as_deref(),
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
                    self.sync_runtime_pool_account(&account.id).await;
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

    async fn usage_cookie_header(&self, account_id: &str) -> Option<String> {
        self.cookie_store
            .as_ref()?
            .cookie_header_for_request(account_id, "chatgpt.com", "/codex/usage")
            .await
            .ok()
            .flatten()
    }

    async fn sync_runtime_pool_account(&self, account_id: &str) {
        let Some(account_pool) = &self.account_pool else {
            return;
        };
        if let Err(error) = account_pool.sync_account_from_repository(account_id).await {
            warn!(
                account_id = %account_id,
                error = %error,
                "failed to sync runtime account pool after quota refresh"
            );
        }
    }
}

fn quota_cooldown_ready(cooldown_until: chrono::DateTime<Utc>, now: chrono::DateTime<Utc>) -> bool {
    cooldown_until
        .checked_add_signed(chrono::Duration::seconds(COOLDOWN_REFRESH_GRACE_SECS))
        .is_some_and(|ready_at| now >= ready_at)
}

/// 单次配额刷新摘要。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QuotaRefreshSummary {
    /// 扫描账号数。
    pub scanned: usize,
    /// 配额锁定或待验证候选账号数。
    pub candidates: usize,
    /// 配额冷却宽限期内跳过的账号数。
    pub skipped_cooldown: usize,
    /// 成功刷新账号数。
    pub refreshed: usize,
    /// 最近刷新过而跳过的账号数。
    pub skipped_recent: usize,
    /// 刷新期间账号消失数量。
    pub missing: usize,
    /// 上游失败账号数。
    pub failed: usize,
}

/// quota refresh 服务错误。
#[derive(Debug, Error)]
pub enum QuotaRefreshServiceError {
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

/// quota refresh 服务结果。
pub type QuotaRefreshServiceResult<T> = Result<T, QuotaRefreshServiceError>;
