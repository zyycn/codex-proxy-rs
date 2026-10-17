//! 账号重置卡计划的 PostgreSQL CAS 与配置审计，不维护独立卡片库存

use super::*;
use gateway_admin::{
    model::{
        audit::MutationAuditOperation,
        reset_credit_automation::{ResetCreditAutomation, ResetCreditAutomationState},
    },
    ports::reset_credit_automation::ResetCreditAutomationStore,
};

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredResetCreditAutomation {
    enabled: bool,
    revision: u64,
    state: ResetCreditAutomationState,
}

#[async_trait]
impl ResetCreditAutomationStore for PgAdminAccountStore {
    async fn reset_credit_automation(
        &self,
        account_id: &str,
    ) -> AdminStoreResult<Option<ResetCreditAutomation>> {
        let row = sqlx::query("select reset_credit_automation_json, reset_credit_next_run_at from provider_accounts where id = $1")
            .bind(account_id).fetch_optional(&self.pool).await
            .map_err(|source| automation_error("load reset-credit automation", source))?;
        let Some(row) = row else {
            return Ok(None);
        };
        let stored: Option<sqlx::types::Json<StoredResetCreditAutomation>> = row
            .try_get("reset_credit_automation_json")
            .map_err(|source| automation_error("decode reset-credit automation", source))?;
        stored
            .map(|stored| {
                Ok(ResetCreditAutomation {
                    account_id: account_id.to_owned(),
                    enabled: stored.enabled,
                    revision: stored.revision,
                    next_run_at: row.try_get("reset_credit_next_run_at").map_err(|source| {
                        automation_error("decode reset-credit deadline", source)
                    })?,
                    state: stored.0.state,
                })
            })
            .transpose()
    }

    async fn due_reset_credit_automations(
        &self,
        now: DateTime<Utc>,
        limit: u32,
    ) -> AdminStoreResult<Vec<String>> {
        sqlx::query_scalar("select id from provider_accounts where enabled and reset_credit_automation_json ->> 'enabled' = 'true' and reset_credit_next_run_at <= $1 order by reset_credit_next_run_at, id limit $2")
            .bind(now).bind(i64::from(limit)).fetch_all(&self.pool).await
            .map_err(|source| automation_error("list due reset-credit automation", source))
    }

    async fn save_reset_credit_automation(
        &self,
        record: &ResetCreditAutomation,
        context: Option<&MutationContext>,
    ) -> AdminStoreResult<u64> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|source| automation_error("begin reset-credit automation", source))?;
        // 配置写入统一先锁全局版本，再锁账号计划，避免和运行设置重排相互等待
        let config_revision = if context.is_some() {
            Some(
                bump_config_revision_in_transaction(&mut tx)
                    .await
                    .map_err(|error| admin_store_error("reset-credit automation", error))?,
            )
        } else {
            None
        };
        let expected = i64::try_from(record.revision).map_err(|_| invalid_automation_revision())?;
        let next = expected
            .checked_add(1)
            .and_then(|value| u64::try_from(value).ok())
            .ok_or_else(invalid_automation_revision)?;
        let stored = StoredResetCreditAutomation {
            enabled: record.enabled,
            revision: next,
            state: record.state.clone(),
        };
        let saved = sqlx::query("update provider_accounts set reset_credit_automation_json = $2, reset_credit_next_run_at = $3 where id = $1 and coalesce((reset_credit_automation_json ->> 'revision')::bigint, 0) = $4")
            .bind(&record.account_id).bind(sqlx::types::Json(stored)).bind(record.next_run_at).bind(expected)
            .execute(&mut *tx).await
            .map_err(|source| automation_error("save reset-credit automation", source))?;
        if saved.rows_affected() != 1 {
            return Err(AdminStoreError::new(
                AdminStoreErrorKind::Conflict,
                "reset-credit automation",
                "automation state changed",
            ));
        }
        if let (Some(context), Some(revision)) = (context, config_revision) {
            append_admin_audit_event_in_transaction(
                &mut tx,
                mutation_audit(
                    context,
                    MutationAuditOperation::ProviderAccountUpdate,
                    &record.account_id,
                    vec!["reset_credit_automation".to_owned()],
                ),
                revision,
            )
            .await
            .map_err(|error| admin_store_error("reset-credit automation", error))?;
        }
        tx.commit()
            .await
            .map_err(|source| automation_error("commit reset-credit automation", source))?;
        Ok(next)
    }
}

fn invalid_automation_revision() -> AdminStoreError {
    AdminStoreError::new(
        AdminStoreErrorKind::Invalid,
        "reset-credit automation",
        "invalid revision",
    )
}

fn automation_error(operation: &'static str, source: sqlx::Error) -> AdminStoreError {
    admin_store_error(
        "reset-credit automation",
        postgres_unavailable(operation, source),
    )
}
