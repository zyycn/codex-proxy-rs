use super::*;

pub(super) async fn count_usage_records(
    pool: &PgPool,
    filter: &UsageRecordFilter,
) -> PgUsageRecordStoreResult<u64> {
    let mut builder = QueryBuilder::<Postgres>::new("select count(*) from usage_records");
    push_filter(&mut builder, filter, None)?;
    let total: i64 = builder.build_query_scalar().fetch_one(pool).await?;
    Ok(total.max(0) as u64)
}

pub(super) async fn usage_summary(
    pool: &PgPool,
    filter: &UsageRecordFilter,
) -> PgUsageRecordStoreResult<UsageRecordSummary> {
    let mut builder = QueryBuilder::<Postgres>::new(
        r"
select
  count(*) as total_requests,
  coalesce(sum(input_tokens), 0)::bigint as input_tokens,
  coalesce(sum(output_tokens), 0)::bigint as output_tokens,
  coalesce(sum(cached_tokens), 0)::bigint as cached_tokens,
  avg(latency_ms::double precision) as average_latency_ms
from usage_records",
    );
    push_filter(&mut builder, filter, None)?;
    let row = builder.build().fetch_one(pool).await?;
    let input_tokens = nonnegative(row.get("input_tokens"));
    let output_tokens = nonnegative(row.get("output_tokens"));
    let cached_tokens = nonnegative(row.get("cached_tokens"));
    Ok(UsageRecordSummary {
        total_requests: nonnegative(row.get("total_requests")),
        input_tokens,
        output_tokens,
        cached_tokens,
        total_tokens: input_tokens + output_tokens,
        average_latency_ms: row.get("average_latency_ms"),
    })
}

pub(super) async fn usage_account_usage(
    pool: &PgPool,
    filter: &UsageRecordFilter,
    limit: u32,
) -> PgUsageRecordStoreResult<Vec<UsageRecordAccountUsage>> {
    let mut builder = QueryBuilder::<Postgres>::new(
        r"
select
  account_id,
  coalesce(sum(input_tokens), 0)::bigint as input_tokens,
  coalesce(sum(output_tokens), 0)::bigint as output_tokens,
  max(created_at) as last_used_at
from usage_records",
    );
    push_filter(&mut builder, filter, None)?;
    builder.push(" group by account_id order by last_used_at desc, account_id limit ");
    builder.push_bind(i64::from(limit.clamp(1, 50)));
    Ok(builder
        .build()
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|row| {
            let input_tokens = nonnegative(row.get("input_tokens"));
            let output_tokens = nonnegative(row.get("output_tokens"));
            UsageRecordAccountUsage {
                account_id: row.get("account_id"),
                input_tokens,
                output_tokens,
                total_tokens: input_tokens + output_tokens,
                last_used_at: row.get("last_used_at"),
            }
        })
        .collect())
}

pub(super) async fn usage_record_account_email_map(
    pool: &PgPool,
    items: &[UsageRecord],
) -> PgUsageRecordStoreResult<HashMap<String, String>> {
    let mut account_ids = items
        .iter()
        .map(|item| item.account_id.clone())
        .collect::<Vec<_>>();
    account_ids.sort_unstable();
    account_ids.dedup();
    if account_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let mut builder = QueryBuilder::<Postgres>::new("select id, email from accounts where id in (");
    let mut values = builder.separated(", ");
    for account_id in account_ids {
        values.push_bind(account_id);
    }
    values.push_unseparated(")");
    Ok(builder
        .build()
        .fetch_all(pool)
        .await?
        .into_iter()
        .filter_map(|row| {
            row.get::<Option<String>, _>("email")
                .filter(|email| !email.trim().is_empty())
                .map(|email| (row.get("id"), email))
        })
        .collect())
}

