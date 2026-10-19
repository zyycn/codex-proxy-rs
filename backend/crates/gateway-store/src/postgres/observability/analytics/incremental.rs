//! 同事务撤销旧事实并合并新事实，构建中的统计只更新已经扫描的前缀

use super::buckets::write_bucket_groups;
use super::worker::collections;
use super::*;
use sqlx::postgres::PgRow;

type GroupKey = (DateTime<Utc>, String, Vec<String>, String);

#[derive(Default)]
struct Change {
    previous: MetricState,
    current: MetricState,
}

struct Progress {
    phase: i16,
    started_at: Option<DateTime<Utc>>,
    id: Option<String>,
    collection: Option<String>,
    dimensions: Option<Vec<String>>,
    outcome: Option<String>,
}

impl Progress {
    fn contains_fact(&self, time: DateTime<Utc>, id: &str) -> bool {
        match self.phase {
            0 => false,
            1 => self
                .started_at
                .zip(self.id.as_deref())
                .is_some_and(|cursor| (time, id) <= cursor),
            _ => true,
        }
    }

    fn contains_quarter(&self, key: &GroupKey) -> bool {
        if self.phase != 4 {
            return false;
        }
        match (
            &self.started_at,
            &self.collection,
            &self.dimensions,
            &self.outcome,
        ) {
            (Some(time), Some(collection), Some(dimensions), Some(outcome)) => {
                (&key.0, &key.1, &key.2, &key.3) <= (time, collection, dimensions, outcome)
            }
            _ => false,
        }
    }
}

pub(super) async fn update_summaries(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    previous: &[PgRow],
    current: &[PgRow],
) -> StoreResult<()> {
    let mut hours = BTreeSet::new();
    for row in previous.iter().chain(current) {
        hours.insert(bucket_for(get(row, "started_at")?, 3600)?);
    }
    if hours.is_empty() {
        return Ok(());
    }
    let progress_rows = sqlx::query("select p.* from unnest($1::timestamptz[]) h(start_at) join request_analytics_pending_buckets p on p.bucket_start >= h.start_at and p.bucket_start < h.start_at+interval '1 hour'")
        .bind(hours.into_iter().collect::<Vec<_>>()).fetch_all(&mut **tx).await
        .map_err(|error|postgres_unavailable("read incremental analytics stages",error))?;
    let mut progress = BTreeMap::new();
    for row in progress_rows {
        let phase =
            if get::<Option<i64>>(&row, "build_version")? == Some(get(&row, "change_version")?) {
                get(&row, "build_phase")?
            } else {
                0
            };
        progress.insert(
            get::<DateTime<Utc>>(&row, "bucket_start")?,
            Progress {
                phase,
                started_at: get(&row, "build_started_at")?,
                id: get(&row, "build_id")?,
                collection: get(&row, "build_collection")?,
                dimensions: get(&row, "build_dimensions")?,
                outcome: get(&row, "build_outcome")?,
            },
        );
    }
    // 两种粒度共用一次事实解码，仍分别检查构建前缀，不能把未发布季度并入小时
    let mut changes = [BTreeMap::<GroupKey, Change>::new(), BTreeMap::new()];
    let clean_hours = progress.is_empty();
    for (rows, adding) in [(previous, false), (current, true)] {
        for row in rows {
            let time = get(row, "started_at")?;
            let quarter = bucket_for(time, 900)?;
            let hour = bucket_for(time, 3600)?;
            let id: String = get(row, "id")?;
            let outcome: String = get(row, "outcome")?;
            let mut state = MetricState::default();
            state.add_row(row)?;
            let forecast = forecast::state(row)?;
            let mut hour_pending = progress.range(hour..hour + TimeDelta::hours(1)).peekable();
            let hour_ready = hour_pending.peek().is_none();
            let hour_owner =
                hour_pending.find_map(|(_, stage)| (stage.phase >= 3).then_some(stage));
            for (collection, dimensions) in collections(row)? {
                let key = (quarter, collection.to_owned(), dimensions, outcome.clone());
                let included = [
                    progress
                        .get(&quarter)
                        .is_none_or(|p| p.contains_fact(time, &id)),
                    !clean_hours
                        && (hour_ready
                            || hour_owner.is_some_and(|stage| stage.contains_quarter(&key))),
                ];
                let hourly_key = (hour, key.1.clone(), key.2.clone(), key.3.clone());
                for (index, key) in [key, hourly_key].into_iter().enumerate() {
                    if !included[index] {
                        continue;
                    }
                    let change = changes[index].entry(key).or_default();
                    let state = if collection == forecast::COLLECTION {
                        &forecast
                    } else {
                        &state
                    };
                    if adding {
                        change.current.merge(state)?;
                    } else {
                        change.previous.merge(state)?;
                    }
                }
            }
        }
    }
    if clean_hours {
        // 无重建中的时间桶时，小时贡献可直接合并季度贡献，避免逐事实再合并一遍全部指标
        let [quarters, hours] = &mut changes;
        for ((quarter, collection, dimensions, outcome), contribution) in quarters.iter() {
            let key = (
                bucket_for(*quarter, 3600)?,
                collection.clone(),
                dimensions.clone(),
                outcome.clone(),
            );
            let hour = hours.entry(key).or_default();
            hour.previous.merge(&contribution.previous)?;
            hour.current.merge(&contribution.current)?;
        }
    }
    for (index, changes) in changes.into_iter().enumerate() {
        apply_changes(tx, index == 1, &progress, changes).await?;
    }
    Ok(())
}

