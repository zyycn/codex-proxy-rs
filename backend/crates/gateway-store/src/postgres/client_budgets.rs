//! Durable admission and idempotent USD settlement, serialized per client key.

use std::{collections::BTreeMap, sync::Mutex, time::Duration};

use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use gateway_core::{
    engine::budget::{
        ClientBudgetAdmission, ClientBudgetCharge, ClientBudgetError, ClientBudgetLimits,
        ClientBudgetPort, ClientBudgetStatus,
    },
    error::{GatewayError, GatewayErrorKind},
    metering::Decimal,
};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{StoreResult, postgres_unavailable};

pub struct PgClientBudgetStore {
    pool: PgPool,
    retry: Mutex<BTreeMap<String, ClientBudgetCharge>>,
}

impl PgClientBudgetStore {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            retry: Mutex::new(BTreeMap::new()),
        }
    }

    async fn admit_inner(&self, request: ClientBudgetAdmission) -> Result<(), GatewayError> {
        // Preserve exact charges across a transient outage. On process loss the durable
        // pending event still blocks a limited key after its deadline, requiring reconciliation.
        let retries = self
            .retry
            .lock()
            .map_err(|_| unavailable())?
            .values()
            .cloned()
            .collect::<Vec<_>>();
        if !retries.is_empty() {
            let ids = retries
                .iter()
                .map(|charge| charge.request_id.as_str())
                .collect::<Vec<_>>();
            let owned = sqlx::query_scalar::<_, String>(
                "select request_id from client_key_charge_events
                where client_api_key_id = $1 and request_id = any($2)",
            )
            .bind(request.key_id.as_str())
            .bind(ids)
            .fetch_all(&self.pool)
            .await
            .map_err(|_| unavailable())?;
            for charge in retries {
                if owned.iter().any(|id| id == charge.request_id.as_str()) {
                    self.settle(charge).await.map_err(|_| unavailable())?;
                }
            }
        }
        let mut tx = self.pool.begin().await.map_err(|_| unavailable())?;
        let row = sqlx::query(
            "select daily_limit_usd::text, weekly_limit_usd::text, enabled
            from client_api_keys where id = $1 for update",
        )
        .bind(request.key_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| unavailable())?
        .ok_or_else(|| {
            GatewayError::new(
                GatewayErrorKind::Unauthorized,
                "client API key no longer exists",
            )
        })?;
        if !row.get::<bool, _>("enabled") {
            return Err(GatewayError::new(
                GatewayErrorKind::PolicyDenied,
                "client API key is disabled",
            ));
        }
        let limits = ClientBudgetLimits {
            daily_usd: row
                .get::<String, _>("daily_limit_usd")
                .parse()
                .map_err(|_| unavailable())?,
            weekly_usd: row
                .get::<String, _>("weekly_limit_usd")
                .parse()
                .map_err(|_| unavailable())?,
        };
        let now = Utc::now();
        advance_windows(&mut tx, request.key_id.as_str(), now)
            .await
            .map_err(|_| unavailable())?;
        if limits.is_limited() {
            let unresolved = sqlx::query_scalar::<_, bool>("select exists(select 1 from client_key_charge_events
                where client_api_key_id = $1 and (state = 'unknown' or (state = 'pending' and deadline_at <= $2)))")
                .bind(request.key_id.as_str()).bind(now).fetch_one(&mut *tx).await.map_err(|_| unavailable())?;
            if unresolved {
                return Err(GatewayError::new(
                    GatewayErrorKind::RateLimited,
                    "client API key has unresolved usage; administrator reconciliation is required",
                )
                .with_client_code("key_budget_unresolved"));
            }
            let window = sqlx::query(
                "select daily_used_usd::text, weekly_used_usd::text, daily_end, weekly_end
                from client_key_budget_windows where client_api_key_id = $1",
            )
            .bind(request.key_id.as_str())
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| unavailable())?;
            let daily: Decimal = window
                .get::<String, _>("daily_used_usd")
                .parse()
                .map_err(|_| unavailable())?;
            let weekly: Decimal = window
                .get::<String, _>("weekly_used_usd")
                .parse()
                .map_err(|_| unavailable())?;
            let daily_exceeded = limits.daily_usd != Decimal::ZERO && daily >= limits.daily_usd;
            let weekly_exceeded = limits.weekly_usd != Decimal::ZERO && weekly >= limits.weekly_usd;
            if daily_exceeded || weekly_exceeded {
                let daily_end: DateTime<Utc> = window.get("daily_end");
                let weekly_end: DateTime<Utc> = window.get("weekly_end");
                let reset = if weekly_exceeded {
                    weekly_end
                } else {
                    daily_end
                };
                let retry = (reset - now).to_std().unwrap_or(Duration::from_secs(1));
                return Err(GatewayError::new(
                    GatewayErrorKind::RateLimited,
                    "client API key budget is exhausted",
                )
                .with_client_code(if weekly_exceeded {
                    "key_weekly_budget_exceeded"
                } else {
                    "key_daily_budget_exceeded"
                })
                .with_retry_after(retry));
            }
        }
        sqlx::query(
            "insert into client_key_charge_events (request_id, client_api_key_id, deadline_at)
            values ($1, $2, $3)",
        )
        .bind(request.request_id.as_str())
        .bind(request.key_id.as_str())
        .bind(DateTime::<Utc>::from(request.deadline_at))
        .execute(&mut *tx)
        .await
        .map_err(|_| unavailable())?;
        tx.commit().await.map_err(|_| unavailable())
    }

    async fn settle_inner(&self, charge: &ClientBudgetCharge) -> Result<(), ClientBudgetError> {
        let mut tx = self.pool.begin().await.map_err(|_| ClientBudgetError)?;
        // Match admission's key -> window -> event lock order, including concurrent settlements.
        let key = sqlx::query_scalar::<_, String>(
            "select k.id from client_api_keys k
            join client_key_charge_events e on e.client_api_key_id = k.id
            where e.request_id = $1 for update of k",
        )
        .bind(charge.request_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| ClientBudgetError)?;
        let Some(key) = key else { return Ok(()) }; // Deleting a key also deletes its ledger.
        settle_in_transaction(&mut tx, &key, charge)
            .await
            .map_err(|_| ClientBudgetError)?;
        tx.commit().await.map_err(|_| ClientBudgetError)
    }
}

