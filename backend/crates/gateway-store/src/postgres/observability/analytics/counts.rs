//! 用完整刻钟计数和同快照源差量定位精确页码，避免每页扫描整个保留期

use super::query::{collection_for, push_collection_filter, push_summary_buckets};
use super::*;

enum ReadVersion {
    Published,
    Before,
    Current,
}

impl AnalyticsSnapshot<'_> {
    pub(crate) async fn completed_request_counts(
        &mut self,
        range: ObservabilityRange,
        filter: &UsageRecordFilter,
    ) -> StoreResult<Option<BTreeMap<DateTime<Utc>, u64>>> {
        let Some(collection) = collection_for(filter, None) else {
            return Ok(None);
        };
        if !self.projection_covers(range) {
            return Ok(None);
        }
        let mut counts = BTreeMap::new();
        let (_, buckets) = self.summary_buckets(range, false).await?;
        if !buckets.is_empty() {
            let mut query = QueryBuilder::<Postgres>::new(
                "select bucket_start,coalesce(sum((counts->>'completed_request_count')::bigint),0)::bigint as request_count from request_analytics_15m a where collection=",
            );
            query
                .push_bind(collection)
                .push(" and schema_version=")
                .push_bind(SCHEMA_VERSION)
                .push(" and histogram_version=")
                .push_bind(HISTOGRAM_VERSION);
            push_summary_buckets(&mut query, &buckets, 900);
            push_collection_filter(&mut query, collection, filter);
            query.push(" group by bucket_start");
            for row in query
                .build()
                .fetch_all(&mut *self.tx)
                .await
                .map_err(|error| postgres_unavailable("read usage page counts", error))?
            {
                counts.insert(
                    get(&row, "bucket_start")?,
                    to_u64(get(&row, "request_count")?)?,
                );
            }
        }
        for (start, end) in self.uncovered_spans(range).await? {
            self.merge_completed_counts(
                &mut counts,
                ObservabilityRange { start, end },
                filter,
                ReadVersion::Published,
            )
            .await?;
        }
        if self.has_pending_requests().await? {
            // 先撤销已投影版本，再加入源表当前版本；移动、删除和未确认的重放均只计算一次
            self.merge_completed_counts(&mut counts, range, filter, ReadVersion::Before)
                .await?;
            self.merge_completed_counts(&mut counts, range, filter, ReadVersion::Current)
                .await?;
        }
        Ok(Some(counts))
    }

    async fn merge_completed_counts(
        &mut self,
        counts: &mut BTreeMap<DateTime<Utc>, u64>,
        range: ObservabilityRange,
        filter: &UsageRecordFilter,
        version: ReadVersion,
    ) -> StoreResult<()> {
        let table = if matches!(version, ReadVersion::Current) {
            "model_request_observations"
        } else {
            "request_analytics_facts"
        };
        let mut query = QueryBuilder::<Postgres>::new(format!(
            "select date_bin(interval '15 minutes',mr.started_at,timestamptz '2000-01-01 UTC') as bucket_start,count(*)::bigint as request_count from {table} mr"
        ));
        if !matches!(version, ReadVersion::Published) {
            query.push(" join request_analytics_pending_requests p on p.request_id=mr.id");
        }
        query
            .push(" where mr.started_at >= ")
            .push_bind(range.start)
            .push(" and mr.started_at < ")
            .push_bind(range.end);
        push_completed_usage_fact_filter(&mut query, "mr");
        push_usage_filter(&mut query, filter, "mr");
        query.push(" group by bucket_start");
        let rows = query
            .build()
            .fetch_all(&mut *self.tx)
            .await
            .map_err(|error| postgres_unavailable("correct usage page counts", error))?;
        for row in rows {
            let value = to_u64(get(&row, "request_count")?)?;
            let count = counts.entry(get(&row, "bucket_start")?).or_default();
            *count = if matches!(version, ReadVersion::Before) {
                count.checked_sub(value)
            } else {
                count.checked_add(value)
            }
            .ok_or_else(|| invalid("usage page count is inconsistent"))?;
        }
        Ok(())
    }
}