async fn apply_changes(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    hourly: bool,
    progress: &BTreeMap<DateTime<Utc>, Progress>,
    changes: BTreeMap<GroupKey, Change>,
) -> StoreResult<()> {
    if changes.is_empty() {
        return Ok(());
    }
    let table = if hourly {
        "request_analytics_1h"
    } else {
        "request_analytics_15m"
    };
    let keys = changes.keys().map(|(bucket,collection,dimensions,outcome)|serde_json::json!({"bucket_start":bucket,"collection":collection,"dimensions":dimensions,"outcome":outcome})).collect::<Vec<_>>();
    let statement = format!(
        "select a.* from {table} a join jsonb_to_recordset($1::jsonb) k(bucket_start timestamptz,collection text,dimensions text[],outcome text) using(bucket_start,collection,dimensions,outcome)"
    );
    let rows = sqlx::query(sqlx::AssertSqlSafe(statement))
        .bind(serde_json::Value::Array(keys))
        .fetch_all(&mut **tx)
        .await
        .map_err(|error| postgres_unavailable("read incremental analytics groups", error))?;
    let mut stored = BTreeMap::new();
    for row in rows {
        stored.insert(
            (
                get(&row, "bucket_start")?,
                get(&row, "collection")?,
                get(&row, "dimensions")?,
                get(&row, "outcome")?,
            ),
            MetricState::decode(&row)?,
        );
    }
    let mut buckets =
        BTreeMap::<DateTime<Utc>, BTreeMap<(String, Vec<String>, String), MetricState>>::new();
    let mut empty_groups = Vec::new();
    let mut rebuild = Vec::new();
    for (key, change) in changes {
        if change.previous.encode()? == change.current.encode()?
            && change.previous.last_used_at() == change.current.last_used_at()
        {
            continue;
        }
        let mut state = stored.remove(&key).unwrap_or_default();
        let needs_rebuild = subtract(&mut state, &change.previous, &change.current)?;
        state.merge(&change.current)?;
        if state.count("request_count") == 0 {
            empty_groups.push(serde_json::json!({
                "bucket_start": key.0,
                "collection": key.1,
                "dimensions": key.2,
                "outcome": key.3,
            }));
        } else {
            // 预测状态只有可加计数和美元总额，零金额无需修复币种存在性
            if needs_rebuild && key.1 != forecast::COLLECTION {
                rebuild.push(key);
                continue;
            }
            buckets
                .entry(key.0)
                .or_default()
                .insert((key.1, key.2, key.3), state);
        }
    }
    for (key, state) in rebuild_groups(tx, hourly, progress, rebuild).await? {
        buckets
            .entry(key.0)
            .or_default()
            .insert((key.1, key.2, key.3), state);
    }
    // 保留期缩短会同时清空大量分组，集合删除避免 SQL 往返随分组数放大
    for groups in empty_groups.chunks(1_000) {
        let statement = format!(
            "delete from {table} a using jsonb_to_recordset($1::jsonb) k(bucket_start timestamptz,collection text,dimensions text[],outcome text)
             where a.bucket_start=k.bucket_start and a.collection=k.collection and a.dimensions=k.dimensions and a.outcome=k.outcome"
        );
        sqlx::query(sqlx::AssertSqlSafe(statement))
            .bind(sqlx::types::Json(groups))
            .execute(&mut **tx)
            .await
            .map_err(|error| postgres_unavailable("remove empty analytics groups", error))?;
    }
    write_bucket_groups(tx, table, buckets).await
}

