//! 互联观测与同步计划的事务持久化，领取租约与连接身份共同阻止迟到写入

use std::time::{Duration, SystemTime};

use gateway_core::account::{
    UpstreamConnection, UpstreamInfoClaim, UpstreamInfoCompletion, UpstreamInfoFailure,
    UpstreamInfoPlan, UpstreamInfoRecord, UpstreamInfoStore,
};
use sqlx::postgres::PgRow;

use super::*;

const MAX_SNAPSHOT_BYTES: usize = 4 * 1024 * 1024;
const PLAN_COLUMNS: &str = "account_id, credential_revision, connection_revision,
    request_generation, completed_generation, next_attempt_at, claimed_until,
    last_attempt_at, last_success_at, last_error, consecutive_failures";

#[async_trait]
impl UpstreamInfoStore for PgProviderAccountRepository {
    async fn list_candidates(
        &self,
        provider: &ProviderKind,
        after: Option<&CoreProviderAccountId>,
        limit: u32,
    ) -> Result<Vec<LoadedCredential>, CoreStoreError> {
        let ids = sqlx::query_scalar::<_, String>(
            "select id from provider_accounts
             where provider_kind = $1 and authentication_kind = 'api_key'
               and ($2::text is null or id > $2)
             order by id limit $3",
        )
        .bind(provider.as_str())
        .bind(after.map(CoreProviderAccountId::as_str))
        .bind(i64::from(limit.min(200)))
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?;
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query(ACCOUNT_SELECT_BY_IDS)
            .bind(ids)
            .bind(provider.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(database_error)?
            .into_iter()
            .map(|row| {
                let record = account_record_from_row(row).map_err(crate::core_store_error)?;
                Ok(LoadedCredential {
                    account: core_account_from_summary(record.summary)?,
                    credential: PlaintextCredential::new(
                        record.provider_credentials_json.fields().clone(),
                    ),
                })
            })
            .collect()
    }

    async fn reconcile(
        &self,
        connection: &UpstreamConnection,
        enabled: bool,
        now: SystemTime,
    ) -> Result<(), CoreStoreError> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        let account = lock_connection(&mut transaction, connection).await?;
        if enabled && account.enabled() {
            ensure_plan(&mut transaction, connection, now).await?;
        } else {
            sqlx::query("delete from provider_account_upstream_info where account_id = $1")
                .bind(connection.account_id.as_str())
                .execute(&mut *transaction)
                .await
                .map_err(database_error)?;
        }
        transaction.commit().await.map_err(database_error)
    }

