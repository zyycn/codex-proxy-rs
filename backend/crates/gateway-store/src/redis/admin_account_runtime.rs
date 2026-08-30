//! Admin query service 使用的账号运行态组合 adapter。

use std::collections::BTreeMap;

use async_trait::async_trait;
use futures::future::join_all;
use gateway_admin::{
    model::accounts::AccountRuntimeSnapshot,
    ports::store::{AccountRuntimeStore, AdminStoreResult},
};

use super::{
    CredentialCooldownRepository as _, CredentialLeaseRepository as _,
    RedisCredentialCooldownRepository, RedisCredentialLeaseRepository,
};

/// 只组合可丢失 Redis 事实；不持有 PostgreSQL，也不执行状态投影。
#[derive(Clone)]
pub struct RedisAdminAccountRuntimeStore {
    cooldowns: RedisCredentialCooldownRepository,
    leases: RedisCredentialLeaseRepository,
}

impl RedisAdminAccountRuntimeStore {
    #[must_use]
    pub const fn new(
        cooldowns: RedisCredentialCooldownRepository,
        leases: RedisCredentialLeaseRepository,
    ) -> Self {
        Self { cooldowns, leases }
    }
}

#[async_trait]
impl AccountRuntimeStore for RedisAdminAccountRuntimeStore {
    async fn active_rate_limits(&self) -> AdminStoreResult<AccountRuntimeSnapshot> {
        self.cooldowns
            .active_cooldowns()
            .await
            .map_err(|error| crate::admin_store_error("account runtime", error))
    }

    async fn account_runtime(
        &self,
        account_ids: &[String],
    ) -> AdminStoreResult<AccountRuntimeSnapshot> {
        let reads = account_ids.iter().map(|account_id| async move {
            self.cooldowns
                .read_credential_cooldown(account_id)
                .await
                .map(|cooldown| {
                    cooldown.map(|cooldown| (account_id.clone(), cooldown.cooldown_until))
                })
        });
        let mut rate_limited_until = BTreeMap::new();
        for result in join_all(reads).await {
            if let Some((account_id, until)) =
                result.map_err(|error| crate::admin_store_error("account runtime", error))?
            {
                rate_limited_until.insert(account_id, until);
            }
        }
        let in_flight = self
            .leases
            .credential_runtime_signals(account_ids)
            .await
            .ok()
            .map(|signals| {
                signals
                    .into_iter()
                    .map(|signal| (signal.resource_id, u64::from(signal.in_flight)))
                    .collect()
            });
        Ok(AccountRuntimeSnapshot {
            rate_limited_until,
            in_flight,
        })
    }
}
