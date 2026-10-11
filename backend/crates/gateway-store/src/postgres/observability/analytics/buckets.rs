//! 同小时的十五分钟桶就绪后共享一次可恢复的小时构建

use super::worker::collections;
use super::*;

type GroupStates = BTreeMap<(String, Vec<String>, String), MetricState>;

pub(super) enum BucketProgress {
    Waiting,
    Advanced,
    Completed(u64),
}

const PEER_NOT_READY: &str =
    "(peer.build_phase <> 2 or peer.build_version is distinct from peer.change_version)";

pub(super) async fn pending_batch(
    tx: &mut sqlx::Transaction<'_, Postgres>,
) -> StoreResult<Vec<DateTime<Utc>>> {
    // 等待其他 quarter 的条目不占用领取槽位，避免八个旧等待项挡住其依赖
    let statement = format!(
        "select pending.bucket_start from request_analytics_pending_buckets pending where pending.build_phase <> 2 or pending.build_version is distinct from pending.change_version or not exists(select 1 from request_analytics_pending_buckets peer where peer.bucket_start >= date_bin(interval '1 hour',pending.bucket_start,timestamptz '2000-01-01 UTC') and peer.bucket_start < date_bin(interval '1 hour',pending.bucket_start,timestamptz '2000-01-01 UTC')+interval '1 hour' and peer.bucket_start <> pending.bucket_start and {PEER_NOT_READY}) order by (pending.build_phase >= 3) desc,pending.oldest_pending_at,pending.bucket_start limit 8"
    );
    sqlx::query_scalar(sqlx::AssertSqlSafe(statement))
        .fetch_all(&mut **tx)
        .await
        .map_err(|error| postgres_unavailable("load analytics bucket queue", error))
}