pub(super) async fn usage_breakdown(
    pool: &PgPool,
    filter: &UsageRecordFilter,
    group_expression: &str,
    limit: u32,
) -> PgUsageRecordStoreResult<Vec<UsageRecordBreakdown>> {
    let mut builder = QueryBuilder::<Postgres>::new("select ");
    builder.push(group_expression);
    builder.push(
        r" as name,
  coalesce(nullif(upstream_model, ''), model) as billing_model,
  service_tier,
  count(*) as request_count,
  coalesce(sum(input_tokens), 0)::bigint as input_tokens,
  coalesce(sum(output_tokens), 0)::bigint as output_tokens,
  coalesce(sum(cached_tokens), 0)::bigint as cached_tokens,
  coalesce(sum(latency_ms), 0)::bigint as latency_sum,
  count(latency_ms) as latency_count
from usage_records",
    );
    push_filter(&mut builder, filter, None)?;
    builder.push(" group by name, billing_model, service_tier order by request_count desc limit ");
    builder.push_bind(i64::from(limit.clamp(1, 50) * 8));

    let mut grouped = BTreeMap::<String, BreakdownAccumulator>::new();
    for row in builder.build().fetch_all(pool).await? {
        let name: String = row.get("name");
        let input_tokens = nonnegative(row.get("input_tokens"));
        let output_tokens = nonnegative(row.get("output_tokens"));
        let cached_tokens = nonnegative(row.get("cached_tokens"));
        let request_count = nonnegative(row.get("request_count"));
        let latency_sum = nonnegative(row.get("latency_sum"));
        let latency_count = nonnegative(row.get("latency_count"));
        let cost = usage_breakdown_cost(
            input_tokens,
            output_tokens,
            cached_tokens,
            &row.get::<String, _>("billing_model"),
            row.get::<Option<String>, _>("service_tier").as_deref(),
        );
        grouped
            .entry(name.clone())
            .or_insert_with(|| BreakdownAccumulator::new(name))
            .push(
                request_count,
                input_tokens,
                output_tokens,
                cached_tokens,
                cost,
                latency_sum,
                latency_count,
            );
    }
    let mut items = grouped
        .into_values()
        .map(BreakdownAccumulator::finish)
        .collect::<Vec<_>>();
    items.sort_by(|left, right| {
        right
            .request_count
            .cmp(&left.request_count)
            .then_with(|| left.name.cmp(&right.name))
    });
    items.truncate(limit.clamp(1, 50) as usize);
    Ok(items)
}

pub(super) async fn usage_trend(
    pool: &PgPool,
    filter: &UsageRecordFilter,
) -> PgUsageRecordStoreResult<Vec<UsageRecordTrendPoint>> {
    let mut builder = QueryBuilder::<Postgres>::new(
        r"
select
  to_char(created_at at time zone 'Asia/Shanghai', 'YYYY-MM-DD') as date,
  coalesce(nullif(upstream_model, ''), model) as billing_model,
  service_tier,
  coalesce(sum(input_tokens), 0)::bigint as input_tokens,
  coalesce(sum(output_tokens), 0)::bigint as output_tokens,
  coalesce(sum(cached_tokens), 0)::bigint as cached_tokens,
  coalesce(sum(latency_ms), 0)::bigint as latency_sum,
  count(latency_ms) as latency_count
from usage_records",
    );
    push_filter(&mut builder, filter, None)?;
    builder.push(" group by date, billing_model, service_tier order by date asc");

    let mut days = BTreeMap::<String, UsageTrendAccumulator>::new();
    for row in builder.build().fetch_all(pool).await? {
        let date: String = row.get("date");
        let input_tokens = nonnegative(row.get("input_tokens"));
        let output_tokens = nonnegative(row.get("output_tokens"));
        let cached_tokens = nonnegative(row.get("cached_tokens"));
        let cost = usage_breakdown_cost(
            input_tokens,
            output_tokens,
            cached_tokens,
            &row.get::<String, _>("billing_model"),
            row.get::<Option<String>, _>("service_tier").as_deref(),
        );
        days.entry(date.clone())
            .or_insert_with(|| UsageTrendAccumulator::new(date))
            .push(
                input_tokens,
                output_tokens,
                cached_tokens,
                cost,
                nonnegative(row.get("latency_sum")),
                nonnegative(row.get("latency_count")),
            );
    }
    let mut points = days
        .into_values()
        .map(UsageTrendAccumulator::finish)
        .collect::<Vec<_>>();
    if points.len() > 60 {
        points = points.split_off(points.len() - 60);
    }
    Ok(points)
}

