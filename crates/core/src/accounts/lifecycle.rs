//! 账号生命周期状态转换规则。

use chrono::{DateTime, Utc};

use crate::accounts::model::{Account, AccountStatus};

/// 根据 token 过期时间推导账号运行时状态。
pub fn effective_status(account: &Account, now: DateTime<Utc>) -> AccountStatus {
    if account.status != AccountStatus::Active {
        return account.status;
    }
    if account
        .access_token_expires_at
        .is_some_and(|expires_at| expires_at <= now)
    {
        AccountStatus::Expired
    } else {
        AccountStatus::Active
    }
}