pub(super) async fn rebuild_bucket_batch(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    bucket: DateTime<Utc>,
    batch_size: u32,
) -> StoreResult<BucketProgress> {
    let row = sqlx::query("select * from request_analytics_pending_buckets where bucket_start=$1")
        .bind(bucket)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|error| postgres_unavailable("read analytics bucket progress", error))?;
    // 同一领取批次中更早的小时发布可能已经确认了这个 quarter
    let Some(row) = row else {
        return Ok(BucketProgress::Waiting);
    };
    let version: i64 = get(&row, "change_version")?;
    let same_version = get::<Option<i64>>(&row, "build_version")? == Some(version);
    let mut phase = if same_version {
        get::<i16>(&row, "build_phase")?
    } else {
        0
    };
    if !same_version {
        // 处理中事实发生变化时清掉未发布的部分状态，不能把新旧快照增量相加
        sqlx::query("update request_analytics_pending_buckets set build_version=change_version,build_phase=0,build_started_at=null,build_id=null,build_collection=null,build_dimensions=null,build_outcome=null where bucket_start=$1")
            .bind(bucket).execute(&mut **tx).await.map_err(|error|postgres_unavailable("restart changed analytics bucket",error))?;
    }
    let hour = bucket_for(bucket, 3600)?;
    if phase < 2 {
        // 小时分段期间不改写其输入；源事实仍可投影，版本变化使小时任务先退回等待
        let owned:bool=sqlx::query_scalar("select exists(select 1 from request_analytics_pending_buckets where bucket_start >= $1 and bucket_start < $1+interval '1 hour' and bucket_start <> $2 and build_phase >= 3)")
            .bind(hour).bind(bucket).fetch_one(&mut **tx).await.map_err(|error|postgres_unavailable("claim analytics hour stage",error))?;
        if owned {
            return Ok(BucketProgress::Waiting);
        }
    } else {
        let statement = format!(
            "select not exists(select 1 from request_analytics_pending_buckets peer where peer.bucket_start >= $1 and peer.bucket_start < $1+interval '1 hour' and peer.bucket_start <> $2 and {PEER_NOT_READY})"
        );
        let ready: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(statement))
            .bind(hour)
            .bind(bucket)
            .fetch_one(&mut **tx)
            .await
            .map_err(|error| postgres_unavailable("check analytics hour inputs", error))?;
        if !ready {
            if phase >= 3 {
                sqlx::query("update request_analytics_pending_buckets set build_phase=2,build_started_at=null,build_id=null,build_collection=null,build_dimensions=null,build_outcome=null where bucket_start=$1")
                    .bind(bucket).execute(&mut **tx).await.map_err(|error|postgres_unavailable("invalidate analytics hour inputs",error))?;
                return Ok(BucketProgress::Advanced);
            }
            return Ok(BucketProgress::Waiting);
        }
    }
    if phase == 2 {
        phase = 3;
        sqlx::query(
            "update request_analytics_pending_buckets set build_phase=3 where bucket_start=$1",
        )
        .bind(bucket)
        .execute(&mut **tx)
        .await
        .map_err(|error| postgres_unavailable("start analytics hour stage", error))?;
    }
    if phase == 0 || phase == 3 {
        let table = if phase == 0 {
            "request_analytics_15m"
        } else {
            "request_analytics_hourly"
        };
        let start = if phase == 0 { bucket } else { hour };
        let statement = format!(
            "delete from {table} where ctid in (select ctid from {table} where bucket_start=$1 limit $2)"
        );
        let removed = sqlx::query(sqlx::AssertSqlSafe(statement))
            .bind(start)
            .bind(i64::from(batch_size))
            .execute(&mut **tx)
            .await
            .map_err(|error| postgres_unavailable("clear staged analytics rows", error))?
            .rows_affected();
        if removed == u64::from(batch_size) {
            return Ok(BucketProgress::Advanced);
        }
        phase += 1;
        sqlx::query("update request_analytics_pending_buckets set build_phase=$2,build_started_at=null,build_id=null,build_collection=null,build_dimensions=null,build_outcome=null where bucket_start=$1")
            .bind(bucket).bind(phase).execute(&mut **tx).await.map_err(|error|postgres_unavailable("advance analytics bucket stage",error))?;
        // 清理阶段只负责清理，下一事务从持久游标开始读取，避免复用上一阶段游标
        return Ok(BucketProgress::Advanced);
    }
    let limit = i64::from(batch_size) + 1;
    let mut groups = GroupStates::new();
    if phase == 1 {
        let statement = format!(
            "{} where mr.started_at >= $1 and mr.started_at < $1+interval '15 minutes' and mr.recovered_at is null and (mr.started_at,mr.id)>($2,$3) order by mr.started_at,mr.id limit $4",
            fact_select()
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(statement))
            .bind(bucket)
            .bind(
                get::<Option<DateTime<Utc>>>(&row, "build_started_at")?
                    .unwrap_or(bucket - TimeDelta::microseconds(1)),
            )
            .bind(get::<Option<String>>(&row, "build_id")?.unwrap_or_default())
            .bind(limit)
            .fetch_all(&mut **tx)
            .await
            .map_err(|error| postgres_unavailable("read analytics fact segment", error))?;
        let selected = &rows[..rows.len().min(batch_size as usize)];
        for fact in selected {
            let outcome: String = get(fact, "outcome")?;
            let mut state = MetricState::default();
            state.add_row(fact)?;
            for (collection, dimensions) in collections(fact)? {
                groups
                    .entry((collection.to_owned(), dimensions, outcome.clone()))
                    .or_default()
                    .merge(&state)?;
            }
        }
        merge_staged_groups(tx, "request_analytics_15m", bucket, groups).await?;
        if rows.len() <= batch_size as usize {
            sqlx::query("update request_analytics_pending_buckets set build_phase=2,build_started_at=null,build_id=null where bucket_start=$1")
                .bind(bucket).execute(&mut **tx).await.map_err(|error|postgres_unavailable("complete analytics quarter stage",error))?;
        } else if let Some(last) = selected.last() {
            sqlx::query("update request_analytics_pending_buckets set build_started_at=$2,build_id=$3 where bucket_start=$1")
                .bind(bucket).bind(get::<DateTime<Utc>>(last,"started_at")?).bind(get::<String>(last,"id")?).execute(&mut **tx).await.map_err(|error|postgres_unavailable("save analytics fact cursor",error))?;
        }
    } else if phase == 4 {
        let rows=sqlx::query("select bucket_start,collection,dimensions,outcome,counts,costs,distributions from request_analytics_15m where bucket_start >= $1 and bucket_start < $1+interval '1 hour' and schema_version=$2 and histogram_version=$3 and (bucket_start,collection,dimensions,outcome)>($4,$5,$6,$7) order by bucket_start,collection,dimensions,outcome limit $8")
            .bind(hour).bind(SCHEMA_VERSION).bind(HISTOGRAM_VERSION).bind(get::<Option<DateTime<Utc>>>(&row,"build_started_at")?.unwrap_or(hour-TimeDelta::microseconds(1)))
            .bind(get::<Option<String>>(&row,"build_collection")?.unwrap_or_default()).bind(get::<Option<Vec<String>>>(&row,"build_dimensions")?.unwrap_or_default()).bind(get::<Option<String>>(&row,"build_outcome")?.unwrap_or_default()).bind(limit)
            .fetch_all(&mut **tx).await.map_err(|error|postgres_unavailable("read analytics hour segment",error))?;
        let selected = &rows[..rows.len().min(batch_size as usize)];
        for quarter in selected {
            groups
                .entry((
                    get(quarter, "collection")?,
                    get(quarter, "dimensions")?,
                    get(quarter, "outcome")?,
                ))
                .or_default()
                .merge(&MetricState::decode(quarter)?)?;
        }
        merge_staged_groups(tx, "request_analytics_hourly", hour, groups).await?;
        if rows.len() <= batch_size as usize {
            // 一次小时发布确认全部就绪 quarter，避免每个条目再次清空并重写同一小时
            let acknowledged = sqlx::query("delete from request_analytics_pending_buckets where bucket_start >= $1 and bucket_start < $1+interval '1 hour' and build_version=change_version and (build_phase=2 or (bucket_start=$2 and build_phase=4 and change_version=$3))")
                .bind(hour).bind(bucket).bind(version).execute(&mut **tx).await.map_err(|error|postgres_unavailable("publish complete analytics hour",error))?.rows_affected();
            return Ok(BucketProgress::Completed(acknowledged));
        } else if let Some(last) = selected.last() {
            sqlx::query("update request_analytics_pending_buckets set build_started_at=$2,build_collection=$3,build_dimensions=$4,build_outcome=$5 where bucket_start=$1")
                .bind(bucket).bind(get::<DateTime<Utc>>(last,"bucket_start")?).bind(get::<String>(last,"collection")?).bind(get::<Vec<String>>(last,"dimensions")?).bind(get::<String>(last,"outcome")?)
                .execute(&mut **tx).await.map_err(|error|postgres_unavailable("save analytics hour cursor",error))?;
        }
    } else {
        return Err(invalid("invalid analytics bucket phase"));
    }
    Ok(BucketProgress::Advanced)
}