/// 返回是否撤销了不能从可加状态推导的极值或币种存在性
pub(super) fn subtract(
    state: &mut MetricState,
    previous: &MetricState,
    current: &MetricState,
) -> StoreResult<bool> {
    if previous.count("request_count") == 0 {
        return Ok(false);
    }
    // 新贡献仍持有旧极值时无需重读事实，例如只补齐 Token、费用或重放同一请求
    let mut rebuild = previous.last_used_at.is_some()
        && previous.last_used_at == state.last_used_at
        && current.last_used_at < state.last_used_at;
    for (name, value) in &previous.counts {
        let count = state.counts.entry(name.clone()).or_default();
        *count = count
            .checked_sub(*value)
            .ok_or_else(|| invalid("analytics counter underflow"))?;
    }
    for (name, removed) in &previous.distributions {
        let distribution = state
            .distributions
            .get_mut(name)
            .ok_or_else(|| invalid("missing analytics distribution"))?;
        distribution.count = distribution
            .count
            .checked_sub(removed.count)
            .ok_or_else(|| invalid("analytics distribution underflow"))?;
        distribution.zero = distribution
            .zero
            .checked_sub(removed.zero)
            .ok_or_else(|| invalid("analytics zero count underflow"))?;
        distribution.sum -= removed.sum;
        for (bin, value) in &removed.bins {
            let count = distribution
                .bins
                .get_mut(bin)
                .ok_or_else(|| invalid("missing analytics bin"))?;
            *count = count
                .checked_sub(*value)
                .ok_or_else(|| invalid("analytics bin underflow"))?;
        }
        distribution.bins.retain(|_, count| *count > 0);
        if distribution.count == 0 {
            *distribution = Default::default();
        } else {
            let replacement = current.distributions.get(name);
            rebuild |= distribution.minimum == removed.minimum
                && replacement
                    .and_then(|value| value.minimum)
                    .is_none_or(|value| Some(value) > distribution.minimum);
            rebuild |= distribution.maximum == removed.maximum
                && replacement
                    .and_then(|value| value.maximum)
                    .is_none_or(|value| Some(value) < distribution.maximum);
            rebuild |= !distribution.sum.is_finite() || distribution.sum < 0.0;
        }
    }
    for (currency, amount) in &previous.costs {
        let stored_amount = state
            .costs
            .get(currency)
            .ok_or_else(|| invalid("missing analytics currency"))?;
        let stored_amount: gateway_core::metering::Decimal = stored_amount
            .parse()
            .map_err(|_| invalid("invalid analytics amount"))?;
        let removed: gateway_core::metering::Decimal = amount
            .parse()
            .map_err(|_| invalid("invalid analytics amount"))?;
        let value = stored_amount
            .scaled()
            .checked_sub(removed.scaled())
            .ok_or_else(|| invalid("analytics amount underflow"))?;
        rebuild |= value == 0 && !current.costs.contains_key(currency);
        state.costs.insert(
            currency.clone(),
            gateway_core::metering::Decimal::from_scaled(value)
                .map_err(|_| invalid("analytics amount overflow"))?
                .canonical(),
        );
    }
    Ok(rebuild)
}