async fn settle_in_transaction(
    tx: &mut Transaction<'_, Postgres>,
    key: &str,
    charge: &ClientBudgetCharge,
) -> Result<(), sqlx::Error> {
    advance_windows(tx, key, Utc::now()).await?;
    let changed = sqlx::query(
        "update client_key_charge_events
            set state = $2, amount_usd = $3::text::numeric, completed_at = $4
            where request_id = $1 and state <> 'settled'",
    )
    .bind(charge.request_id.as_str())
    .bind(if charge.amount_usd.is_some() {
        "settled"
    } else {
        "unknown"
    })
    .bind(charge.amount_usd.map(|amount| amount.canonical()))
    .bind(DateTime::<Utc>::from(charge.completed_at))
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if changed == 1
        && let Some(amount) = charge.amount_usd
    {
        sqlx::query("update client_key_budget_windows set
                daily_used_usd = daily_used_usd + case when $3 >= daily_start and $3 < daily_end then $2::text::numeric else 0 end,
                weekly_used_usd = weekly_used_usd + case when $3 >= weekly_start and $3 < weekly_end then $2::text::numeric else 0 end
                where client_api_key_id = $1")
                .bind(key).bind(amount.canonical()).bind(DateTime::<Utc>::from(charge.completed_at))
                .execute(&mut **tx).await?;
    }
    Ok(())
}

impl ClientBudgetPort for PgClientBudgetStore {
    fn admit(&self, request: ClientBudgetAdmission) -> BoxFuture<'_, Result<(), GatewayError>> {
        Box::pin(async move { self.admit_inner(request).await })
    }

    fn settle(&self, charge: ClientBudgetCharge) -> BoxFuture<'_, Result<(), ClientBudgetError>> {
        Box::pin(async move {
            let result = self.settle_inner(&charge).await;
            let mut retry = self.retry.lock().map_err(|_| ClientBudgetError)?;
            if result.is_err() {
                retry.insert(charge.request_id.as_str().to_owned(), charge);
            } else {
                retry.remove(charge.request_id.as_str());
            }
            result
        })
    }
}

