//! 持久账号事实与可丢失冷却事实的统一状态投影

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
    cooldown: Option<gateway_core::account::AccountCooldown>,
) -> AccountStatusProjection {
    resolve_account_status(
        &AccountStatusFacts {
            enabled: account.enabled,
            credential_state: account.credential_state,
            access_token_expires_at: account.access_token_expires_at.map(Into::into),
            quota: account.quota,
            cooldown,
            last_error_reason: account.last_error_reason,
            last_error_message: account.last_error_message.clone(),
        },
        now,
    )
}

/// 尽力读取账号当前有效的 429 冷却
///
/// 冷却属于可丢失的账号级运行时事实：端口不可用、单条读取失败或脏账号 ID
/// 都视为没有冷却，不污染持久账号状态
/// 凭据轮换不会解除上游对同一账号的限流
pub(crate) async fn load_cooldown(
    cooldowns: Option<&dyn ProviderCooldownPort>,
    accounts: &[ProviderAccountStatus],
    now: SystemTime,
) -> BTreeMap<String, gateway_core::account::AccountCooldown> {
    let Some(cooldowns) = cooldowns else {
        return BTreeMap::new();
    };
    let mut result = BTreeMap::new();
    for accounts in accounts.chunks(crate::redis::ACCOUNT_STATE_READ_CONCURRENCY) {
        let reads = accounts.iter().map(|account| async move {
            let account_id = ProviderAccountId::new(account.id.clone()).ok()?;
            let cooldown = cooldowns.read(&account_id).await.ok().flatten()?;
            cooldown
                .scheduling_state()
                .is_active(now)
                .then(|| (account.id.clone(), cooldown.scheduling_state()))
        });
        result.extend(join_all(reads).await.into_iter().flatten());
    }
    result
}

/// Dashboard 仅需状态事实、账号标识和并发覆盖，不读取身份资料与代理位置
pub(crate) struct ProviderAccountStatus {
    pub id: String,
    pub concurrency_limit: Option<gateway_core::account::AccountConcurrencyLimit>,
    pub quota_version: Option<gateway_core::account::QuotaObservationVersion>,
    pub facts: AccountStatusFacts,
}

pub(crate) async fn load_account_statuses<'e, E: sqlx::Executor<'e, Database = sqlx::Postgres>>(
    executor: E,
) -> crate::StoreResult<Vec<ProviderAccountStatus>> {
    use super::rows::{account_status_facts_from_row, get};
    let rows = sqlx::query(
        "select id, enabled, concurrency_limit, credential_revision, quota_observed_at, credential_state, access_token_expires_at,
                quota_access_state, quota_evidence, quota_access_observed_at, quota_reset_at,
                last_error_reason, last_error_message
         from provider_accounts",
    )
    .fetch_all(executor)
    .await
    .map_err(|source| crate::postgres_unavailable("load account status facts", source))?;
    rows.iter()
        .map(|row| {
            let concurrency_limit = get::<Option<i64>>(row, "concurrency_limit")?
                .map(|value| {
                    u32::try_from(value)
                        .ok()
                        .and_then(gateway_core::account::AccountConcurrencyLimit::new)
                        .ok_or_else(|| super::invalid("invalid concurrency_limit"))
                })
                .transpose()?;
            Ok(ProviderAccountStatus {
                quota_version: if concurrency_limit.is_none() {
                    super::quota_version_from_row(row)?
                } else {
                    None
                },
                id: get(row, "id")?,
                concurrency_limit,
                facts: account_status_facts_from_row(row)?,
            })
        })
        .collect()
}
