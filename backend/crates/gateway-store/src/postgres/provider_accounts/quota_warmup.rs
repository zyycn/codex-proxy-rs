//! 额度桶预热进度的版本读取和原子领取

use super::*;
use gateway_core::account::{QuotaWarmupState, QuotaWarmupStateUpdate};

impl PgProviderAccountRepository {
    pub(super) async fn load_warmup_state(
        &self,
        account: &CoreProviderAccount,
    ) -> StoreResult<Option<QuotaWarmupState>> {
        let row = sqlx::query(
            "select quota_warmup_revision, quota_warmup_state from provider_accounts
             where id = $1 and credential_revision = $2",
        )
        .bind(account.id().as_str())
        .bind(to_i64(account.revision().get())?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| postgres_unavailable("load quota warmup state", error))?;
        row.map(|row| {
            let version: i64 = row
                .try_get("quota_warmup_revision")
                .map_err(|error| postgres_unavailable("read quota warmup revision", error))?;
            let data: serde_json::Value = row
                .try_get("quota_warmup_state")
                .map_err(|error| postgres_unavailable("read quota warmup state", error))?;
            Ok(QuotaWarmupState {
                version: u64::try_from(version)
                    .map_err(|_| invalid("invalid quota warmup revision"))?,
                data: data
                    .as_object()
                    .cloned()
                    .ok_or_else(|| invalid("invalid quota warmup state"))?,
            })
        })
        .transpose()
    }

    pub(super) async fn write_warmup_state(
        &self,
        update: QuotaWarmupStateUpdate<'_>,
    ) -> StoreResult<bool> {
        let data = JsonObject::try_from_value(
            "quota_warmup_state",
            serde_json::Value::Object(update.data),
            QUOTA_MAX_BYTES,
        )?;
        let result = sqlx::query(
            "update provider_accounts set quota_warmup_state = $4,
                 quota_warmup_revision = quota_warmup_revision + 1
             where id = $1 and credential_revision = $2 and quota_warmup_revision = $3
               and quota_settings = $5 and enabled
               and quota_observed_at is not distinct from $6
               and credential_state = $7 and quota_access_state = $8 and quota_paused = $9 and model_access_json = $10",
        )
        .bind(update.account.id().as_str())
        .bind(to_i64(update.account.revision().get())?)
        .bind(to_i64(update.expected_version)?)
        .bind(serde_json::Value::Object(data.fields().clone()))
        .bind(
            serde_json::to_value(update.account.quota_settings())
                .map_err(|_| invalid("invalid quota settings"))?,
        )
        .bind(update.expected_quota_observed_at.map(DateTime::<Utc>::from))
        .bind(update.account.credential_state().as_str())
        .bind(update.account.quota().access().as_str())
        .bind(update.account.quota_paused())
        .bind(serde_json::to_value(update.account.model_access()).map_err(|_| invalid("invalid model access"))?)
        .execute(&self.pool)
        .await
        .map_err(|error| postgres_unavailable("write quota warmup state", error))?;
        Ok(result.rows_affected() == 1)
    }
}
