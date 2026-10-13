//! 请求观测的持久化投影、可重建汇总与受预算约束的读取

pub(crate) mod accounts;
mod aggregate;
mod buckets;
mod counts;
mod forecast;
mod histogram;
mod incremental;
mod metrics;
mod query;
mod reset;
mod worker;

pub(crate) use query::*;
pub use worker::{AnalyticsMaintenance, AnalyticsStatus, PgRequestAnalyticsRepository};

use super::*;
pub(crate) use metrics::MetricState;

const SCHEMA_VERSION: i32 = 1;
const HISTOGRAM_VERSION: i32 = histogram::HISTOGRAM_VERSION;

pub(super) async fn begin_snapshot(
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
