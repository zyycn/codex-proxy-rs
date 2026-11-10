//! 读取 Key 预算快照，并按 Key 幂等累计已取得的 USD 费用

use gateway_admin::model::audit::MutationAuditOperation;
use std::{collections::BTreeMap, time::Duration};

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
        ClientKeyBudgetMutationOrigin::Admin => pool.begin().await.map_err(|source| {
            crate::admin_store_error(
                "client API key budget",
                postgres_unavailable("begin budget reset", source),
            )
        })?,
        ClientKeyBudgetMutationOrigin::Plugin(owner) => {
            super::plugins::begin_plugin_mutation(pool, owner).await?
        }
    };
    reset_client_key_budget_in_transaction(&mut tx, &command, context)
        .await
        .map_err(|error| crate::admin_store_error("client API key budget", error))?;
    tx.commit().await.map_err(|source| {
        crate::admin_store_error(
            "client API key budget",
            postgres_unavailable("commit budget reset", source),
        )
    })
}

async fn reset_client_key_budget_in_transaction(
    tx: &mut Transaction<'_, Postgres>,
    command: &ResetClientKeyBudget,
    context: &MutationContext,
) -> StoreResult<()> {
    // 与准入、结算共用 Key 行锁，重置边界必须在取得锁之后确定
    let exists =
        sqlx::query_scalar::<_, String>("select id from client_api_keys where id = $1 for update")
            .bind(command.id.as_str())
            .fetch_optional(&mut **tx)
            .await
            .map_err(|source| postgres_unavailable("lock budget reset key", source))?;
    if exists.is_none() {
        return Err(StoreError::NotFound {
            source: None,
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
    // 起止时间收拢到重置边界，窗口保持未开启，同时排除重置前完成的迟到费用
    sqlx::query(
        "insert into client_api_key_budget_windows
        (client_api_key_id,daily_start,daily_end,weekly_start,weekly_end,revision)
        values ($1,$4,$4,$4,$4,1)
        on conflict (client_api_key_id) do update set
        daily_used_usd = case when $2 then 0 else client_api_key_budget_windows.daily_used_usd end,
        daily_start = case when $2 then $4 else client_api_key_budget_windows.daily_start end,
        daily_end = case when $2 then $4 else client_api_key_budget_windows.daily_end end,
        weekly_used_usd = case when $3 then 0 else client_api_key_budget_windows.weekly_used_usd end,
        weekly_start = case when $3 then $4 else client_api_key_budget_windows.weekly_start end,
        weekly_end = case when $3 then $4 else client_api_key_budget_windows.weekly_end end,
        daily_explicit=case when $2 then false else client_api_key_budget_windows.daily_explicit end,
        weekly_explicit=case when $3 then false else client_api_key_budget_windows.weekly_explicit end,
        revision=client_api_key_budget_windows.revision+1, last_operation_request=null",
    )
    .bind(command.id.as_str())
    .bind(daily)
    .bind(weekly)
    .bind(reset_at)
    .execute(&mut **tx)
    .await
    .map_err(|source| postgres_unavailable("reset client budget", source))?;
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
    timezone: gateway_core::time::DeploymentTimeZone,
    pool: PgPool,
}

impl PgClientBudgetStore {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            timezone: Default::default(),
            pool,
        }
    }

    #[must_use]
    pub fn with_timezone(mut self, timezone: gateway_core::time::DeploymentTimeZone) -> Self {
        self.timezone = timezone;
        self
    }

    pub(super) async fn admit_with_pending(
        &self,
        key_id: ClientApiKeyId,
        pending: &[ClientBudgetCharge],
    ) -> Result<(), GatewayError> {
        // 同一语句快照同时读取窗口和已落库请求，避免提交与队列移除之间重复计算费用
        // 普通准入只读，不等待后台结算持有的 Key 行锁
        let ids = pending
            .iter()
            .map(|charge| charge.request_id.as_str())
            .collect::<Vec<_>>();
        let row = sqlx::query(
            "select k.enabled, k.daily_limit_usd::text, k.weekly_limit_usd::text,
                w.daily_start, w.daily_end, w.weekly_start, w.weekly_end,
                coalesce(w.daily_explicit,false) as daily_explicit,
                coalesce(w.weekly_explicit,false) as weekly_explicit,
                coalesce(w.daily_used_usd, 0)::text as daily_used,
                coalesce(w.weekly_used_usd, 0)::text as weekly_used,
                case when k.daily_limit_usd = 0 and k.weekly_limit_usd = 0 then array[]::text[]
                    else array(select request_id from client_api_key_charge_events where request_id = any($2))
                end as persisted
            from client_api_keys k left join client_api_key_budget_windows w on w.client_api_key_id = k.id
            where k.id = $1",
        )
        .bind(key_id.as_str()).bind(ids).fetch_optional(&self.pool).await
        .map_err(|source| unavailable().with_source(source))?
        .ok_or_else(|| GatewayError::new(GatewayErrorKind::Unauthorized, "Client API key no longer exists."))?;
        if !row.get::<bool, _>("enabled") {
            return Err(GatewayError::new(
                GatewayErrorKind::PolicyDenied,
                "Client API key is disabled.",
            ));
        }
        let parse = |field: &str| -> Result<Decimal, GatewayError> {
            row.get::<String, _>(field)
                .parse()
                .map_err(|source| unavailable().with_source(source))
        };
        let limits = ClientBudgetLimits {
            daily_usd: parse("daily_limit_usd")?,
            weekly_usd: parse("weekly_limit_usd")?,
        };
        let now = Utc::now();
        let day = self.timezone.day_start(now).ok_or_else(unavailable)?;
        let window =
            |start: &str, end: &str, used: &str, explicit: &str, days| -> Result<_, GatewayError> {
                let start = row.get::<Option<DateTime<Utc>>, _>(start);
                let end = row.get::<Option<DateTime<Utc>>, _>(end);
                if row.get::<bool, _>(explicit) {
                    let start = start.ok_or_else(unavailable)?;
                    let end = end.ok_or_else(unavailable)?;
                    if now < start || now >= end {
                        return Err(GatewayError::new(
                            GatewayErrorKind::BudgetExhausted,
                            "No active spending window.",
                        )
                        .with_client_code("key_budget_window_inactive"));
                    }
                    return Ok((start, end, parse(used)?, false));
                }
                let reopen = end.is_none_or(|end| end <= now);
                if reopen {
                    Ok((
                        day.max(start.unwrap_or(day)).max(end.unwrap_or(day)),
                        self.timezone
                            .days_after(now, days)
                            .ok_or_else(unavailable)?,
                        Decimal::ZERO,
                        true,
                    ))
                } else {
                    Ok((
                        start.ok_or_else(unavailable)?,
                        end.ok_or_else(unavailable)?,
                        parse(used)?,
                        false,
                    ))
                }
            };
        let (daily_start, daily_end, mut daily, open_daily) = window(
            "daily_start",
            "daily_end",
            "daily_used",
            "daily_explicit",
            1,
        )?;
        let (weekly_start, weekly_end, mut weekly, open_weekly) = window(
            "weekly_start",
            "weekly_end",
            "weekly_used",
            "weekly_explicit",
            7,
        )?;
        if limits.is_limited() {
            let persisted = row
                .get::<Vec<String>, _>("persisted")
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>();
            for charge in pending {
                if persisted.contains(charge.request_id.as_str()) {
                    continue;
                }
                let completed_at = DateTime::<Utc>::from(charge.completed_at);
                if completed_at >= daily_start && completed_at < daily_end {
                    daily = daily
                        .checked_add(charge.amount_usd)
                        .ok_or_else(unavailable)?;
                }
                if completed_at >= weekly_start && completed_at < weekly_end {
                    weekly = weekly
                        .checked_add(charge.amount_usd)
                        .ok_or_else(unavailable)?;
                }
            }
            let access = limits.access(daily, weekly);
            if !access.allowed() {
                let (reset, code, message) = if access.weekly_limit_reached {
                    (
                        weekly_end,
                        "key_weekly_budget_exceeded",
                        "Weekly spending limit reached.",
                    )
                } else {
                    (
                        daily_end,
                        "key_daily_budget_exceeded",
                        "Daily spending limit reached.",
                    )
                };
                return Err(
                    GatewayError::new(GatewayErrorKind::BudgetExhausted, message)
                        .with_client_code(code)
                        .with_retry_after((reset - now).to_std().unwrap_or(Duration::from_secs(1))),
                );
            }
        }
        // 首次使用或窗口到期才写入；与重置、结算保持相同锁序
        if open_daily || open_weekly {
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|source| unavailable().with_source(source))?;
            sqlx::query("select id from client_api_keys where id = $1 for update")
                .bind(key_id.as_str())
                .fetch_optional(&mut *tx)
                .await
                .map_err(|source| unavailable().with_source(source))?
                .ok_or_else(|| {
                    GatewayError::new(
                        GatewayErrorKind::Unauthorized,
                        "Client API key no longer exists.",
                    )
                })?;
            advance_windows(&mut tx, key_id.as_str(), now, now, self.timezone)
                .await
                .map_err(|source| unavailable().with_source(source))?;
            tx.commit()
                .await
                .map_err(|source| unavailable().with_source(source))?;
        }
        Ok(())
    }

    pub(super) async fn settle_charges(
        &self,
        charges: &[ClientBudgetCharge],
    ) -> Result<(), ClientBudgetError> {
        let Some(first) = charges.first() else {
            return Ok(());
        };
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|source| ClientBudgetError(Some(source.into())))?;
        settle_in_transaction(&mut tx, first.key_id.as_str(), charges, self.timezone)
            .await
            .map_err(|source| ClientBudgetError(Some(source.into())))?;
        tx.commit()
            .await
            .map_err(|source| ClientBudgetError(Some(source.into())))
    }
}

