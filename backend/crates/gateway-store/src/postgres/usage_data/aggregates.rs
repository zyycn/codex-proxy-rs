//! 在既有聚合表中维护调用方贡献，主键覆盖不追加去重账本

use super::*;
use sqlx::QueryBuilder;
use std::collections::BTreeMap;

pub(crate) const AGGREGATE_COLLECTION: &str = "usage_contribution";
fn dimensions(key: &UsageAggregateKey) -> Vec<String> {
    let d = &key.dimensions;
    [
        &d.provider,
        &d.model,
        &d.account_ref,
        &d.client_key_ref,
        &d.operation,
        &d.transport,
    ]
    .into_iter()
    .map(|v| v.clone().unwrap_or_default())
    .chain(std::iter::once(key.key.clone()))
    .collect()
}
pub(super) async fn write(
    tx: &mut Transaction<'_, Postgres>,
    change: UsageAggregateChange,
) -> StoreResult<UsageAggregateKey> {
    match change {
        UsageAggregateChange::Delete { identity } => {
            sqlx::query("delete from request_analytics_1h where collection=$1 and bucket_start=$2 and dimensions=$3 and outcome=''").bind(AGGREGATE_COLLECTION).bind(at(identity.bucket_start_ms)?).bind(dimensions(&identity)).execute(&mut **tx).await.map_err(db_error)?;
            Ok(identity)
        }
        UsageAggregateChange::Put { aggregate } => {
            let counts: BTreeMap<_, _> = aggregate
                .counts
                .iter()
                .map(|(metric, value)| (*metric, value.get()))
                .collect();
            sqlx::query("insert into request_analytics_1h(bucket_start,collection,dimensions,outcome,schema_version,histogram_version,counts,costs,distributions) values($1,$2,$3,'',0,0,$4,$5,'{}') on conflict(bucket_start,collection,dimensions,outcome) do update set counts=excluded.counts,costs=excluded.costs")
                .bind(at(aggregate.identity.bucket_start_ms)?).bind(AGGREGATE_COLLECTION).bind(dimensions(&aggregate.identity)).bind(sqlx::types::Json(counts)).bind(sqlx::types::Json(aggregate.costs)).execute(&mut **tx).await.map_err(db_error)?;
            Ok(aggregate.identity)
        }
    }
}
pub(super) async fn list(
    tx: &mut Transaction<'_, Postgres>,
    request: ListUsageAggregates,
) -> StoreResult<UsageAggregatePage> {
    let limit = request.limit.unwrap_or(100).min(USAGE_DATA_MAX_PAGE_SIZE);
    let mut q = QueryBuilder::new(
        "select bucket_start,dimensions,counts,costs from request_analytics_1h where collection=",
    );
    q.push_bind(AGGREGATE_COLLECTION)
        .push(" and bucket_start >= ")
        .push_bind(at(request.filter.start_ms)?)
        .push(" and bucket_start < ")
        .push_bind(at(request.filter.end_ms)?);
    for (index, value) in [
        (1, request.filter.provider),
        (2, request.filter.model),
        (3, request.filter.account_ref),
        (4, request.filter.client_key_ref),
    ] {
        if let Some(value) = value {
            q.push(format!(" and dimensions[{index}]="))
                .push_bind(value);
        }
    }
    if let Some(cursor) = request.cursor {
        let identity: UsageAggregateKey =
            serde_json::from_str(&cursor).map_err(|_| invalid("invalid aggregate cursor"))?;
        q.push(" and (bucket_start,dimensions) > (")
            .push_bind(at(identity.bucket_start_ms)?)
            .push(",")
            .push_bind(dimensions(&identity))
            .push(")");
    }
    q.push(" order by bucket_start,dimensions limit ")
        .push_bind(i64::from(limit) + 1);
    let mut rows = q.build().fetch_all(&mut **tx).await.map_err(db_error)?;
    let more = rows.len() > usize::from(limit);
    rows.truncate(usize::from(limit));
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        let d: Vec<String> = row.try_get("dimensions").map_err(db_error)?;
        if d.len() != 7 {
            return Err(invalid("invalid aggregate dimensions"));
        }
        let optional = |i: usize| (!d[i].is_empty()).then(|| d[i].clone());
        let counts: sqlx::types::Json<BTreeMap<UsageMetric, i64>> =
            row.try_get("counts").map_err(db_error)?;
        let costs: sqlx::types::Json<BTreeMap<String, String>> =
            row.try_get("costs").map_err(db_error)?;
        items.push(UsageAggregate {
            identity: UsageAggregateKey {
                bucket_start_ms: row
                    .try_get::<DateTime<Utc>, _>("bucket_start")
                    .map_err(db_error)?
                    .timestamp_millis(),
                dimensions: UsageDimensions {
                    provider: optional(0),
                    model: optional(1),
                    account_ref: optional(2),
                    client_key_ref: optional(3),
                    operation: optional(4),
                    transport: optional(5),
                },
                key: d[6].clone(),
            },
            counts: counts
                .0
                .into_iter()
                .map(|(k, v)| (k, UsageDelta(v)))
                .collect(),
            costs: costs.0,
        });
    }
    let next_cursor = if more {
        items
            .last()
            .map(|item| {
                serde_json::to_string(&item.identity)
                    .map_err(|_| invalid("encode aggregate cursor"))
            })
            .transpose()?
    } else {
        None
    };
    Ok(UsageAggregatePage { items, next_cursor })
}
