//! 明确账号事实触发的运行时失效与连接驱逐。

use chrono::{DateTime, Utc};

use crate::{
    fleet::{account::AccountStatus, pool::AccountPoolService},
    upstream::openai::transport::CodexBackendClient,
};

pub(super) enum AccountStateEffect {
    SetStatus(AccountStatus),
    MarkQuotaLimitedUntil(DateTime<Utc>),
}

pub(super) struct AccountStateEffects;

impl AccountStateEffects {
    pub(super) async fn apply(
        account_pool: &AccountPoolService,
        codex: &CodexBackendClient,
        account_id: &str,
        effect: &AccountStateEffect,
    ) {
        match effect {
            AccountStateEffect::SetStatus(status) => {
                account_pool
                    .set_status_immediately(account_id, *status)
                    .await;
            }
            AccountStateEffect::MarkQuotaLimitedUntil(until) => {
                account_pool
                    .mark_quota_limited_until_immediately(account_id, *until)
                    .await;
            }
        }
        codex.evict_websocket_account(account_id).await;
    }
}