async fn settle_in_transaction(
    tx: &mut Transaction<'_, Postgres>,
    key: &str,
    charges: &[ClientBudgetCharge],
    timezone: gateway_core::time::DeploymentTimeZone,
) -> Result<(), sqlx::Error> {
    // 凭据存在时与准入和重置共用 Key 行锁；删除后由准入已开启的窗口串行化迟到结算
    let active = sqlx::query("select 1 from client_api_keys where id = $1 for update")
        .bind(key)
        .fetch_optional(&mut **tx)
        .await?;
    if active.is_none()
        && sqlx::query(
            "select 1 from client_api_key_budget_windows where client_api_key_id = $1 for update",
        )
        .bind(key)
        .fetch_optional(&mut **tx)
        .await?
        .is_none()
    {
        return Ok(());
    }
    let ids = charges
        .iter()
        .map(|charge| charge.request_id.as_str())
        .collect::<Vec<_>>();
    let amounts = charges
        .iter()
        .map(|charge| charge.amount_usd.canonical())
        .collect::<Vec<_>>();
    let times = charges
        .iter()
        .map(|charge| DateTime::<Utc>::from(charge.completed_at))
        .collect::<Vec<_>>();
    // 同一 Key 的小批次共用事务，只累计本次真正插入的事件
    let inserted = sqlx::query(
        "insert into client_api_key_charge_events (request_id, client_api_key_id, amount_usd, completed_at)
            select request_id, $1, amount::numeric, completed_at
            from unnest($2::text[], $3::text[], $4::timestamptz[]) as charges(request_id, amount, completed_at)
            on conflict (request_id) do nothing returning amount_usd::text as amount, completed_at",
    ).bind(key).bind(ids).bind(amounts).bind(times).fetch_all(&mut **tx).await?;
    let amounts = inserted
        .iter()
        .map(|row| row.get::<String, _>("amount"))
        .collect::<Vec<_>>();
    let times = inserted
        .iter()
        .map(|row| row.get::<DateTime<Utc>, _>("completed_at"))
        .collect::<Vec<_>>();
    let Some(latest) = times.iter().max().copied() else {
        return Ok(());
    };
    advance_windows(tx, key, Utc::now(), latest, timezone).await?;
    sqlx::query(
        "update client_api_key_budget_windows w set
        daily_used_usd = daily_used_usd + coalesce((select sum(amount::numeric)
            from unnest($2::text[], $3::timestamptz[]) as charges(amount, completed_at)
            where completed_at >= w.daily_start and completed_at < w.daily_end), 0),
        weekly_used_usd = weekly_used_usd + coalesce((select sum(amount::numeric)
            from unnest($2::text[], $3::timestamptz[]) as charges(amount, completed_at)
            where completed_at >= w.weekly_start and completed_at < w.weekly_end), 0)
        where client_api_key_id = $1",
    )
    .bind(key)
    .bind(amounts)
    .bind(times)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

impl ClientBudgetPort for PgClientBudgetStore {
    fn admit(&self, key_id: ClientApiKeyId) -> BoxFuture<'_, Result<(), GatewayError>> {
        Box::pin(async move { self.admit_with_pending(key_id, &[]).await })
    }

    fn settle(&self, charge: ClientBudgetCharge) -> BoxFuture<'_, Result<(), ClientBudgetError>> {
        Box::pin(async move { self.settle_charges(std::slice::from_ref(&charge)).await })
    }
}

