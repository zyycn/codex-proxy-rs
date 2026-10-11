//! 按持久主键游标清理过期统计版本，保留重建期间的源请求队列

use super::*;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", content = "key", rename_all = "snake_case")]
enum ResetCursor {
    Fact {
        id: String,
    },
    Rollup {
        bucket_start: DateTime<Utc>,
        collection: String,
        dimensions: Vec<String>,
        outcome: String,
    },
    Bucket {
        bucket_start: DateTime<Utc>,
    },
}

pub(super) async fn reset_obsolete_batch(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    batch_size: u32,
) -> StoreResult<bool> {
    let state = sqlx::query("select schema_version,histogram_version,reset_phase,reset_cursor from request_analytics_build_state where singleton")
        .fetch_one(&mut **tx).await.map_err(|error|postgres_unavailable("read analytics build version",error))?;
    let mut phase: i16 = get(&state, "reset_phase")?;
    if phase == 0 {
        if get::<i32>(&state, "schema_version")? == SCHEMA_VERSION
            && get::<i32>(&state, "histogram_version")? == HISTOGRAM_VERSION
        {
            return Ok(false);
        }
        phase = 1;
        sqlx::query("update request_analytics_build_state set reset_phase=1,reset_cursor=null,backfill_complete=false,coverage_start=null,coverage_end=null,generation=generation+1,updated_at=clock_timestamp() where singleton")
            .execute(&mut **tx).await.map_err(|error|postgres_unavailable("begin obsolete analytics reset",error))?;
    }
    let (table, keys, kind) = match phase {
        1 => ("request_analytics_facts", "id", "fact"),
        2 => (
            "request_analytics_15m",
            "bucket_start,collection,dimensions,outcome",
            "rollup",
        ),
        3 => (
            "request_analytics_hourly",
            "bucket_start,collection,dimensions,outcome",
            "rollup",
        ),
        4 => (
            "request_analytics_pending_buckets",
            "bucket_start",
            "bucket",
        ),
        _ => return Err(invalid("invalid analytics reset phase")),
    };
    let mut query = QueryBuilder::<Postgres>::new(format!(
        "with batch as materialized (select ctid,{keys} from {table}"
    ));
    let cursor = get::<Option<sqlx::types::Json<ResetCursor>>>(&state, "reset_cursor")?;
    // LIMIT 只限制删除数量，主键下界才避免后续批次反复扫描已经清空的堆页
    match cursor.map(|cursor| cursor.0) {
        None => {}
        Some(ResetCursor::Fact { id }) if phase == 1 => {
            query.push(" where id > ").push_bind(id);
        }
        Some(ResetCursor::Rollup {
            bucket_start,
            collection,
            dimensions,
            outcome,
        }) if phase == 2 || phase == 3 => {
            query
                .push(" where (bucket_start,collection,dimensions,outcome) > (")
                .push_bind(bucket_start)
                .push(",")
                .push_bind(collection)
                .push(",")
                .push_bind(dimensions)
                .push(",")
                .push_bind(outcome)
                .push(")");
        }
        Some(ResetCursor::Bucket { bucket_start }) if phase == 4 => {
            query.push(" where bucket_start > ").push_bind(bucket_start);
        }
        Some(_) => return Err(invalid("analytics reset cursor does not match its phase")),
    }
    query
        .push(format!(" order by {keys} limit "))
        .push_bind(i64::from(batch_size));
    let descending = keys
        .split(',')
        .map(|key| format!("{key} desc"))
        .collect::<Vec<_>>()
        .join(",");
    // CTID 只在本次事务内定位已选行，跨事务进度始终使用不会被表重写改变的主键
    query.push(format!(") , removed as (delete from {table} where ctid=any(array(select ctid from batch)) returning 1) select (select count(*) from removed) as removed,(select jsonb_build_object('kind','{kind}','key',to_jsonb(batch)-'ctid') from batch order by {descending} limit 1) as cursor"));
    let result = query
        .build()
        .fetch_one(&mut **tx)
        .await
        .map_err(|error| postgres_unavailable("reset obsolete analytics batch", error))?;
    let removed: i64 = get(&result, "removed")?;
    if removed < i64::from(batch_size) {
        if phase == 4 {
            // 所有旧派生表清空后才发布新版本，源请求队列始终保留
            sqlx::query("update request_analytics_build_state set schema_version=$1,histogram_version=$2,reset_phase=0,reset_cursor=null,backfill_cursor_started_at=null,backfill_cursor_id=null,backfill_complete=false,generation=generation+1,updated_at=clock_timestamp() where singleton")
                .bind(SCHEMA_VERSION).bind(HISTOGRAM_VERSION).execute(&mut **tx).await.map_err(|error|postgres_unavailable("restart analytics backfill",error))?;
        } else {
            sqlx::query("update request_analytics_build_state set reset_phase=$1,reset_cursor=null,updated_at=clock_timestamp() where singleton")
                .bind(phase+1).execute(&mut **tx).await.map_err(|error|postgres_unavailable("advance analytics reset phase",error))?;
        }
    } else {
        let cursor = get::<Option<sqlx::types::Json<ResetCursor>>>(&result, "cursor")?
            .ok_or_else(|| invalid("missing analytics reset cursor"))?;
        sqlx::query("update request_analytics_build_state set reset_cursor=$1,updated_at=clock_timestamp() where singleton")
            .bind(cursor).execute(&mut **tx).await.map_err(|error|postgres_unavailable("save analytics reset cursor",error))?;
    }
    Ok(true)
}
