//! Admin query service 使用的账号运行态组合 adapter

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gateway_admin::{
    model::accounts::AccountRuntimeSnapshot,
    ports::store::{AccountRuntimeStore, AdminStoreResult},
};

use super::{
    CredentialLeaseRepository as _, RedisCredentialCooldownRepository,
    RedisCredentialLeaseRepository,
};

/// 只组合可丢失 Redis 事实；不持有 PostgreSQL，也不执行状态投影
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
    async fn account_concurrency(
        &self,
        account_id: &str,
        offset: u64,
        limit: u32,
    ) -> AdminStoreResult<gateway_admin::model::accounts::AccountConcurrencyPage> {
        self.leases
            .account_concurrency(account_id, offset, limit)
            .await
            .map_err(|error| crate::admin_store_error("account concurrency", error))
    }

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
        let mut snapshot = self.account_rate_limits(account_ids).await?;
        snapshot.in_flight = self
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
        Ok(snapshot)
    }

    async fn account_rate_limits(
        &self,
        account_ids: &[String],
    ) -> AdminStoreResult<AccountRuntimeSnapshot> {
        self.cooldowns
            .account_cooldowns(account_ids)
            .await
            .map_err(|error| crate::admin_store_error("account runtime", error))
    }

    async fn active_freezes(
        &self,
    ) -> AdminStoreResult<BTreeMap<String, gateway_admin::model::accounts::AccountFreeze>> {
        self.cooldowns
            .active_freezes()
            .await
            .map_err(|error| crate::admin_store_error("account runtime", error))
    }

    async fn finish_freeze(
        &self,
        account_id: &str,
        expected: &gateway_admin::model::accounts::AccountFreeze,
        postpone_until: Option<DateTime<Utc>>,
    ) -> AdminStoreResult<bool> {
        self.cooldowns
            .finish_freeze(account_id, expected, postpone_until)
            .await
            .map_err(|error| crate::admin_store_error("account runtime", error))
    }
}
