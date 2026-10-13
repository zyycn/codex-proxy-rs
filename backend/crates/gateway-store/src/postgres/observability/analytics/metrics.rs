//! 窄事实的可合并指标状态，不持久化已经计算的分位数

use super::super::*;
use super::histogram::Histogram;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;

pub(super) const TOKEN_METRICS: [&str; 8] = [
    "input_tokens",
    "output_tokens",
    "cached_input_tokens",
    "cache_write_input_tokens",
    "reasoning_output_tokens",
    "total_tokens",
    "image_input_tokens",
    "image_output_tokens",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct MetricState {
    pub(super) counts: BTreeMap<String, u64>,
    pub(super) distributions: BTreeMap<String, Histogram>,
    pub(super) costs: BTreeMap<String, String>,
    pub(super) last_used_at: Option<DateTime<Utc>>,
    #[serde(skip)]
    exact_percentiles: Option<BTreeMap<String, Vec<Option<f64>>>>,
}

impl MetricState {
    pub fn decode(row: &PgRow) -> StoreResult<Self> {
        Ok(Self {
            // 直接解码为指标类型，避免每个汇总行先构造通用 JSON 树再遍历转换
            counts: get::<sqlx::types::Json<BTreeMap<String, u64>>>(row, "counts")?.0,
            costs: get::<sqlx::types::Json<BTreeMap<String, String>>>(row, "costs")?.0,
            distributions: get::<sqlx::types::Json<BTreeMap<String, Histogram>>>(
                row,
                "distributions",
            )?
            .0,
            last_used_at: get(row, "last_used_at")?,
            exact_percentiles: None,
        })
    }
    pub(super) fn decode_aggregate(row: &PgRow, exact: bool) -> StoreResult<Self> {
        let mut state = Self::decode(row)?;
        if exact {
            state.exact_percentiles = Some(
                get::<sqlx::types::Json<BTreeMap<String, Vec<Option<f64>>>>>(
                    row,
                    "exact_percentiles",
                )?
                .0,
            );
        }
        Ok(state)
    }
    pub fn encode(&self) -> StoreResult<(serde_json::Value, serde_json::Value, serde_json::Value)> {
        Ok((
            serde_json::to_value(&self.counts)
                .map_err(|error| postgres_unavailable("encode analytics counters", error))?,
            serde_json::to_value(&self.costs)
                .map_err(|error| postgres_unavailable("encode analytics costs", error))?,
            serde_json::to_value(&self.distributions)
                .map_err(|error| postgres_unavailable("encode analytics histograms", error))?,
        ))
    }
    pub fn count(&self, name: &str) -> u64 {
        self.counts.get(name).copied().unwrap_or_default()
    }
    pub fn optional_count(&self, name: &str) -> Option<u64> {
        (self.count(&format!("{name}_known_count")) > 0).then(|| self.count(name))
    }
    pub fn last_used_at(&self) -> Option<DateTime<Utc>> {
        self.last_used_at
    }
    pub(super) fn has_exact_percentiles(&self) -> bool {
        self.exact_percentiles.is_some()
    }
    pub(super) fn increment(&mut self, name: &str, amount: u64) -> StoreResult<()> {
        // 合并同一分组时键通常已经存在，避免每条事实都为已有指标分配字符串
        let count = match self.counts.get_mut(name) {
            Some(count) => count,
            None => self.counts.entry(name.to_owned()).or_default(),
        };
        *count = count
            .checked_add(amount)
            .ok_or_else(|| invalid("analytics counter overflow"))?;
        Ok(())
    }
    fn sample(&mut self, name: &str, sample: Option<f64>) -> StoreResult<()> {
        if let Some(sample) = sample {
            self.distributions
                .entry(name.to_owned())
                .or_default()
                .add(sample)?;
        }
        Ok(())
    }
    pub(super) fn add_cost(&mut self, currency: &str, amount: &str) -> StoreResult<()> {
        let amount: DecimalAmount = amount
            .parse()
            .map_err(|_| invalid("analytics invalid cost"))?;
        let total = if let Some(previous) = self.costs.get(currency) {
            previous
                .parse::<DecimalAmount>()
                .map_err(|_| invalid("analytics invalid stored cost"))?
                .checked_add(&amount)
                .ok_or_else(|| invalid("analytics cost overflow"))?
        } else {
            amount
        };
        self.costs.insert(currency.to_owned(), total.to_string());
        Ok(())
    }
    pub fn merge(&mut self, other: &Self) -> StoreResult<()> {
        // 精确分位数只属于数据库已经完成分组的结果，不能把分位数再平均或合并
        if self.count("request_count") > 0
            && other.count("request_count") > 0
            && (self.exact_percentiles.is_some() || other.exact_percentiles.is_some())
        {
            return Err(invalid(
                "cannot merge finalized exact analytics percentiles",
            ));
        }
        if self.count("request_count") == 0 {
            self.exact_percentiles.clone_from(&other.exact_percentiles);
        }
        self.last_used_at = self.last_used_at.max(other.last_used_at);
        for (name, count) in &other.counts {
            self.increment(name, *count)?;
        }
        for (name, distribution) in &other.distributions {
            let stored = match self.distributions.get_mut(name) {
                Some(stored) => stored,
                None => self.distributions.entry(name.clone()).or_default(),
            };
            stored.merge(distribution)?;
        }
        for (currency, amount) in &other.costs {
            self.add_cost(currency, amount)?;
        }
        Ok(())
    }
    pub fn add_row(&mut self, row: &PgRow) -> StoreResult<()> {
        let outcome: String = get(row, "outcome")?;
        let completed: bool = get(row, "is_completed_usage")?;
        self.increment("request_count", 1)?;
        for (value, name) in [
            ("succeeded", "success_count"),
            ("failed", "failure_count"),
            ("cancelled", "cancelled_count"),
            ("incomplete", "incomplete_count"),
        ] {
            if outcome == value {
                self.increment(name, 1)?;
            }
        }
        let client_status: Option<i32> = get(row, "client_status_code")?;
        if client_status.is_some_and(|value| (400..=499).contains(&value)) {
            self.increment("caller_error_count", 1)?;
        }
        let attempts = u64::try_from(get::<i32>(row, "attempt_count")?)
            .map_err(|_| invalid("negative analytics attempt count"))?;
        self.increment("attempt_count", attempts)?;
        self.increment("retry_count", attempts.saturating_sub(1))?;
        if attempts > 1 {
            self.increment("retried_request_count", 1)?;
        }
        if attempts > 0 {
            for (value, name) in [
                ("succeeded", "attempt_success_count"),
                ("failed", "attempt_failure_count"),
                ("cancelled", "attempt_cancelled_count"),
                ("incomplete", "attempt_incomplete_count"),
            ] {
                if outcome == value {
                    self.increment(name, 1)?;
                }
            }
            if outcome == "failed" {
                let status: Option<i32> =
                    get::<Option<i32>>(row, "upstream_status_code")?.or(client_status);
                let kind: Option<String> = get(row, "error_kind")?;
                if matches!(kind.as_deref(), Some("rate_limited" | "quota_exhausted"))
                    || status == Some(429)
                {
                    self.increment("rate_limited_count", 1)?;
                }
                if matches!(
                    kind.as_deref(),
                    Some("authentication" | "authorization" | "invalid_credential")
                ) || matches!(status, Some(401 | 403))
                {
                    self.increment("auth_failure_count", 1)?;
                }
                if status.is_some_and(|status| (500..=599).contains(&status)) {
                    self.increment("provider_5xx_count", 1)?;
                }
            }
        }
        for (source, target) in [
            ("ops_failure_count", "attempt_failure_count"),
            ("ops_rate_limited_count", "rate_limited_count"),
            ("ops_auth_failure_count", "auth_failure_count"),
            ("ops_provider_5xx_count", "provider_5xx_count"),
        ] {
            self.increment(target, unsigned(row, source)?)?;
        }
        for name in ["admission_decision_ms", "account_selection_wait_ms"] {
            self.sample(
                name,
                get::<Option<i64>>(row, name)?.map(|value| value as f64),
            )?;
        }
        let capacity_total: Option<i64> = get(row, "capacity_total_slots")?;
        if capacity_total.is_some() {
            self.increment("capacity_sample_count", 1)?;
        }
        if let (Some(used), Some(total)) = (
            get::<Option<i64>>(row, "capacity_used_slots")?,
            capacity_total,
        ) && total > 0
        {
            self.sample("capacity", Some(used as f64 / total as f64 * 10_000.0))?;
        }
        if completed {
            self.increment("completed_request_count", 1)?;
            self.last_used_at = self.last_used_at.max(Some(get(row, "started_at")?));
            for (value, name) in [
                (Some(true), "image_request_count"),
                (Some(false), "image_request_failed_count"),
            ] {
                if get::<Option<bool>>(row, "image_generation_succeeded")? == value {
                    self.increment(name, 1)?;
                }
            }
            for name in TOKEN_METRICS {
                if let Some(value) = optional_unsigned(row, name)? {
                    self.increment(name, value)?;
                    self.increment(&format!("{name}_known_count"), 1)?;
                }
            }
            self.increment(
                "window_total_tokens",
                optional_unsigned(row, "total_tokens")?.unwrap_or(
                    optional_unsigned(row, "input_tokens")?
                        .unwrap_or_default()
                        .checked_add(optional_unsigned(row, "output_tokens")?.unwrap_or_default())
                        .ok_or_else(|| invalid("analytics token overflow"))?,
                ),
            )?;
            if get::<Option<i64>>(row, "input_tokens")?.is_some() {
                self.increment("cache_eligible_request_count", 1)?;
                if optional_unsigned(row, "cached_input_tokens")?.is_some_and(|value| value > 0) {
                    self.increment("cache_hit_request_count", 1)?;
                }
            }
            for name in ["latency_ms", "first_token_ms"] {
                let value = optional_unsigned(row, name)?;
                if let Some(value) = value {
                    self.increment(&format!("{name}_sum"), value)?;
                }
                self.sample(name, value.map(|value| value as f64))?;
            }
            if let (Some(tokens), Some(milliseconds)) = (
                optional_unsigned(row, "output_tokens")?,
                optional_unsigned(row, "upstream_response_ms")?,
            ) && tokens > 0
                && milliseconds > 0
            {
                self.sample(
                    "throughput",
                    Some(tokens as f64 * 1000.0 / milliseconds as f64),
                )?;
            }
            for (name, source) in [
                ("standard_usd", "standard_cost_amount_text"),
                ("no_cache_usd", "no_cache_cost_amount_text"),
            ] {
                if let Some(amount) = get::<Option<String>>(row, source)? {
                    self.increment(&format!("{name}_known_count"), 1)?;
                    self.add_cost(name, &amount)?;
                }
            }
            let source: String = get(row, "cost_source")?;
            self.increment(&format!("{source}_count"), 1)?;
            if let (Some(currency), Some(amount)) = (
                get::<Option<String>>(row, "cost_currency")?,
                get::<Option<String>>(row, "cost_amount_text")?,
            ) {
                self.add_cost(&currency, &amount)?;
            }
        }
        Ok(())
    }
    fn histogram(&self, name: &str) -> Histogram {
        self.distributions.get(name).cloned().unwrap_or_default()
    }
    pub fn percentile(&self, name: &str, fraction: f64) -> Option<f64> {
        if let Some(percentiles) = &self.exact_percentiles {
            let index = [0.1, 0.5, 0.9, 0.95, 0.99]
                .iter()
                .position(|value| (*value - fraction).abs() < f64::EPSILON)?;
            return percentiles.get(name)?.get(index).copied().flatten();
        }
        self.histogram(name).percentile(fraction)
    }
    fn percentiles(&self, name: &str) -> StoreResult<LatencyPercentiles> {
        let percentile = |fraction| {
            self.percentile(name, fraction)
                .map(|value| {
                    PercentileMilliseconds::new(value)
                        .map_err(|_| invalid("invalid analytics percentile"))
                })
                .transpose()
        };
        Ok(LatencyPercentiles {
            p50_ms: percentile(0.5)?,
            p95_ms: percentile(0.95)?,
            p99_ms: percentile(0.99)?,
        })
    }
    pub fn request_metrics(&self) -> StoreResult<RequestMetrics> {
        let latency = self.histogram("latency_ms");
        let first = self.histogram("first_token_ms");
        let capacity = self.histogram("capacity");
        Ok(RequestMetrics {
            request_count: self.count("request_count"),
            success_count: self.count("success_count"),
            failure_count: self.count("failure_count"),
            cancelled_count: self.count("cancelled_count"),
            incomplete_count: self.count("incomplete_count"),
            caller_error_count: self.count("caller_error_count"),
            input_tokens: self.count("input_tokens"),
            output_tokens: self.count("output_tokens"),
            cached_input_tokens: self.count("cached_input_tokens"),
            cache_write_input_tokens: self.count("cache_write_input_tokens"),
            reasoning_output_tokens: self.count("reasoning_output_tokens"),
            total_tokens: self.count("total_tokens"),
            cache_eligible_request_count: self.count("cache_eligible_request_count"),
            cache_hit_request_count: self.count("cache_hit_request_count"),
            capacity_sample_count: self.count("capacity_sample_count"),
            first_token_latency_sum_ms: self.count("first_token_ms_sum"),
            first_token_latency_count: first.count,
            latency_sum_ms: self.count("latency_ms_sum"),
            latency_count: latency.count,
            max_latency_ms: latency.maximum.map(|value| value as u64),
            min_latency_ms: latency.minimum.map(|value| value as u64),
            latency_percentiles: self.percentiles("latency_ms")?,
            first_token_latency_percentiles: self.percentiles("first_token_ms")?,
            admission_decision_count: self.histogram("admission_decision_ms").count,
            admission_decision_percentiles: self.percentiles("admission_decision_ms")?,
            account_selection_wait_count: self.histogram("account_selection_wait_ms").count,
            account_selection_wait_percentiles: self.percentiles("account_selection_wait_ms")?,
            output_throughput_p10: self
                .percentile("throughput", 0.1)
                .map(|value| value.round() as u64),
            output_throughput_p50: self
                .percentile("throughput", 0.5)
                .map(|value| value.round() as u64),
            output_throughput_p90: self
                .percentile("throughput", 0.9)
                .map(|value| value.round() as u64),
            capacity_utilization_avg_basis_points: (capacity.count > 0)
                .then(|| (capacity.sum / capacity.count as f64).round() as u64),
            capacity_utilization_p95_basis_points: self
                .percentile("capacity", 0.95)
                .map(|value| value.round() as u64),
        })
    }
    pub fn coverage(&self) -> CostCoverage {
        CostCoverage {
            provider_reported_count: self.count("provider_reported_count"),
            calculated_count: self.count("calculated_count"),
            unavailable_count: self.count("unavailable_count"),
        }
    }
    pub fn projected_cost(&self, name: &str) -> StoreResult<Option<DecimalAmount>> {
        self.costs
            .get(name)
            .map(|value| {
                value
                    .parse()
                    .map_err(|_| invalid("analytics invalid projected cost"))
            })
            .transpose()
    }
    pub fn costs(&self) -> StoreResult<Vec<CurrencyCostTotal>> {
        self.costs
            .iter()
            .filter(|(currency, _)| !matches!(currency.as_str(), "standard_usd" | "no_cache_usd"))
            .map(|(currency, amount)| {
                Ok(CurrencyCostTotal {
                    currency: currency.clone(),
                    amount: amount
                        .parse()
                        .map_err(|_| invalid("analytics invalid cost"))?,
                })
            })
            .collect()
    }
    pub fn attempts(&self) -> StoreResult<AttemptMetrics> {
        Ok(AttemptMetrics {
            attempt_count: self.count("attempt_count"),
            success_count: self.count("attempt_success_count"),
            failure_count: self.count("attempt_failure_count"),
            cancelled_count: self.count("attempt_cancelled_count"),
            incomplete_count: self.count("attempt_incomplete_count"),
            rate_limited_count: self.count("rate_limited_count"),
            auth_failure_count: self.count("auth_failure_count"),
            provider_5xx_count: self.count("provider_5xx_count"),
            cost_coverage: self.coverage(),
            costs: self.costs()?,
        })
    }
}