async fn advance_windows(
    tx: &mut Transaction<'_, Postgres>,
    key: &str,
    now: DateTime<Utc>,
    used_at: DateTime<Utc>,
    timezone: gateway_core::time::DeploymentTimeZone,
) -> Result<(), sqlx::Error> {
    // 已打开窗口不因部署时区变化清零；续接起点不能早于旧窗口末端或人工重置边界
    let day = timezone
        .day_start(now)
        .ok_or_else(|| sqlx::Error::Protocol("invalid budget day".to_owned()))?;
    let daily_end = timezone
        .days_after(now, 1)
        .ok_or_else(|| sqlx::Error::Protocol("invalid budget day end".to_owned()))?;
    let weekly_end = timezone
        .days_after(now, 7)
        .ok_or_else(|| sqlx::Error::Protocol("invalid budget week end".to_owned()))?;
    // 未滚动的窗口无需产生新元组与 WAL
    sqlx::query("insert into client_api_key_budget_windows
        (client_api_key_id, daily_start, daily_end, weekly_start, weekly_end)
        select $1, $4, $5, $4, $6
        on conflict (client_api_key_id) do update set
            daily_start = case when not client_api_key_budget_windows.daily_explicit and client_api_key_budget_windows.daily_end <= $2 and client_api_key_budget_windows.daily_start <= $3 then greatest(excluded.daily_start, client_api_key_budget_windows.daily_end, client_api_key_budget_windows.daily_start) else client_api_key_budget_windows.daily_start end,
            daily_end = case when not client_api_key_budget_windows.daily_explicit and client_api_key_budget_windows.daily_end <= $2 and client_api_key_budget_windows.daily_start <= $3 then excluded.daily_end else client_api_key_budget_windows.daily_end end,
            daily_used_usd = case when not client_api_key_budget_windows.daily_explicit and client_api_key_budget_windows.daily_end <= $2 and client_api_key_budget_windows.daily_start <= $3 then 0 else client_api_key_budget_windows.daily_used_usd end,
            weekly_start = case when not client_api_key_budget_windows.weekly_explicit and client_api_key_budget_windows.weekly_end <= $2 and client_api_key_budget_windows.weekly_start <= $3 then greatest(excluded.weekly_start, client_api_key_budget_windows.weekly_end, client_api_key_budget_windows.weekly_start) else client_api_key_budget_windows.weekly_start end,
            weekly_end = case when not client_api_key_budget_windows.weekly_explicit and client_api_key_budget_windows.weekly_end <= $2 and client_api_key_budget_windows.weekly_start <= $3 then excluded.weekly_end else client_api_key_budget_windows.weekly_end end,
            weekly_used_usd = case when not client_api_key_budget_windows.weekly_explicit and client_api_key_budget_windows.weekly_end <= $2 and client_api_key_budget_windows.weekly_start <= $3 then 0 else client_api_key_budget_windows.weekly_used_usd end
        where (not client_api_key_budget_windows.daily_explicit and client_api_key_budget_windows.daily_end <= $2 and client_api_key_budget_windows.daily_start <= $3)
            or (not client_api_key_budget_windows.weekly_explicit and client_api_key_budget_windows.weekly_end <= $2 and client_api_key_budget_windows.weekly_start <= $3)")
        .bind(key).bind(now).bind(used_at).bind(day).bind(daily_end).bind(weekly_end).execute(&mut **tx).await?;
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
    // 起止相等是重置后的未开启窗口，不能因应用时钟领先数据库而投影为活跃预算
    let rows = sqlx::query(
        "select k.id, k.daily_limit_usd::text, k.weekly_limit_usd::text,
        (case when w.daily_explicit or (w.daily_end > w.daily_start and w.daily_end > now()) then w.daily_used_usd else 0 end)::text as daily_used,
        (case when w.weekly_explicit or (w.weekly_end > w.weekly_start and w.weekly_end > now()) then w.weekly_used_usd else 0 end)::text as weekly_used,
        case when w.daily_explicit or (w.daily_end > w.daily_start and w.daily_end > now()) then w.daily_end end as daily_end,
        case when w.weekly_explicit or (w.weekly_end > w.weekly_start and w.weekly_end > now()) then w.weekly_end end as weekly_end
        from client_api_keys k left join client_api_key_budget_windows w on w.client_api_key_id = k.id
        where k.id = any($1)",
    )
    .bind(ids)
    .fetch_all(pool)
    .await
    .map_err(|source| postgres_unavailable("load client budgets", source))?;
    let mut budgets = BTreeMap::new();
    for row in rows {
        let parse = |field| -> StoreResult<Decimal> {
            row.get::<String, _>(field)
                .parse()
                .map_err(|source| postgres_unavailable("decode client budget", source))
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
        record.budget =
            budgets
                .remove(&record.id)
                .ok_or_else(|| crate::StoreError::Unavailable {
                    backend: crate::StoreBackend::PostgreSql,
                    message: "load client budget policy".to_owned(),
                    source: None,
                })?;
    }
    Ok(())
}

pub(super) fn unavailable() -> GatewayError {
    GatewayError::new(
        GatewayErrorKind::ProviderInfrastructureUnavailable,
        "Client budget service is temporarily unavailable.",
    )
    .with_client_code("key_budget_unavailable")
}