async fn advance_windows(
    tx: &mut Transaction<'_, Postgres>,
    key: &str,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query("insert into client_key_budget_windows
        (client_api_key_id, daily_start, daily_end, weekly_start, weekly_end)
        select $1, day, day + interval '24 hours', day, day + interval '168 hours'
        from (select date_trunc('day', $2::timestamptz at time zone 'Asia/Shanghai') at time zone 'Asia/Shanghai' as day) d
        on conflict (client_api_key_id) do update set
            daily_start = case when client_key_budget_windows.daily_end <= $2 then excluded.daily_start else client_key_budget_windows.daily_start end,
            daily_end = case when client_key_budget_windows.daily_end <= $2 then excluded.daily_end else client_key_budget_windows.daily_end end,
            daily_used_usd = case when client_key_budget_windows.daily_end <= $2 then 0 else client_key_budget_windows.daily_used_usd end,
            weekly_start = case when client_key_budget_windows.weekly_end <= $2 then excluded.weekly_start else client_key_budget_windows.weekly_start end,
            weekly_end = case when client_key_budget_windows.weekly_end <= $2 then excluded.weekly_end else client_key_budget_windows.weekly_end end,
            weekly_used_usd = case when client_key_budget_windows.weekly_end <= $2 then 0 else client_key_budget_windows.weekly_used_usd end")
        .bind(key).bind(now).execute(&mut **tx).await?;
    Ok(())
}

pub(super) async fn load_client_key_budgets(
    pool: &PgPool,
    records: &mut [super::ClientApiKeyRecord],
) -> StoreResult<()> {
    if records.is_empty() {
        return Ok(());
    }
    let ids = records
        .iter()
        .map(|record| record.id.as_str())
        .collect::<Vec<_>>();
    let rows = sqlx::query("select k.id, k.daily_limit_usd::text, k.weekly_limit_usd::text,
        (case when w.daily_end > now() then w.daily_used_usd else 0 end)::text as daily_used,
        (case when w.weekly_end > now() then w.weekly_used_usd else 0 end)::text as weekly_used,
        case when w.daily_end > now() then w.daily_end end as daily_end,
        case when w.weekly_end > now() then w.weekly_end end as weekly_end,
        (select count(*) from client_key_charge_events e where e.client_api_key_id = k.id
            and (e.state = 'unknown' or (e.state = 'pending' and e.deadline_at <= now()))) as unresolved
        from client_api_keys k left join client_key_budget_windows w on w.client_api_key_id = k.id
        where k.id = any($1)")
        .bind(ids).fetch_all(pool).await.map_err(|_| postgres_unavailable("load client budgets"))?;
    let mut budgets = BTreeMap::new();
    for row in rows {
        let parse = |field| -> StoreResult<Decimal> {
            row.get::<String, _>(field)
                .parse()
                .map_err(|_| postgres_unavailable("decode client budget"))
        };
        budgets.insert(
            row.get::<String, _>("id"),
            ClientBudgetStatus {
                limits: ClientBudgetLimits {
                    daily_usd: parse("daily_limit_usd")?,
                    weekly_usd: parse("weekly_limit_usd")?,
                },
                daily_used_usd: parse("daily_used")?,
                weekly_used_usd: parse("weekly_used")?,
                daily_resets_at: row
                    .get::<Option<DateTime<Utc>>, _>("daily_end")
                    .map(Into::into),
                weekly_resets_at: row
                    .get::<Option<DateTime<Utc>>, _>("weekly_end")
                    .map(Into::into),
                unresolved_requests: row.get::<i64, _>("unresolved").unsigned_abs(),
            },
        );
    }
    for record in records {
        record.budget = budgets
            .remove(&record.id)
            .ok_or_else(|| postgres_unavailable("load client budget policy"))?;
    }
    Ok(())
}

fn unavailable() -> GatewayError {
    GatewayError::new(
        GatewayErrorKind::ProviderInfrastructureUnavailable,
        "client budget service is temporarily unavailable",
    )
    .with_client_code("key_budget_unavailable")
}

pub(super) async fn unresolved_charges(
    pool: &PgPool,
    key: &gateway_core::policy::ClientApiKeyId,
) -> gateway_admin::ports::store::AdminStoreResult<
    Vec<gateway_admin::model::client_keys::UnresolvedClientCharge>,
> {
    let rows = sqlx::query("select request_id, started_at, completed_at, state from client_key_charge_events
        where client_api_key_id = $1 and (state = 'unknown' or (state = 'pending' and deadline_at <= now()))
        order by started_at, request_id limit 200")
        .bind(key.as_str()).fetch_all(pool).await.map_err(|_| budget_admin_unavailable())?;
    Ok(rows
        .into_iter()
        .map(
            |row| gateway_admin::model::client_keys::UnresolvedClientCharge {
                request_id: row.get("request_id"),
                started_at: row.get("started_at"),
                completed_at: row.get("completed_at"),
                state: row.get("state"),
            },
        )
        .collect())
}

pub(super) async fn reconcile_charge(
    pool: &PgPool,
    command: gateway_admin::model::client_keys::ReconcileClientCharge,
    context: &gateway_admin::model::MutationContext,
    revision: gateway_admin::model::Revision,
) -> gateway_admin::ports::store::AdminStoreResult<()> {
    use gateway_admin::ports::store::{AdminStoreError, AdminStoreErrorKind};
    let conflict = || {
        AdminStoreError::new(
            AdminStoreErrorKind::Conflict,
            "client budget",
            "charge is missing, active, or already settled with another amount",
        )
    };
    let mut tx = pool.begin().await.map_err(|_| budget_admin_unavailable())?;
    sqlx::query("select id from client_api_keys where id = $1 for update")
        .bind(command.key_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| budget_admin_unavailable())?
        .ok_or_else(conflict)?;
    let row = sqlx::query("select state, amount_usd::text, coalesce(completed_at, deadline_at) as completed_at, deadline_at <= now() as expired
        from client_key_charge_events where request_id = $1 and client_api_key_id = $2 for update")
        .bind(&command.request_id).bind(command.key_id.as_str()).fetch_optional(&mut *tx).await.map_err(|_| budget_admin_unavailable())?.ok_or_else(conflict)?;
    let state: &str = row.get("state");
    if state == "settled" {
        return if row.get::<String, _>("amount_usd").parse::<Decimal>().ok()
            == Some(command.amount_usd)
        {
            Ok(())
        } else {
            Err(conflict())
        };
    }
    if state == "pending" && !row.get::<bool, _>("expired") {
        return Err(conflict());
    }
    let charge = ClientBudgetCharge {
        request_id: gateway_core::engine::ModelRequestId::new(command.request_id.clone())
            .map_err(|_| conflict())?,
        amount_usd: Some(command.amount_usd),
        completed_at: row.get::<DateTime<Utc>, _>("completed_at").into(),
    };
    settle_in_transaction(&mut tx, command.key_id.as_str(), &charge)
        .await
        .map_err(|_| budget_admin_unavailable())?;
    sqlx::query("update client_key_charge_events set reconciled_at = now(), reconciliation_reason = $2 where request_id = $1")
        .bind(&command.request_id).bind(command.reason).execute(&mut *tx).await.map_err(|_| budget_admin_unavailable())?;
    let audit = crate::mutation_audit(
        context,
        "reconcile",
        "client_key_charge",
        &command.request_id,
        vec!["amount_usd".to_owned(), "reconciliation_reason".to_owned()],
    );
    super::admin_security_audit::append_admin_audit_event_in_transaction(
        &mut tx,
        audit,
        crate::store_revision(revision)?,
    )
    .await
    .map_err(|_| budget_admin_unavailable())?;
    tx.commit().await.map_err(|_| budget_admin_unavailable())
}

fn budget_admin_unavailable() -> gateway_admin::ports::store::AdminStoreError {
    gateway_admin::ports::store::AdminStoreError::new(
        gateway_admin::ports::store::AdminStoreErrorKind::Unavailable,
        "client budget",
        "storage unavailable",
    )
}
