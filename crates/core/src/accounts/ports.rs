//! 账号领域端口。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::accounts::{
    model::{Account, AccountStatus},
    usage::AccountUsageDelta,
};

/// 账号存储错误。
#[derive(Debug, Error)]
pub enum AccountStoreError {
    /// 底层存储失败。
    #[error("account store operation failed: {message}")]
    OperationFailed {
        /// 错误说明。
        message: String,
    },
}

/// 账号存储结果类型。
pub type AccountStoreResult<T> = Result<T, AccountStoreError>;

/// 提供运行时账号列表的端口。
#[async_trait]
pub trait AccountStore: Send + Sync + 'static {
    /// 列出当前账号池可见的账号。
    async fn list_pool_accounts(&self) -> AccountStoreResult<Vec<Account>>;

    /// 读取单个账号池账号快照。
    async fn get_pool_account(&self, account_id: &str) -> AccountStoreResult<Option<Account>> {
        let accounts = self.list_pool_accounts().await?;
        Ok(accounts
            .into_iter()
            .find(|account| account.id == account_id))
    }

    /// 标记账号进入配额冷却期。
    async fn mark_quota_limited_until(
        &self,
        account_id: &str,
        cooldown_until: DateTime<Utc>,
    ) -> AccountStoreResult<bool>;

    /// 标记账号进入 Cloudflare 冷却期。
    async fn set_cloudflare_cooldown_until(
        &self,
        account_id: &str,
        cooldown_until: DateTime<Utc>,
    ) -> AccountStoreResult<bool>;

    /// 更新账号状态。
    async fn set_status(&self, account_id: &str, status: AccountStatus)
        -> AccountStoreResult<bool>;

    /// 记录账号用量增量。
    async fn record_usage_delta(
        &self,
        account_id: &str,
        usage: AccountUsageDelta,
    ) -> AccountStoreResult<()>;

    /// 读取账号当前配额 JSON。
    async fn get_quota_json(&self, _account_id: &str) -> AccountStoreResult<Option<String>> {
        Ok(None)
    }

    /// 更新账号当前配额 JSON。
    async fn update_quota_json(
        &self,
        _account_id: &str,
        _quota_json: &str,
    ) -> AccountStoreResult<bool> {
        Ok(false)
    }

    /// 应用已经验证过的账号配额快照。
    async fn apply_quota_snapshot(
        &self,
        account_id: &str,
        quota_json: &str,
        limit_reached: bool,
        cooldown_until: Option<DateTime<Utc>>,
    ) -> AccountStoreResult<bool> {
        let _ = (limit_reached, cooldown_until);
        self.update_quota_json(account_id, quota_json).await
    }

    /// 同步账号当前 rate-limit 统计窗口。
    async fn sync_rate_limit_window(
        &self,
        _account_id: &str,
        _reset_at: DateTime<Utc>,
        _limit_window_seconds: Option<u64>,
    ) -> AccountStoreResult<()> {
        Ok(())
    }

    /// 记录账号被用于一次外部请求。
    async fn record_request(&self, account_id: &str) -> AccountStoreResult<()> {
        self.record_usage_delta(
            account_id,
            AccountUsageDelta {
                requests: 1,
                ..AccountUsageDelta::default()
            },
        )
        .await
    }
}