    async fn request_refresh(
        &self,
        connection: &UpstreamConnection,
        now: SystemTime,
        minimum_interval: Duration,
    ) -> Result<UpstreamInfoPlan, CoreStoreError> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        if !lock_connection(&mut transaction, connection)
            .await?
            .enabled()
        {
            return Err(CoreStoreError::new(CoreStoreErrorKind::Conflict));
        }
        ensure_plan(&mut transaction, connection, now).await?;
        let current = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {PLAN_COLUMNS} from provider_account_upstream_info
             where account_id = $1 for update"
        )))
        .bind(connection.account_id.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)
        .and_then(|row| plan_from_row(&row))?;
        let record = if current.request_generation > current.completed_generation {
            current
        } else {
            let mut next = current
                .last_attempt_at
                .and_then(|last| last.checked_add(minimum_interval))
                .unwrap_or(now)
                .max(now);
            if current.last_error.is_some() {
                next = next.max(current.next_attempt_at);
            }
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "update provider_account_upstream_info
                 set request_generation = request_generation + 1, next_attempt_at = $2
                 where account_id = $1 returning {PLAN_COLUMNS}"
            )))
            .bind(connection.account_id.as_str())
            .bind(DateTime::<Utc>::from(next))
            .fetch_one(&mut *transaction)
            .await
            .map_err(database_error)
            .and_then(|row| plan_from_row(&row))?
        };
        transaction.commit().await.map_err(database_error)?;
        Ok(record)
    }

    async fn read_plans(
        &self,
        accounts: &[CoreProviderAccountId],
    ) -> Result<Vec<UpstreamInfoPlan>, CoreStoreError> {
        if accounts.is_empty() {
            return Ok(Vec::new());
        }
        let ids = accounts
            .iter()
            .map(CoreProviderAccountId::as_str)
            .collect::<Vec<_>>();
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {PLAN_COLUMNS} from provider_account_upstream_info
             where account_id = any($1::text[])"
        )))
        .bind(ids)
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?
        .iter()
        .map(plan_from_row)
        .collect()
    }

    async fn read(
        &self,
        accounts: &[CoreProviderAccountId],
    ) -> Result<Vec<UpstreamInfoRecord>, CoreStoreError> {
        if accounts.is_empty() {
            return Ok(Vec::new());
        }
        let ids = accounts
            .iter()
            .map(CoreProviderAccountId::as_str)
            .collect::<Vec<_>>();
        sqlx::query(
            "select * from provider_account_upstream_info where account_id = any($1::text[])",
        )
        .bind(ids)
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?
        .into_iter()
        .map(record_from_row)
        .collect()
    }

    async fn claim_due(
        &self,
        provider: &ProviderKind,
        now: SystemTime,
        limit: u32,
        lease_duration: Duration,
    ) -> Result<Vec<UpstreamInfoClaim>, CoreStoreError> {
        let until = now
            .checked_add(lease_duration)
            .filter(|until| *until > now)
            .ok_or_else(|| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?;
        // 领取标识隔离计划删除重建，代次继续用于刷新等待与租约回收
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "with due as (
               select info.account_id as due_account_id from provider_account_upstream_info info
               join provider_accounts account on account.id = info.account_id
               where account.provider_kind = $1 and account.enabled
                 and account.credential_revision = info.credential_revision
                 and info.next_attempt_at <= $2
                 and (info.claimed_until is null or info.claimed_until <= $2)
               order by info.next_attempt_at, info.account_id
               for update of info skip locked limit $3
             )
             update provider_account_upstream_info info
             set request_generation = case when info.claimed_until is not null
                   then info.request_generation + 1
                   else greatest(info.request_generation, info.completed_generation + 1) end,
                 claimed_until = $4, claim_token = gen_random_uuid(), last_attempt_at = $2
             from due where info.account_id = due.due_account_id
             returning {PLAN_COLUMNS}, claim_token"
        )))
        .bind(provider.as_str())
        .bind(DateTime::<Utc>::from(now))
        .bind(i64::from(limit.min(200)))
        .bind(DateTime::<Utc>::from(until))
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?
        .into_iter()
        .map(|row| {
            Ok(UpstreamInfoClaim {
                plan: plan_from_row(&row)?,
                claim_token: row.try_get("claim_token").map_err(database_error)?,
            })
        })
        .collect()
    }

    async fn complete(&self, completion: UpstreamInfoCompletion) -> Result<bool, CoreStoreError> {
        let failure = completion.result.as_ref().err().copied();
        let snapshot = match completion.result {
            Ok(data) => Some(
                JsonObject::try_from_value(
                    "upstream snapshot",
                    serde_json::Value::Object(data.into_inner()),
                    MAX_SNAPSHOT_BYTES,
                )
                .map_err(crate::core_store_error)?,
            ),
            Err(_) => None,
        };
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        let account = match lock_connection(&mut transaction, &completion.connection).await {
            Ok(account) => account,
            Err(error) if error.kind() == CoreStoreErrorKind::Conflict => return Ok(false),
            Err(error) => return Err(error),
        };
        if !account.enabled() {
            return Ok(false);
        }
        let result = sqlx::query(
            "update provider_account_upstream_info
             set completed_generation = request_generation, claimed_until = null, claim_token = null,
                 next_attempt_at = $4, last_error = $5,
                 consecutive_failures = case when $5::text is null then 0
                   else least(consecutive_failures::bigint + 1, 2147483647)::integer end,
                 last_success_at = case when $5::text is null then $6 else last_success_at end,
                 snapshot_json = case when $5::text is null then $7::jsonb
                   when $5 = 'unauthorized' then null else snapshot_json end
             where account_id = $1 and connection_revision = $2 and request_generation = $3
               and claimed_until > $6 and claim_token = $8",
        )
        .bind(completion.connection.account_id.as_str())
        .bind(&completion.connection.revision)
        .bind(
            i64::try_from(completion.request_generation)
                .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?,
        )
        .bind(DateTime::<Utc>::from(completion.next_attempt_at))
        .bind(failure.map(UpstreamInfoFailure::as_str))
        .bind(DateTime::<Utc>::from(completion.completed_at))
        .bind(snapshot.as_ref().map(|value| sqlx::types::Json(value.fields())))
        .bind(completion.claim_token)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(result.rows_affected() == 1)
    }
}

async fn lock_connection(
    transaction: &mut Transaction<'_, Postgres>,
    expected: &UpstreamConnection,
) -> Result<CoreProviderAccount, CoreStoreError> {
    // 托管代理修改会在同一事务更新账号的解析后 URL，账号行锁同时保护出口事实
    let query = format!("{ACCOUNT_SELECT} for update of provider_accounts");
    let row = sqlx::query(sqlx::AssertSqlSafe(query))
        .bind(expected.account_id.as_str())
        .fetch_optional(&mut **transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| CoreStoreError::new(CoreStoreErrorKind::Conflict))?;
    let record = account_record_from_row(row).map_err(crate::core_store_error)?;
    let account = core_account_from_summary(record.summary)?;
    if UpstreamConnection::for_account(&account) != *expected {
        return Err(CoreStoreError::new(CoreStoreErrorKind::Conflict));
    }
    Ok(account)
}