pub(super) fn usage_record_from_row(row: &sqlx::postgres::PgRow) -> UsageRecord {
    UsageRecord {
        id: row.get("id"),
        request_id: row.get("request_id"),
        client_api_key_id: row.get("client_api_key_id"),
        kind: row.get("kind"),
        provider: row.get("provider"),
        account_id: row.get("account_id"),
        route: row.get("route"),
        model: row.get("model"),
        requested_model: row.get("requested_model"),
        upstream_model: row.get("upstream_model"),
        service_tier: row.get("service_tier"),
        status_code: row.get::<i32, _>("status_code").into(),
        transport: row.get("transport"),
        attempt_index: row.get("attempt_index"),
        response_id: row.get("response_id"),
        upstream_request_id: row.get("upstream_request_id"),
        latency_ms: row.get("latency_ms"),
        first_token_ms: row.get("first_token_ms"),
        input_tokens: row.get("input_tokens"),
        output_tokens: row.get("output_tokens"),
        cached_tokens: row.get("cached_tokens"),
        reasoning_tokens: row.get("reasoning_tokens"),
        message: row.get("message"),
        metadata: row
            .get::<sqlx::types::Json<serde_json::Value>, _>("metadata_json")
            .0,
        created_at: row.get("created_at"),
    }
}

fn nonnegative(value: Option<i64>) -> u64 {
    optional_nonnegative_i64_to_u64(value)
}

fn usage_breakdown_cost(
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    model: &str,
    service_tier: Option<&str>,
) -> f64 {
    billing::calculate_cost(
        input_tokens,
        output_tokens,
        cached_tokens,
        model,
        service_tier,
    )
}

struct BreakdownAccumulator {
    item: UsageRecordBreakdown,
    latency_sum: u64,
    latency_count: u64,
}

impl BreakdownAccumulator {
    fn new(name: String) -> Self {
        Self {
            item: UsageRecordBreakdown {
                name,
                ..Default::default()
            },
            latency_sum: 0,
            latency_count: 0,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        requests: u64,
        input: u64,
        output: u64,
        cached: u64,
        cost: f64,
        latency_sum: u64,
        latency_count: u64,
    ) {
        self.item.request_count += requests;
        self.item.input_tokens += input;
        self.item.output_tokens += output;
        self.item.cached_tokens += cached;
        self.item.total_tokens += input + output;
        self.item.cost += cost;
        self.item.actual_cost += cost;
        self.item.account_cost += cost;
        self.latency_sum += latency_sum;
        self.latency_count += latency_count;
    }

    fn finish(mut self) -> UsageRecordBreakdown {
        self.item.average_latency_ms =
            (self.latency_count > 0).then(|| self.latency_sum as f64 / self.latency_count as f64);
        self.item
    }
}

struct UsageTrendAccumulator {
    point: UsageRecordTrendPoint,
    latency_sum: u64,
    latency_count: u64,
}

impl UsageTrendAccumulator {
    fn new(date: String) -> Self {
        Self {
            point: UsageRecordTrendPoint {
                date,
                ..Default::default()
            },
            latency_sum: 0,
            latency_count: 0,
        }
    }

    fn push(
        &mut self,
        input: u64,
        output: u64,
        cached: u64,
        cost: f64,
        latency_sum: u64,
        latency_count: u64,
    ) {
        self.point.input_tokens += input;
        self.point.output_tokens += output;
        self.point.cached_tokens += cached;
        self.point.total_tokens += input + output;
        self.point.cost += cost;
        self.point.actual_cost += cost;
        self.latency_sum += latency_sum;
        self.latency_count += latency_count;
    }

    fn finish(mut self) -> UsageRecordTrendPoint {
        self.point.average_latency_ms =
            (self.latency_count > 0).then(|| self.latency_sum as f64 / self.latency_count as f64);
        self.point
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct UsageRecordSummary {
    pub total_requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub total_tokens: u64,
    pub average_latency_ms: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRecordAccountUsage {
    pub account_id: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub last_used_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageRecordModelSource {
    Requested,
    Upstream,
    Mapping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageRecordEndpointSource {
    Inbound,
    Upstream,
    Path,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageRecordBreakdown {
    pub name: String,
    pub request_count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub total_tokens: u64,
    pub cost: f64,
    pub actual_cost: f64,
    pub account_cost: f64,
    pub average_latency_ms: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageRecordTrendPoint {
    pub date: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub total_tokens: u64,
    pub cost: f64,
    pub actual_cost: f64,
    pub average_latency_ms: Option<f64>,
}
