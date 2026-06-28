//! 账号管理用例辅助。

use chrono::{DateTime, Utc};

use crate::upstream::accounts::model::Account;

/// 账号服务。
#[derive(Debug, Clone, Default)]
pub struct AccountService;

impl AccountService {
    /// 判断账号配额锁定是否已经释放。
    pub fn quota_available_at(
        account: &Account,
        now: DateTime<Utc>,
        skip_quota_limited: bool,
    ) -> bool {
        if !skip_quota_limited || !account.quota_limit_reached {
            return true;
        }
        account
            .quota_cooldown_until
            .is_some_and(|cooldown_until| now >= cooldown_until)
    }

    /// 判断 Cloudflare 冷却是否已经释放。
    pub fn cloudflare_available_at(account: &Account, now: DateTime<Utc>) -> bool {
        account
            .cloudflare_cooldown_until
            .is_none_or(|cooldown_until| now >= cooldown_until)
    }
}