async fn merge_staged_groups(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    table: &str,
    bucket: DateTime<Utc>,
    mut groups: GroupStates,
) -> StoreResult<()> {
    if groups.is_empty() {
        return Ok(());
    }
    let keys=groups.keys().map(|(collection,dimensions,outcome)|serde_json::json!({"collection":collection,"dimensions":dimensions,"outcome":outcome})).collect::<Vec<_>>();
    let statement = format!(
        "select stored.collection,stored.dimensions,stored.outcome,stored.counts,stored.costs,stored.distributions from {table} stored join jsonb_to_recordset($2::jsonb) requested(collection text,dimensions text[],outcome text) using(collection,dimensions,outcome) where bucket_start=$1"
    );
    let rows = sqlx::query(sqlx::AssertSqlSafe(statement))
        .bind(bucket)
        .bind(serde_json::Value::Array(keys))
        .fetch_all(&mut **tx)
        .await
        .map_err(|error| postgres_unavailable("read staged analytics groups", error))?;
    for row in rows {
        let key = (
            get(&row, "collection")?,
            get(&row, "dimensions")?,
            get(&row, "outcome")?,
        );
        groups
            .get_mut(&key)
            .ok_or_else(|| invalid("missing staged analytics group"))?
            .merge(&MetricState::decode(&row)?)?;
    }
    write_groups(tx, table, bucket, groups).await
}

async fn write_groups(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    table: &str,
    bucket: DateTime<Utc>,
    groups: GroupStates,
) -> StoreResult<()> {
    if groups.is_empty() {
        return Ok(());
    }
    let mut statement = QueryBuilder::<Postgres>::new(format!(
        "insert into {table}(bucket_start,collection,dimensions,outcome,schema_version,histogram_version,counts,costs,distributions) "
    ));
    let rows = groups
        .into_iter()
        .map(|((collection, dimensions, outcome), state)| {
            let (counts, costs, distributions) = state.encode()?;
            Ok((
                collection,
                dimensions,
                outcome,
                counts,
                costs,
                distributions,
            ))
        })
        .collect::<StoreResult<Vec<_>>>()?;
    // 九个绑定参数限制单语句批量，避免高基数时间桶越过 PostgreSQL 参数上限
    for chunk in rows.chunks(1000) {
        statement.push_values(
            chunk,
            |mut row, (collection, dimensions, outcome, counts, costs, distributions)| {
                row.push_bind(bucket)
                    .push_bind(collection)
                    .push_bind(dimensions)
                    .push_bind(outcome)
                    .push_bind(SCHEMA_VERSION)
                    .push_bind(HISTOGRAM_VERSION)
                    .push_bind(counts)
                    .push_bind(costs)
                    .push_bind(distributions);
            },
        );
        statement.push(" on conflict(bucket_start,collection,dimensions,outcome) do update set counts=excluded.counts,costs=excluded.costs,distributions=excluded.distributions,schema_version=excluded.schema_version,histogram_version=excluded.histogram_version");
        statement
            .build()
            .execute(&mut **tx)
            .await
            .map_err(|error| postgres_unavailable("write analytics bucket", error))?;
        statement.reset();
    }
    Ok(())
}