async fn ensure_plan(
    transaction: &mut Transaction<'_, Postgres>,
    connection: &UpstreamConnection,
    now: SystemTime,
) -> Result<(), CoreStoreError> {
    sqlx::query(
        "insert into provider_account_upstream_info
           (account_id, credential_revision, connection_revision, next_attempt_at)
         values ($1, $2, $3, $4)
         on conflict (account_id) do update
         set credential_revision = excluded.credential_revision,
             connection_revision = excluded.connection_revision,
             request_generation = provider_account_upstream_info.request_generation + 1,
             completed_generation = provider_account_upstream_info.request_generation,
             next_attempt_at = excluded.next_attempt_at, claimed_until = null, claim_token = null,
             last_attempt_at = null, last_success_at = null, last_error = null,
             consecutive_failures = 0, snapshot_json = null
         where provider_account_upstream_info.connection_revision <> excluded.connection_revision",
    )
    .bind(connection.account_id.as_str())
    .bind(
        i64::try_from(connection.credential_revision.get())
            .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?,
    )
    .bind(&connection.revision)
    .bind(DateTime::<Utc>::from(now))
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(())
}

fn record_from_row(row: PgRow) -> Result<UpstreamInfoRecord, CoreStoreError> {
    let snapshot: Option<sqlx::types::Json<serde_json::Map<String, serde_json::Value>>> =
        row.try_get("snapshot_json").map_err(database_error)?;
    Ok(UpstreamInfoRecord {
        plan: plan_from_row(&row)?,
        snapshot: snapshot.map(|json| OpaqueProviderData::new(json.0)),
    })
}

fn plan_from_row(row: &PgRow) -> Result<UpstreamInfoPlan, CoreStoreError> {
    let invalid = || CoreStoreError::new(CoreStoreErrorKind::InvalidData);
    let error: Option<String> = row.try_get("last_error").map_err(database_error)?;
    let last_error = match error.as_deref() {
        None => None,
        Some("unauthorized") => Some(UpstreamInfoFailure::Unauthorized),
        Some("unsupported") => Some(UpstreamInfoFailure::Unsupported),
        Some("rate_limited") => Some(UpstreamInfoFailure::RateLimited),
        Some("invalid_response") => Some(UpstreamInfoFailure::InvalidResponse),
        Some("unavailable") => Some(UpstreamInfoFailure::Unavailable),
        Some(_) => return Err(invalid()),
    };
    Ok(UpstreamInfoPlan {
        connection: UpstreamConnection {
            account_id: CoreProviderAccountId::new(
                row.try_get::<String, _>("account_id")
                    .map_err(database_error)?,
            )
            .map_err(|_| invalid())?,
            credential_revision: CoreCredentialRevision::new(read_generation(
                row,
                "credential_revision",
            )?)
            .map_err(|_| invalid())?,
            revision: row.try_get("connection_revision").map_err(database_error)?,
        },
        request_generation: read_generation(row, "request_generation")?,
        completed_generation: read_generation(row, "completed_generation")?,
        next_attempt_at: row
            .try_get::<DateTime<Utc>, _>("next_attempt_at")
            .map_err(database_error)?
            .into(),
        claimed_until: row
            .try_get::<Option<DateTime<Utc>>, _>("claimed_until")
            .map_err(database_error)?
            .map(Into::into),
        last_attempt_at: row
            .try_get::<Option<DateTime<Utc>>, _>("last_attempt_at")
            .map_err(database_error)?
            .map(Into::into),
        last_success_at: row
            .try_get::<Option<DateTime<Utc>>, _>("last_success_at")
            .map_err(database_error)?
            .map(Into::into),
        last_error,
        consecutive_failures: u32::try_from(
            row.try_get::<i32, _>("consecutive_failures")
                .map_err(database_error)?,
        )
        .map_err(|_| invalid())?,
    })
}

fn read_generation(row: &PgRow, column: &str) -> Result<u64, CoreStoreError> {
    u64::try_from(row.try_get::<i64, _>(column).map_err(database_error)?)
        .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::InvalidData))
}

fn database_error(source: sqlx::Error) -> CoreStoreError {
    CoreStoreError::caused_by(CoreStoreErrorKind::Unavailable, source)
}
