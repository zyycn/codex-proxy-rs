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
        "request_analytics_hourly"
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
            let statement = format!(
                "delete from {table} where bucket_start=$1 and collection=$2 and dimensions=$3 and outcome=$4"
            );
            sqlx::query(sqlx::AssertSqlSafe(statement))
                .bind(key.0)
                .bind(&key.1)
                .bind(&key.2)
                .bind(&key.3)
                .execute(&mut **tx)
                .await
                .map_err(|error| postgres_unavailable("remove empty analytics group", error))?;
        } else {
            // 预测状态只有可加计数和美元总额，零金额无需修复币种存在性
            if needs_rebuild && key.1 != forecast::COLLECTION {
                state = rebuild_group(tx, hourly, progress, &key).await?;
            }
            buckets
                .entry(key.0)
                .or_default()
                .insert((key.1, key.2, key.3), state);
        }
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

async fn rebuild_group(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    hourly: bool,
    progress: &BTreeMap<DateTime<Utc>, Progress>,
    key: &GroupKey,
) -> StoreResult<MetricState> {
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
    let mut query = QueryBuilder::<Postgres>::new("with matching as materialized (select mr.*,");
    query.push_bind(key.0).push("::timestamptz as output_bucket,''::text as name,coalesce((")
        .push(completed_usage_fact_predicate("mr")).push("),false) as is_completed_usage from request_analytics_facts mr where mr.started_at >= ")
        .push_bind(key.0).push(" and mr.started_at < ").push_bind(end)
        .push(" and mr.recovered_at is null and mr.outcome=").push_bind(&key.3);
    if let Some((time, id)) = cursor {
        query
            .push(" and (mr.started_at,mr.id collate \"C\") <= (")
            .push_bind(time)
            .push(",")
            .push_bind(id)
            .push(")");
    }
    let columns = worker::COLLECTION_COLUMNS
        .iter()
        .find_map(|(collection, columns)| (*collection == key.1).then_some(*columns))
        .ok_or_else(|| invalid("unknown incremental analytics collection"))?;
    for (column, value) in columns.iter().zip(&key.2) {
        query
            .push(format!(" and coalesce(mr.{column}::text,'')="))
            .push_bind(value);
    }
    query.push(")");
    aggregate::push_fact_aggregation(&mut query, false, true);
    let row = query
        .build()
        .fetch_optional(&mut **tx)
        .await
        .map_err(|error| postgres_unavailable("rebuild changed analytics extrema", error))?;
    row.as_ref()
        .map(MetricState::decode)
        .transpose()
        .map(Option::unwrap_or_default)
}
