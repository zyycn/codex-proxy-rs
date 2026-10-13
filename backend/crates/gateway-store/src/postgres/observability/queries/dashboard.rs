//! 时间桶补齐与部署时区的日历边界

use super::super::*;

pub(crate) fn fill_metric_gaps(
    range: ObservabilityRange,
    granularity: ObservationGranularity,
    mut points: BTreeMap<DateTime<Utc>, RequestMetricPoint>,
    timezone: gateway_core::time::DeploymentTimeZone,
) -> StoreResult<Vec<RequestMetricPoint>> {
    if granularity == ObservationGranularity::Day {
        return Ok(calendar_buckets(range, timezone)?
            .into_iter()
            .map(|bucket| {
                points.remove(&bucket).unwrap_or(RequestMetricPoint {
                    bucket_start: bucket,
                    granularity,
                    metrics: RequestMetrics::default(),
                    cost_coverage: CostCoverage::default(),
                    costs: Vec::new(),
                })
            })
            .collect());
    }
    let seconds = granularity.seconds();
    let mut bucket =
        DateTime::from_timestamp(range.start.timestamp().div_euclid(seconds) * seconds, 0)
            .ok_or_else(|| invalid("invalid metric start"))?;
    let step = TimeDelta::seconds(seconds);
    let mut result = Vec::new();
    while bucket < range.end {
        result.push(points.remove(&bucket).unwrap_or(RequestMetricPoint {
            bucket_start: bucket,
            granularity,
            metrics: RequestMetrics::default(),
            cost_coverage: CostCoverage::default(),
            costs: Vec::new(),
        }));
        bucket = bucket
            .checked_add_signed(step)
            .ok_or_else(|| invalid("invalid next metric bucket"))?;
    }
    Ok(result)
}

pub(crate) fn calendar_buckets(
    range: ObservabilityRange,
    timezone: gateway_core::time::DeploymentTimeZone,
) -> StoreResult<Vec<DateTime<Utc>>> {
    let mut day = timezone
        .day_start(range.start)
        .ok_or_else(|| invalid("invalid calendar day"))?;
    let mut result = Vec::new();
    while day < range.end {
        result.push(day);
        day = timezone
            .days_after(day, 1)
            .ok_or_else(|| invalid("invalid next calendar day"))?;
    }
    Ok(result)
}
