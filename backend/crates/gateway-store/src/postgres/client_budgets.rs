//! 按 Key 串行检查限额，并幂等累计已取得的 USD 费用。

use gateway_admin::model::audit::MutationAuditOperation;
use std::{collections::BTreeMap, sync::Mutex, time::Duration};

use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use gateway_admin::model::{
    MutationContext,
    client_keys::{ClientKeyBudgetMutationOrigin, ClientKeyBudgetPeriod, ResetClientKeyBudget},
};
use gateway_admin::ports::store::AdminStoreResult;
use gateway_core::{
    engine::budget::{
        ClientBudgetCharge, ClientBudgetError, ClientBudgetLimits, ClientBudgetPort,
        ClientBudgetStatus,
    },
    error::{GatewayError, GatewayErrorKind},
    metering::Decimal,
    policy::ClientApiKeyId,
};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{StoreError, StoreResult, mutation_audit, postgres_unavailable};

pub(super) async fn reset_client_key_budget(
    pool: &PgPool,
    command: ResetClientKeyBudget,
    origin: ClientKeyBudgetMutationOrigin,
    context: &MutationContext,
) -> AdminStoreResult<()> {
    let mut tx = match &origin {
        ClientKeyBudgetMutationOrigin::Admin => pool.begin().await.map_err(|_| {
            crate::admin_store_error(
                "client API key budget",
                postgres_unavailable("begin budget reset"),
            )
        })?,
        ClientKeyBudgetMutationOrigin::Plugin(owner) => {
            super::plugins::begin_plugin_mutation(pool, owner).await?
        }
    };
    reset_client_key_budget_in_transaction(&mut tx, &command, context)
        .await
        .map_err(|error| crate::admin_store_error("client API key budget", error))?;
    tx.commit().await.map_err(|_| {
        crate::admin_store_error(
            "client API key budget",
            postgres_unavailable("commit budget reset"),
        )
    })
}

