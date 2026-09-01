//! 持久账号事实与可丢失冷却事实的统一状态投影。

use std::{collections::BTreeMap, time::SystemTime};

use futures::future::join_all;
use gateway_core::{
    account::{
        AccountStatusFacts, AccountStatusProjection, ProviderAccountId, resolve_account_status,
    },
    provider_ports::ProviderCooldownPort,
};

use super::ProviderAccountSummary;

#[must_use]
pub(crate) fn account_status_projection(
    account: &ProviderAccountSummary,
    now: SystemTime,
    rate_limited_until: Option<SystemTime>,
) -> AccountStatusProjection {
    resolve_account_status(
        &AccountStatusFacts {
            enabled: account.enabled,
            credential_state: account.credential_state,
            access_token_expires_at: account.access_token_expires_at.map(Into::into),
            quota: account.quota,
            rate_limited_until,
            last_error_reason: account.last_error_reason,
            last_error_message: account.last_error_message.clone(),
        },
        now,
    )
}

/// 尽力读取账号当前有效的 429 冷却。
///
/// 冷却属于可丢失的账号级运行时事实：端口不可用、单条读取失败或脏账号 ID
/// 都视为没有冷却，不污染持久账号状态。凭据轮换不会解除上游对同一账号的限流。
pub(crate) async fn load_rate_limited_until(
    cooldowns: Option<&dyn ProviderCooldownPort>,
    accounts: &[ProviderAccountSummary],
    now: SystemTime,
) -> BTreeMap<String, SystemTime> {
    let Some(cooldowns) = cooldowns else {
        return BTreeMap::new();
    };
    let reads = accounts.iter().map(|account| async move {
        let account_id = ProviderAccountId::new(account.id.clone()).ok()?;
        let cooldown = cooldowns.read(&account_id).await.ok().flatten()?;
        (cooldown.until() > now).then(|| (account.id.clone(), cooldown.until()))
    });
    join_all(reads).await.into_iter().flatten().collect()
}
