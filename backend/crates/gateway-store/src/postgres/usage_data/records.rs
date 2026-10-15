//! 按原生主键维护已结束的请求明细，复用统计失效触发器

use super::*;
use futures::TryStreamExt;
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, postgres::PgRow};

const SELECT: &str = "select id,client_api_key_ref,provider_kind,provider_account_ref,requested_model_id,upstream_model_id,request_observation_json #>> '{request,protocol}' as protocol,request_observation_json #>> '{request,endpoint}' as endpoint,operation,client_transport,service_tier,started_at,completed_at,outcome,input_tokens,output_tokens,cached_input_tokens,cache_write_input_tokens,reasoning_output_tokens,image_input_tokens,image_output_tokens,total_tokens,cost_source,cost_currency,cost_amount::text as cost_amount,billing_snapshot_json,provider_observation_json from model_requests";
#[derive(Serialize, Deserialize)]
struct Cursor {
    started_at: DateTime<Utc>,
    id: String,
}

pub(super) async fn get(tx: &mut Transaction<'_, Postgres>, id: &str) -> StoreResult<UsageRecord> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!("{SELECT} where id=$1")))
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_error)?
        .ok_or_else(|| StoreError::NotFound {
            entity: "usage record",
            id: id.into(),
            source: None,
        })?;
    decode(row)
}
pub(super) async fn list(
    tx: &mut Transaction<'_, Postgres>,
    request: ListUsageRecords,
) -> StoreResult<UsageRecordPage> {
    let limit = request.limit.unwrap_or(100).min(USAGE_DATA_MAX_PAGE_SIZE);
    let mut q = QueryBuilder::new(SELECT);
    q.push(" where started_at >= ")
        .push_bind(at(request.filter.start_ms)?)
        .push(" and started_at < ")
        .push_bind(at(request.filter.end_ms)?);
    for (column, value) in [
        ("provider_kind", request.filter.provider),
        (
            "coalesce(upstream_model_id,requested_model_id)",
            request.filter.model,
        ),
        ("provider_account_ref", request.filter.account_ref),
        ("client_api_key_ref", request.filter.client_key_ref),
    ] {
        if let Some(value) = value {
            q.push(format!(" and {column}=")).push_bind(value);
        }
    }
    if let Some(cursor) = request.cursor {
        let cursor: Cursor =
            serde_json::from_str(&cursor).map_err(|_| invalid("invalid usage cursor"))?;
        q.push(" and (started_at,id) > (")
            .push_bind(cursor.started_at)
            .push(",")
            .push_bind(cursor.id)
            .push(")");
    }
    q.push(" order by started_at,id limit ")
        .push_bind(i64::from(limit) + 1);
    let mut rows = q.build().fetch(&mut **tx);
    let mut items = Vec::new();
    let mut last = None;
    let mut bytes = 0usize;
    let mut next_cursor = None;
    while let Some(row) = rows.try_next().await.map_err(db_error)? {
        if items.len() == usize::from(limit) {
            next_cursor = last
                .map(|v| serde_json::to_string(&v).map_err(|_| invalid("encode usage cursor")))
                .transpose()?;
            break;
        }
        last = Some(Cursor {
            started_at: row.try_get("started_at").map_err(db_error)?,
            id: row.try_get("id").map_err(db_error)?,
        });
        let record = decode(row)?;
        bytes = bytes
            .checked_add(
                serde_json::to_vec(&record)
                    .map_err(|_| invalid("encode usage record"))?
                    .len(),
            )
            .ok_or_else(|| invalid("usage page byte overflow"))?;
        // 原生观测 JSON 可能较大，流式读取并限制页面，避免先物化两百条大记录
        if bytes > USAGE_DATA_MAX_REQUEST_BYTES {
            return Err(invalid("usage page exceeds 2 MiB; reduce page size"));
        }
        items.push(record);
    }
    Ok(UsageRecordPage { items, next_cursor })
}
fn decode(row: PgRow) -> StoreResult<UsageRecord> {
    let token = |name: &str| -> StoreResult<Option<UsageCount>> {
        row.try_get::<Option<i64>, _>(name)
            .map_err(db_error)?
            .map(|v| {
                u64::try_from(v)
                    .map(UsageCount)
                    .map_err(|_| invalid("negative native token count"))
            })
            .transpose()
    };
    let outcome = match row.try_get::<&str, _>("outcome").map_err(db_error)? {
        "running" => UsageOutcome::Running,
        "succeeded" => UsageOutcome::Succeeded,
        "failed" => UsageOutcome::Failed,
        "cancelled" => UsageOutcome::Cancelled,
        "incomplete" => UsageOutcome::Incomplete,
        _ => return Err(invalid("unknown native outcome")),
    };
    let source: String = row.try_get("cost_source").map_err(db_error)?;
    let cost = match (
        row.try_get::<Option<String>, _>("cost_amount")
            .map_err(db_error)?,
        row.try_get::<Option<String>, _>("cost_currency")
            .map_err(db_error)?,
    ) {
        (Some(amount), Some(currency)) => UsageCost::Known {
            source: if source == "provider_reported" {
                UsageCostSource::ProviderReported
            } else {
                UsageCostSource::Calculated
            },
            amount,
            currency,
        },
        _ => UsageCost::Unavailable,
    };
    Ok(UsageRecord {
        id: row.try_get("id").map_err(db_error)?,
        data: UsageRecordData {
            client_key_ref: row.try_get("client_api_key_ref").map_err(db_error)?,
            provider: row.try_get("provider_kind").map_err(db_error)?,
            account_ref: row.try_get("provider_account_ref").map_err(db_error)?,
            requested_model: row.try_get("requested_model_id").map_err(db_error)?,
            model: row.try_get("upstream_model_id").map_err(db_error)?,
            protocol: row.try_get("protocol").map_err(db_error)?,
            operation: row.try_get("operation").map_err(db_error)?,
            endpoint: row.try_get("endpoint").map_err(db_error)?,
            client_transport: row.try_get("client_transport").map_err(db_error)?,
            service_tier: row.try_get("service_tier").map_err(db_error)?,
            started_at_ms: row
                .try_get::<DateTime<Utc>, _>("started_at")
                .map_err(db_error)?
                .timestamp_millis(),
            completed_at_ms: row
                .try_get::<Option<DateTime<Utc>>, _>("completed_at")
                .map_err(db_error)?
                .map(|v| v.timestamp_millis()),
            outcome,
            tokens: UsageTokens {
                input_tokens: token("input_tokens")?,
                output_tokens: token("output_tokens")?,
                cached_input_tokens: token("cached_input_tokens")?,
                cache_write_input_tokens: token("cache_write_input_tokens")?,
                reasoning_output_tokens: token("reasoning_output_tokens")?,
                image_input_tokens: token("image_input_tokens")?,
                image_output_tokens: token("image_output_tokens")?,
                total_tokens: token("total_tokens")?,
            },
            cost,
        },
        billing_snapshot: row.try_get("billing_snapshot_json").map_err(db_error)?,
        provider_observation: row.try_get("provider_observation_json").map_err(db_error)?,
    })
}
fn cost_parts(cost: &UsageCost) -> (&str, Option<&str>, Option<&str>) {
    match cost {
        UsageCost::Unavailable => ("unavailable", None, None),
        UsageCost::Known {
            source,
            amount,
            currency,
        } => (
            match source {
                UsageCostSource::ProviderReported => "provider_reported",
                UsageCostSource::Calculated => "calculated",
            },
            Some(amount),
            Some(currency),
        ),
    }
}
fn token_values(t: &UsageTokens) -> StoreResult<Vec<Option<i64>>> {
    [
        t.input_tokens,
        t.output_tokens,
        t.cached_input_tokens,
        t.cache_write_input_tokens,
        t.reasoning_output_tokens,
        t.image_input_tokens,
        t.image_output_tokens,
        t.total_tokens,
    ]
    .into_iter()
    .map(|v| {
        v.map(|v| i64::try_from(v.get()).map_err(|_| invalid("native token overflow")))
            .transpose()
    })
    .collect()
}
pub(super) async fn write(
    tx: &mut Transaction<'_, Postgres>,
    change: &UsageRecordChange,
) -> StoreResult<bool> {
    // 请求执行持有自己的状态；维护终态数据不参与运行请求的结算或租约
    let running: Option<String> =
        sqlx::query_scalar("select outcome from model_requests where id=$1 for update")
            .bind(change.id())
            .fetch_optional(&mut **tx)
            .await
            .map_err(db_error)?;
    if running.as_deref() == Some("running") {
        return Err(StoreError::Conflict {
            entity: "usage record",
            id: change.id().into(),
            kind: crate::ConflictKind::InvalidTransition,
            source: None,
        });
    }
    match change {
        UsageRecordChange::Delete { id } => Ok(sqlx::query(
            "delete from model_requests where id=$1 and outcome<>'running'",
        )
        .bind(id)
        .execute(&mut **tx)
        .await
        .map_err(db_error)?
        .rows_affected()
            > 0),
        UsageRecordChange::Put { id, data } => {
            let outcome =
                serde_json::to_value(data.outcome).map_err(|_| invalid("invalid outcome"))?;
            let outcome = outcome.as_str().ok_or_else(|| invalid("invalid outcome"))?;
            let end = at(data
                .completed_at_ms
                .ok_or_else(|| invalid("completed time required"))?)?;
            let obs = serde_json::json!({"request":{"configRevision":1,"protocol":data.protocol,"endpoint":data.endpoint,"compact":false},"routing":{"scope":"all","groupRefs":[],"groupNamesSnapshot":[]}});
            let (source, amount, currency) = cost_parts(&data.cost);
            let mut q = QueryBuilder::new(
                "insert into model_requests(id,client_api_key_ref,provider_kind,provider_account_ref,requested_model_id,upstream_model_id,operation,client_transport,service_tier,started_at,recovery_lease_expires_at,completed_at,outcome,request_observation_json,client_status_code,downstream_committed_at,input_tokens,output_tokens,cached_input_tokens,cache_write_input_tokens,reasoning_output_tokens,image_input_tokens,image_output_tokens,total_tokens,cost_source,cost_amount,cost_currency) values (",
            );
            q.push_bind(id)
                .push(",")
                .push_bind(&data.client_key_ref)
                .push(",")
                .push_bind(&data.provider)
                .push(",")
                .push_bind(&data.account_ref)
                .push(",")
                .push_bind(&data.requested_model)
                .push(",")
                .push_bind(&data.model)
                .push(",")
                .push_bind(&data.operation)
                .push(",")
                .push_bind(&data.client_transport)
                .push(",")
                .push_bind(&data.service_tier)
                .push(",")
                .push_bind(at(data.started_at_ms)?)
                .push(",")
                .push_bind(end)
                .push(",")
                .push_bind(end)
                .push(",")
                .push_bind(outcome)
                .push(",")
                .push_bind(obs)
                .push(",")
                .push_bind(if data.outcome == UsageOutcome::Succeeded {
                    Some(200i32)
                } else {
                    None
                })
                .push(",")
                .push_bind(if data.outcome == UsageOutcome::Succeeded {
                    Some(end)
                } else {
                    None
                });
            for token in token_values(&data.tokens)? {
                q.push(",").push_bind(token);
            }
            q.push(",")
                .push_bind(source)
                .push(",")
                .push_bind(amount)
                .push("::numeric,")
                .push_bind(currency)
                .push(") on conflict(id) do update set ");
            for (i, column) in [
                "client_api_key_ref",
                "provider_kind",
                "provider_account_ref",
                "requested_model_id",
                "upstream_model_id",
                "operation",
                "client_transport",
                "service_tier",
                "started_at",
                "recovery_lease_expires_at",
                "completed_at",
                "outcome",
                "request_observation_json",
                "client_status_code",
                "downstream_committed_at",
                "input_tokens",
                "output_tokens",
                "cached_input_tokens",
                "cache_write_input_tokens",
                "reasoning_output_tokens",
                "image_input_tokens",
                "image_output_tokens",
                "total_tokens",
                "cost_source",
                "cost_amount",
                "cost_currency",
            ]
            .iter()
            .enumerate()
            {
                if i > 0 {
                    q.push(",");
                }
                q.push(format!("{column}=excluded.{column}"));
            }
            q.push(",billing_snapshot_json=null where model_requests.outcome<>'running'");
            let changed = q
                .build()
                .execute(&mut **tx)
                .await
                .map_err(db_error)?
                .rows_affected()
                > 0;
            if !changed {
                return Err(invalid("cannot replace running usage record"));
            }
            Ok(true)
        }
        UsageRecordChange::Update { id, tokens, cost } => {
            if running.is_none() {
                return Err(StoreError::NotFound {
                    entity: "usage record",
                    id: id.clone(),
                    source: None,
                });
            }
            let mut q = QueryBuilder::new("update model_requests set billing_snapshot_json=null");
            if let Some(tokens) = tokens {
                for (name, value) in [
                    "input_tokens",
                    "output_tokens",
                    "cached_input_tokens",
                    "cache_write_input_tokens",
                    "reasoning_output_tokens",
                    "image_input_tokens",
                    "image_output_tokens",
                    "total_tokens",
                ]
                .into_iter()
                .zip(token_values(tokens)?)
                {
                    q.push(format!(",{name}=")).push_bind(value);
                }
            }
            if let Some(cost) = cost {
                let (source, amount, currency) = cost_parts(cost);
                q.push(",cost_source=")
                    .push_bind(source)
                    .push(",cost_amount=")
                    .push_bind(amount)
                    .push("::numeric,cost_currency=")
                    .push_bind(currency);
            }
            q.push(" where id=").push_bind(id);
            q.build().execute(&mut **tx).await.map_err(db_error)?;
            Ok(true)
        }
    }
}