async fn rebuild_groups(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    hourly: bool,
    progress: &BTreeMap<DateTime<Utc>, Progress>,
    keys: Vec<GroupKey>,
) -> StoreResult<BTreeMap<GroupKey, MetricState>> {
    let mut by_collection = BTreeMap::<&str, Vec<&GroupKey>>::new();
    for key in &keys {
        by_collection.entry(&key.1).or_default().push(key);
    }
    let mut rebuilt = BTreeMap::new();
    for (collection, keys) in by_collection {
        let columns = worker::COLLECTION_COLUMNS
            .iter()
            .find_map(|(name, columns)| (*name == collection).then_some(*columns))
            .ok_or_else(|| invalid("unknown incremental analytics collection"))?;
        // 同类分组共用维度匹配和聚合 SQL，各组仍独立遵守已发布的构建前缀
        for keys in keys.chunks(1_000) {
            let mut input = Vec::with_capacity(keys.len());
            for (index, key) in keys.iter().enumerate() {
                let mut end = key.0 + TimeDelta::seconds(if hourly { 3600 } else { 900 });
                let mut cursor = None;
                if hourly {
                    if let Some((_, owner)) = progress.range(key.0..end).find(|(_, p)| p.phase == 4)
                        && let Some(time) = owner.started_at
                    {
                        let candidate = (time, key.1.clone(), key.2.clone(), key.3.clone());
                        end = if owner.contains_quarter(&candidate) {
                            time + TimeDelta::minutes(15)
                        } else {
                            time
                        };
                    }
                } else if let Some(stage) = progress.get(&key.0)
                    && stage.phase == 1
                {
                    cursor = stage.started_at.zip(stage.id.as_deref());
                }
                input.push(serde_json::json!({
                    "bucket_start": key.0,
                    "end_at": end,
                    "cursor_time": cursor.map(|(time, _)| time),
                    "cursor_id": cursor.map(|(_, id)| id),
                    "dimensions": key.2,
                    "outcome": key.3,
                    "name": index.to_string(),
                }));
                rebuilt.insert((*key).clone(), MetricState::default());
            }
            // 等值桶键避免范围连接反复比较其他时间桶，范围和游标仍限定已发布前缀
            let mut query = QueryBuilder::<Postgres>::new(
                "with matching as materialized (select mr.*,k.bucket_start as output_bucket,k.name,coalesce((",
            );
            query.push(completed_usage_fact_predicate("mr"))
                .push("),false) as is_completed_usage from jsonb_to_recordset(")
                .push_bind(serde_json::Value::Array(input))
                .push("::jsonb) k(bucket_start timestamptz,end_at timestamptz,cursor_time timestamptz,cursor_id text,dimensions text[],outcome text,name text)
                    join request_analytics_facts mr on date_bin(make_interval(secs => ")
                .push_bind(if hourly { 3600_f64 } else { 900_f64 })
                .push("),mr.started_at,timestamptz '2000-01-01 UTC')=k.bucket_start
                    and mr.started_at >= k.bucket_start and mr.started_at < k.end_at
                    and mr.recovered_at is null and mr.outcome=k.outcome
                    and (k.cursor_time is null or (mr.started_at,mr.id collate \"C\") <= (k.cursor_time,k.cursor_id collate \"C\"))");
            for (index, column) in columns.iter().enumerate() {
                query.push(format!(
                    " and coalesce(mr.{column}::text,'')=k.dimensions[{}]",
                    index + 1
                ));
            }
            query.push(")");
            aggregate::push_fact_aggregation(&mut query, false, true);
            let rows = query.build().fetch_all(&mut **tx).await.map_err(|error| {
                postgres_unavailable("rebuild changed analytics extrema", error)
            })?;
            for row in rows {
                let name: String = get(&row, "name")?;
                let index = name
                    .parse::<usize>()
                    .map_err(|_| invalid("invalid analytics rebuild group"))?;
                let key = keys
                    .get(index)
                    .ok_or_else(|| invalid("unknown analytics rebuild group"))?;
                rebuilt.insert((*key).clone(), MetricState::decode(&row)?);
            }
        }
    }
    Ok(rebuilt)
}
