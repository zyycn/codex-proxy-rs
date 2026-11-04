//! 请求观测的持久化投影、可重建汇总与受预算约束的读取

pub(crate) mod accounts;
mod aggregate;
mod buckets;
mod contributions;
mod counts;
mod forecast;
mod histogram;
mod incremental;
mod metrics;
mod query;
mod reset;
mod usage_query;
mod worker;
pub(crate) use usage_query::{declare_usage_statistics, decode_usage_statistics};

pub(crate) use query::*;
pub use worker::{AnalyticsMaintenance, AnalyticsStatus, PgRequestAnalyticsRepository};

use super::*;
pub(crate) use metrics::MetricState;

pub(crate) const AGGREGATE_COLLECTION: &str = "usage_contribution";

const SCHEMA_VERSION: i32 = 1;
const HISTOGRAM_VERSION: i32 = histogram::HISTOGRAM_VERSION;

/// 业务备份只消费已发布小时汇总，重建中的前缀不能作为完整历史导出
pub(crate) async fn backup_projection_ready(
    tx: &mut sqlx::Transaction<'_, Postgres>,
) -> StoreResult<bool> {
    sqlx::query_scalar("select exists(select 1 from request_analytics_build_state where singleton and backfill_complete and reset_phase=0 and schema_version=$1 and histogram_version=$2) and not exists(select 1 from request_analytics_pending_buckets)")
        .bind(SCHEMA_VERSION).bind(HISTOGRAM_VERSION).fetch_one(&mut **tx).await
        .map_err(|error| postgres_unavailable("check backup analytics readiness", error))
}

pub(crate) async fn begin_snapshot(
    connection: &mut ObservabilityConnection,
) -> StoreResult<sqlx::Transaction<'_, Postgres>> {
    let query_timeout = connection.query_timeout();
    let mut transaction = connection
        .begin()
        .await
        .map_err(|error| postgres_unavailable("begin analytics snapshot", error))?;
    sqlx::query("set transaction isolation level repeatable read")
        .execute(&mut *transaction)
        .await
        .map_err(|error| postgres_unavailable("isolate analytics snapshot", error))?;
    sqlx::query("select set_config('statement_timeout',$1,true)")
        .bind(format!("{}ms", query_timeout.as_millis()))
        .execute(&mut *transaction)
        .await
        .map_err(|error| postgres_unavailable("set analytics query deadline", error))?;
    Ok(transaction)
}

fn fact_select() -> String {
    format!(
        "select mr.*, mr.cost_amount::text as cost_amount_text, mr.standard_cost_amount::text as standard_cost_amount_text, mr.no_cache_cost_amount::text as no_cache_cost_amount_text, coalesce(({}),false) as is_completed_usage from request_analytics_facts mr",
        completed_usage_fact_predicate("mr")
    )
}

fn bucket_for(time: DateTime<Utc>, seconds: i64) -> StoreResult<DateTime<Utc>> {
    DateTime::from_timestamp(time.timestamp().div_euclid(seconds) * seconds, 0)
        .ok_or_else(|| invalid("invalid analytics bucket"))
}

fn unavailable(message: &str) -> StoreError {
    StoreError::Unavailable {
        backend: crate::StoreBackend::PostgreSql,
        message: message.to_owned(),
        source: None,
    }
}