async fn reset_client_key_budget_in_transaction(
    tx: &mut Transaction<'_, Postgres>,
    command: &ResetClientKeyBudget,
    context: &MutationContext,
) -> StoreResult<()> {
    // 与准入、结算共用 Key 行锁，重置边界必须在取得锁之后确定。
    let exists =
        sqlx::query_scalar::<_, String>("select id from client_api_keys where id = $1 for update")
            .bind(command.id.as_str())
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| postgres_unavailable("lock budget reset key"))?;
    if exists.is_none() {
        return Err(StoreError::NotFound {
            entity: "client API key",
            id: command.id.as_str().to_owned(),
        });
    }
    let daily = matches!(
        command.period,
        ClientKeyBudgetPeriod::Daily | ClientKeyBudgetPeriod::All
    );
    let weekly = matches!(
        command.period,
        ClientKeyBudgetPeriod::Weekly | ClientKeyBudgetPeriod::All
    );
    let reset_at = Utc::now();
    // 起止时间收拢到重置边界，窗口保持未开启，同时排除重置前完成的迟到费用。
    sqlx::query(
        "update client_key_budget_windows set
        daily_used_usd = case when $2 then 0 else daily_used_usd end,
        daily_start = case when $2 then $4 else daily_start end,
        daily_end = case when $2 then $4 else daily_end end,
        weekly_used_usd = case when $3 then 0 else weekly_used_usd end,
        weekly_start = case when $3 then $4 else weekly_start end,
        weekly_end = case when $3 then $4 else weekly_end end
        where client_api_key_id = $1",
    )
    .bind(command.id.as_str())
    .bind(daily)
    .bind(weekly)
    .bind(reset_at)
    .execute(&mut **tx)
    .await
    .map_err(|_| postgres_unavailable("reset client budget"))?;
    let mut fields = Vec::new();
    if daily {
        fields.extend([
            "daily_used_usd".to_owned(),
            "daily_start".to_owned(),
            "daily_end".to_owned(),
        ]);
    }
    if weekly {
        fields.extend([
            "weekly_used_usd".to_owned(),
            "weekly_start".to_owned(),
            "weekly_end".to_owned(),
        ]);
    }
    super::append_admin_audit_event_in_transaction(
        tx,
        mutation_audit(
            context,
            MutationAuditOperation::ClientApiKeyResetBudget,
            command.id.as_str(),
            fields,
        ),
        None,
    )
    .await?;
    Ok(())
}

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

    async fn admit_inner(&self, key_id: ClientApiKeyId) -> Result<(), GatewayError> {
        // 短暂存储故障后按原金额重试；进程退出丢失的费用不转成人工核账或阻断 Key。
        let retries = self
            .retry
            .lock()
            .map_err(|_| unavailable())?
            .values()
            .filter(|charge| charge.key_id == key_id)
            .cloned()
            .collect::<Vec<_>>();
        for charge in retries {
            self.settle(charge).await.map_err(|_| unavailable())?;
        }
        let mut tx = self.pool.begin().await.map_err(|_| unavailable())?;
        let row = sqlx::query(
            "select daily_limit_usd::text, weekly_limit_usd::text, enabled
            from client_api_keys where id = $1 for update",
        )
        .bind(key_id.as_str())
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
        advance_windows(&mut tx, key_id.as_str(), now, now)
            .await
            .map_err(|_| unavailable())?;
        if limits.is_limited() {
            let window = sqlx::query(
                "select daily_used_usd::text, weekly_used_usd::text, daily_end, weekly_end
                from client_key_budget_windows where client_api_key_id = $1",
            )
            .bind(key_id.as_str())
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
        tx.commit().await.map_err(|_| unavailable())
    }

    async fn settle_inner(&self, charge: &ClientBudgetCharge) -> Result<(), ClientBudgetError> {
        let mut tx = self.pool.begin().await.map_err(|_| ClientBudgetError)?;
        // 与准入统一先锁 Key，再写窗口和费用，串行化同一 Key 的并发结算。
        let key = sqlx::query_scalar::<_, String>(
            "select id from client_api_keys where id = $1 for update",
        )
        .bind(charge.key_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| ClientBudgetError)?;
        let Some(key) = key else { return Ok(()) }; // 删除 Key 时也会删除其费用记录。
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
    let completed_at = DateTime::<Utc>::from(charge.completed_at);
    // 仅在请求结束时写入费用；请求 ID 冲突时不重复累计。
    let changed = sqlx::query(
        "insert into client_key_charge_events (request_id, client_api_key_id, amount_usd, completed_at)
            values ($1, $2, $3::text::numeric, $4)
            on conflict (request_id) do nothing",
    )
    .bind(charge.request_id.as_str())
    .bind(key)
    .bind(charge.amount_usd.canonical())
    .bind(completed_at)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if changed == 1 {
        advance_windows(tx, key, Utc::now(), completed_at).await?;
        sqlx::query("update client_key_budget_windows set
                daily_used_usd = daily_used_usd + case when $3 >= daily_start and $3 < daily_end then $2::text::numeric else 0 end,
                weekly_used_usd = weekly_used_usd + case when $3 >= weekly_start and $3 < weekly_end then $2::text::numeric else 0 end
                where client_api_key_id = $1")
                .bind(key).bind(charge.amount_usd.canonical()).bind(completed_at)
                .execute(&mut **tx).await?;
    }
    Ok(())
}

impl ClientBudgetPort for PgClientBudgetStore {
    fn admit(&self, key_id: ClientApiKeyId) -> BoxFuture<'_, Result<(), GatewayError>> {
        Box::pin(async move { self.admit_inner(key_id).await })
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
    used_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    // 到期日仍按北京时间零点计算，计费起点不能退回重置前；迟到费用不启动新窗口。
    sqlx::query("insert into client_key_budget_windows
        (client_api_key_id, daily_start, daily_end, weekly_start, weekly_end)
        select $1, day, day + interval '24 hours', day, day + interval '168 hours'
        from (select date_trunc('day', $2::timestamptz at time zone 'Asia/Shanghai') at time zone 'Asia/Shanghai' as day) d
        on conflict (client_api_key_id) do update set
            daily_start = case when client_key_budget_windows.daily_end <= $2 and client_key_budget_windows.daily_start <= $3 then greatest(excluded.daily_start, client_key_budget_windows.daily_start) else client_key_budget_windows.daily_start end,
            daily_end = case when client_key_budget_windows.daily_end <= $2 and client_key_budget_windows.daily_start <= $3 then excluded.daily_end else client_key_budget_windows.daily_end end,
            daily_used_usd = case when client_key_budget_windows.daily_end <= $2 and client_key_budget_windows.daily_start <= $3 then 0 else client_key_budget_windows.daily_used_usd end,
            weekly_start = case when client_key_budget_windows.weekly_end <= $2 and client_key_budget_windows.weekly_start <= $3 then greatest(excluded.weekly_start, client_key_budget_windows.weekly_start) else client_key_budget_windows.weekly_start end,
            weekly_end = case when client_key_budget_windows.weekly_end <= $2 and client_key_budget_windows.weekly_start <= $3 then excluded.weekly_end else client_key_budget_windows.weekly_end end,
            weekly_used_usd = case when client_key_budget_windows.weekly_end <= $2 and client_key_budget_windows.weekly_start <= $3 then 0 else client_key_budget_windows.weekly_used_usd end")
        .bind(key).bind(now).bind(used_at).execute(&mut **tx).await?;
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
    let rows = sqlx::query(
        "select k.id, k.daily_limit_usd::text, k.weekly_limit_usd::text,
        (case when w.daily_end > now() then w.daily_used_usd else 0 end)::text as daily_used,
        (case when w.weekly_end > now() then w.weekly_used_usd else 0 end)::text as weekly_used,
        case when w.daily_end > now() then w.daily_end end as daily_end,
        case when w.weekly_end > now() then w.weekly_end end as weekly_end
        from client_api_keys k left join client_key_budget_windows w on w.client_api_key_id = k.id
        where k.id = any($1)",
    )
    .bind(ids)
    .fetch_all(pool)
    .await
    .map_err(|_| postgres_unavailable("load client budgets"))?;
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
