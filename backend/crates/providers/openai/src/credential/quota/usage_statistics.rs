use std::{
    cmp::{Ordering, Reverse},
    collections::BTreeMap,
};

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveTime, TimeDelta, Utc};
use gateway_core::accounting::{Decimal, Money};
use serde_json::Value;

use crate::transport::{OpenAiBillingUsage, openai_aggregate_billing_breakdown};

const EPSILON: f64 = 1e-9;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CodexUsageStatisticsServiceTier {
    Standard,
    Fast,
}

impl CodexUsageStatisticsServiceTier {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Fast => "fast",
        }
    }

    const fn billing_service_tier(self) -> &'static str {
        match self {
            Self::Standard => "default",
            Self::Fast => "fast",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CodexUsageStatisticsTokens {
    pub uncached_input: u64,
    pub cached_input: u64,
    pub output: u64,
    pub total: u64,
}

impl CodexUsageStatisticsTokens {
    fn checked_add(self, other: Self) -> Option<Self> {
        Some(Self {
            uncached_input: self.uncached_input.checked_add(other.uncached_input)?,
            cached_input: self.cached_input.checked_add(other.cached_input)?,
            output: self.output.checked_add(other.output)?,
            total: self.total.checked_add(other.total)?,
        })
    }

    const fn is_empty(self) -> bool {
        self.total == 0 && self.uncached_input == 0 && self.cached_input == 0 && self.output == 0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodexUsageStatisticsCycle {
    pub offset: i8,
    pub start_at: DateTime<Utc>,
    pub end_at: DateTime<Utc>,
    pub window_seconds: u64,
    pub used_percent: Option<f64>,
    pub is_current: bool,
    pub can_go_previous: bool,
    pub can_go_next: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodexUsageStatisticsModel {
    pub key: String,
    pub model: String,
    pub service_tier: CodexUsageStatisticsServiceTier,
    pub credit_share: Option<f64>,
    pub quota_share: Option<f64>,
    pub tokens: CodexUsageStatisticsTokens,
    pub estimated_cost: Option<Money>,
    pub has_unknown_pricing: bool,
    pub has_estimated_allocation: bool,
    pub has_rate_fallback: bool,
    pub has_missing_token_data: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodexUsageStatisticsDay {
    pub date: NaiveDate,
    pub credit_share: Option<f64>,
    pub tokens: CodexUsageStatisticsTokens,
    pub estimated_cost: Option<Money>,
    pub has_unknown_pricing: bool,
    pub has_missing_token_data: bool,
    pub is_boundary_day: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodexUsageStatisticsSummary {
    pub tokens: CodexUsageStatisticsTokens,
    pub estimated_cost: Option<Money>,
    pub projected_tokens: Option<u64>,
    pub projected_cost: Option<Money>,
    pub day_count: u32,
    pub has_unknown_pricing: bool,
    pub has_missing_token_data: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodexUsageStatistics {
    pub cycle: CodexUsageStatisticsCycle,
    pub summary: CodexUsageStatisticsSummary,
    pub models: Vec<CodexUsageStatisticsModel>,
    pub daily: Vec<CodexUsageStatisticsDay>,
}

#[derive(Debug, Clone, Copy)]
pub struct CodexUsageStatisticsPeriod {
    pub cycle_offset: i8,
    pub start_at: DateTime<Utc>,
    pub end_at: DateTime<Utc>,
    pub window_seconds: u64,
    pub query_start_date: NaiveDate,
    pub query_end_date: NaiveDate,
}

pub fn statistics_period(
    current_start_at: DateTime<Utc>,
    current_end_at: DateTime<Utc>,
    window_seconds: u64,
    cycle_offset: i8,
    timezone: FixedOffset,
    now: DateTime<Utc>,
) -> Option<CodexUsageStatisticsPeriod> {
    let window_delta = TimeDelta::seconds(i64::try_from(window_seconds).ok()?);
    let shift = window_delta.checked_mul(i32::from(cycle_offset))?;
    let start_at = current_start_at.checked_add_signed(shift)?;
    let end_at = current_end_at.checked_add_signed(shift)?;
    let query_start_date = start_at
        .checked_sub_signed(TimeDelta::days(1))?
        .with_timezone(&timezone)
        .date_naive();
    let effective_end = end_at.min(now);
    let query_end_date = effective_end
        .checked_add_signed(TimeDelta::days(1))?
        .with_timezone(&timezone)
        .date_naive();
    Some(CodexUsageStatisticsPeriod {
        cycle_offset,
        start_at,
        end_at,
        window_seconds,
        query_start_date,
        query_end_date,
    })
}

pub(super) struct BuildUsageStatistics<'a> {
    pub period: CodexUsageStatisticsPeriod,
    pub timezone: FixedOffset,
    pub current_used_percent: Option<f64>,
    pub now: DateTime<Utc>,
    pub model_breakdown: &'a Value,
    pub daily_totals: &'a Value,
    pub max_cycle_offset: u8,
}

pub(super) fn build_usage_statistics(input: BuildUsageStatistics<'_>) -> CodexUsageStatistics {
    let BuildUsageStatistics {
        period,
        timezone,
        current_used_percent,
        now,
        model_breakdown,
        daily_totals,
        max_cycle_offset,
    } = input;
    let all_days = build_personal_days(daily_totals, model_breakdown);
    let selected_days = select_cycle_days(
        &all_days,
        period.start_at,
        period.end_at,
        timezone,
        now,
        true,
    );
    let is_current = period.cycle_offset == 0;
    let used_percent = is_current
        .then_some(current_used_percent)
        .flatten()
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(0.0, 100.0));
    let used_ratio = used_percent
        .map(|value| value.clamp(0.0, 100.0) / 100.0)
        .unwrap_or_default();
    let summary = summarize(&selected_days, used_ratio, is_current);
    let models = aggregate_models(&selected_days, used_ratio, is_current);
    let total_credits = selected_days
        .iter()
        .map(|day| day.credit_weight)
        .sum::<f64>();
    let mut daily = selected_days
        .into_iter()
        .map(|day| CodexUsageStatisticsDay {
            date: day.date,
            credit_share: (total_credits > EPSILON).then_some(day.credit_weight / total_credits),
            tokens: day.tokens,
            estimated_cost: day.estimated_cost,
            has_unknown_pricing: day.has_unknown_pricing,
            has_missing_token_data: day.has_missing_token_data,
            is_boundary_day: day.is_boundary_day,
        })
        .collect::<Vec<_>>();
    daily.sort_by_key(|day| Reverse(day.date));

    CodexUsageStatistics {
        cycle: CodexUsageStatisticsCycle {
            offset: period.cycle_offset,
            start_at: period.start_at,
            end_at: period.end_at,
            window_seconds: period.window_seconds,
            used_percent,
            is_current,
            can_go_previous: period.cycle_offset > -(max_cycle_offset as i8),
            can_go_next: period.cycle_offset < 0,
        },
        summary,
        models,
        daily,
    }
}

#[derive(Debug, Clone)]
struct DailyModelUsage {
    model: String,
    service_tier: CodexUsageStatisticsServiceTier,
    credits: f64,
    tokens: CodexUsageStatisticsTokens,
    estimated_cost: Option<Money>,
    has_unknown_pricing: bool,
    has_estimated_allocation: bool,
    has_rate_fallback: bool,
    has_missing_token_data: bool,
}

#[derive(Debug, Clone)]
struct DailyUsage {
    date: NaiveDate,
    credit_weight: f64,
    tokens: CodexUsageStatisticsTokens,
    estimated_cost: Option<Money>,
    has_unknown_pricing: bool,
    has_missing_token_data: bool,
    is_boundary_day: bool,
    models: Vec<DailyModelUsage>,
}

#[derive(Default)]
struct MoneySum {
    value: Option<Money>,
    overflowed: bool,
}

impl MoneySum {
    fn push(&mut self, value: Option<Money>) {
        let Some(value) = value else {
            return;
        };
        if self.overflowed {
            return;
        }
        self.value = match self.value {
            Some(current) => match current.checked_add(value) {
                Some(total) => Some(total),
                None => {
                    self.overflowed = true;
                    None
                }
            },
            None => Some(value),
        };
    }

    fn finish(self) -> Option<Money> {
        (!self.overflowed).then_some(self.value).flatten()
    }
}

fn build_personal_days(totals_payload: &Value, breakdown_payload: &Value) -> Vec<DailyUsage> {
    let totals_by_date = rows_by_date(totals_payload);
    let breakdown_by_date = rows_by_date(breakdown_payload);
    let mut dates = totals_by_date
        .keys()
        .chain(breakdown_by_date.keys())
        .copied()
        .collect::<Vec<_>>();
    dates.sort_unstable();
    dates.dedup();

    dates
        .into_iter()
        .filter_map(|date| {
            let totals_row = totals_by_date.get(&date).unwrap_or(&Value::Null);
            let breakdown_row = breakdown_by_date.get(&date).unwrap_or(&Value::Null);
            let totals = token_parts(totals_row.get("totals").unwrap_or(totals_row));
            let credit_weight = day_credit_weight(breakdown_row);
            let weighted = weighted_models(breakdown_row);
            if totals.is_empty() {
                if credit_weight <= EPSILON {
                    return None;
                }
                let models = weighted
                    .into_iter()
                    .map(|model| DailyModelUsage {
                        model: model.model,
                        service_tier: model.service_tier,
                        credits: model.weight,
                        tokens: CodexUsageStatisticsTokens::default(),
                        estimated_cost: None,
                        has_unknown_pricing: false,
                        has_estimated_allocation: false,
                        has_rate_fallback: false,
                        has_missing_token_data: true,
                    })
                    .collect();
                return Some(finalize_day(date, credit_weight, totals, models, true));
            }

            let has_model_breakdown = !weighted.is_empty();
            let weighted = if has_model_breakdown {
                weighted
            } else {
                vec![WeightedModel {
                    model: "unknown".to_owned(),
                    service_tier: CodexUsageStatisticsServiceTier::Standard,
                    weight: 1.0,
                }]
            };
            let allocation = rate_adjusted_shares(&weighted, totals);
            let uncached = allocate_integer_total(totals.uncached_input, &allocation.token_shares);
            let cached = allocate_integer_total(totals.cached_input, &allocation.token_shares);
            let output = allocate_integer_total(totals.output, &allocation.token_shares);
            let total = allocate_integer_total(totals.total, &allocation.token_shares);
            let estimated_allocation = weighted.len() > 1 || !has_model_breakdown;
            let rate_fallback = allocation.has_rate_fallback || !has_model_breakdown;
            let models = weighted
                .into_iter()
                .enumerate()
                .map(|(index, model)| {
                    let tokens = CodexUsageStatisticsTokens {
                        uncached_input: uncached.get(index).copied().unwrap_or_default(),
                        cached_input: cached.get(index).copied().unwrap_or_default(),
                        output: output.get(index).copied().unwrap_or_default(),
                        total: total.get(index).copied().unwrap_or_default(),
                    };
                    let estimated_cost = aggregate_cost(&model.model, model.service_tier, tokens);
                    DailyModelUsage {
                        model: model.model,
                        service_tier: model.service_tier,
                        credits: if has_model_breakdown {
                            model.weight
                        } else {
                            0.0
                        },
                        tokens,
                        has_unknown_pricing: estimated_cost.is_none() && !tokens.is_empty(),
                        estimated_cost,
                        has_estimated_allocation: estimated_allocation,
                        has_rate_fallback: rate_fallback,
                        has_missing_token_data: false,
                    }
                })
                .collect();
            Some(finalize_day(date, credit_weight, totals, models, false))
        })
        .collect()
}

fn finalize_day(
    date: NaiveDate,
    credit_weight: f64,
    tokens: CodexUsageStatisticsTokens,
    models: Vec<DailyModelUsage>,
    has_missing_token_data: bool,
) -> DailyUsage {
    let mut costs = MoneySum::default();
    for model in &models {
        costs.push(model.estimated_cost);
    }
    DailyUsage {
        date,
        credit_weight,
        tokens,
        estimated_cost: costs.finish(),
        has_unknown_pricing: models.iter().any(|model| model.has_unknown_pricing),
        has_missing_token_data,
        is_boundary_day: false,
        models,
    }
}

#[derive(Debug)]
struct WeightedModel {
    model: String,
    service_tier: CodexUsageStatisticsServiceTier,
    weight: f64,
}

struct Allocation {
    token_shares: Vec<f64>,
    has_rate_fallback: bool,
}

fn weighted_models(row: &Value) -> Vec<WeightedModel> {
    value_array(row.get("models"))
        .iter()
        .filter_map(|value| {
            let weight = non_negative_f64(value.get("credits"));
            (weight > EPSILON).then(|| WeightedModel {
                model: model_name(value),
                service_tier: model_service_tier(value.get("speed").or_else(|| value.get("mode"))),
                weight,
            })
        })
        .collect()
}

fn rate_adjusted_shares(
    models: &[WeightedModel],
    totals: CodexUsageStatisticsTokens,
) -> Allocation {
    let total_weight = models.iter().map(|model| model.weight).sum::<f64>();
    let credit_shares = models
        .iter()
        .map(|model| model.weight / total_weight)
        .collect::<Vec<_>>();
    if models.len() == 1 {
        return Allocation {
            token_shares: vec![1.0],
            has_rate_fallback: false,
        };
    }
    let classified_tokens = totals
        .uncached_input
        .saturating_add(totals.cached_input)
        .saturating_add(totals.output);
    if classified_tokens == 0 {
        return Allocation {
            token_shares: credit_shares,
            has_rate_fallback: true,
        };
    }
    let rates = models
        .iter()
        .map(|model| blended_rate(&model.model, model.service_tier, totals, classified_tokens))
        .collect::<Vec<_>>();
    let known = rates
        .iter()
        .enumerate()
        .filter_map(|(index, rate)| rate.map(|rate| (index, rate)))
        .collect::<Vec<_>>();
    if known.is_empty() {
        return Allocation {
            token_shares: credit_shares,
            has_rate_fallback: true,
        };
    }
    let known_weight = known
        .iter()
        .map(|(index, _)| models[*index].weight)
        .sum::<f64>();
    let fallback_rate = known
        .iter()
        .map(|(index, rate)| rate * models[*index].weight / known_weight)
        .sum::<f64>();
    let inverse = models
        .iter()
        .enumerate()
        .map(|(index, model)| model.weight / rates[index].unwrap_or(fallback_rate))
        .collect::<Vec<_>>();
    let inverse_total = inverse.iter().sum::<f64>();
    Allocation {
        token_shares: inverse
            .into_iter()
            .map(|value| value / inverse_total)
            .collect(),
        has_rate_fallback: known.len() != models.len(),
    }
}

#[allow(clippy::cast_precision_loss)]
fn blended_rate(
    model: &str,
    service_tier: CodexUsageStatisticsServiceTier,
    totals: CodexUsageStatisticsTokens,
    classified_tokens: u64,
) -> Option<f64> {
    let cost = aggregate_cost(model, service_tier, totals)?;
    let rate = cost.amount().scaled() as f64 / classified_tokens as f64;
    (rate > EPSILON).then_some(rate)
}

fn aggregate_cost(
    model: &str,
    service_tier: CodexUsageStatisticsServiceTier,
    tokens: CodexUsageStatisticsTokens,
) -> Option<Money> {
    let input = tokens.uncached_input.checked_add(tokens.cached_input)?;
    openai_aggregate_billing_breakdown(
        model,
        OpenAiBillingUsage::new(input, tokens.output, tokens.cached_input, 0),
        Some(service_tier.billing_service_tier()),
    )
    .map(|breakdown| breakdown.total_amount())
}

#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn allocate_integer_total(total: u64, shares: &[f64]) -> Vec<u64> {
    if shares.is_empty() {
        return Vec::new();
    }
    let share_total = shares.iter().copied().sum::<f64>();
    if !share_total.is_finite() || share_total <= EPSILON {
        return vec![0; shares.len()];
    }
    let raw = shares
        .iter()
        .map(|share| total as f64 * (share.max(0.0) / share_total))
        .collect::<Vec<_>>();
    let mut allocated = raw
        .iter()
        .map(|value| value.floor() as u64)
        .collect::<Vec<_>>();
    let allocated_total = allocated.iter().copied().sum::<u64>();
    let mut remainder = total.saturating_sub(allocated_total);
    let mut order = raw
        .iter()
        .enumerate()
        .map(|(index, value)| (index, value - value.floor()))
        .collect::<Vec<_>>();
    order.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    let mut index = 0_usize;
    while remainder > 0 {
        let target = order[index % order.len()].0;
        allocated[target] = allocated[target].saturating_add(1);
        remainder -= 1;
        index += 1;
    }
    allocated
}

fn select_cycle_days(
    days: &[DailyUsage],
    start_at: DateTime<Utc>,
    end_at: DateTime<Utc>,
    timezone: FixedOffset,
    now: DateTime<Utc>,
    mark_boundary: bool,
) -> Vec<DailyUsage> {
    let local_start = start_at.with_timezone(&timezone);
    let start_date = local_start.date_naive();
    let effective_end = end_at.min(now);
    let end_date = effective_end.with_timezone(&timezone).date_naive();
    let exclude_end_date = end_at <= now && end_date > start_date;
    let is_boundary = local_start.time() != NaiveTime::MIN;
    days.iter()
        .filter(|day| {
            day.date >= start_date
                && if exclude_end_date {
                    day.date < end_date
                } else {
                    day.date <= end_date
                }
        })
        .cloned()
        .map(|mut day| {
            day.is_boundary_day = mark_boundary && is_boundary && day.date == start_date;
            day
        })
        .collect()
}

fn summarize(
    days: &[DailyUsage],
    used_ratio: f64,
    is_current: bool,
) -> CodexUsageStatisticsSummary {
    let tokens = sum_tokens(days.iter().map(|day| day.tokens));
    let mut costs = MoneySum::default();
    for day in days {
        costs.push(day.estimated_cost);
    }
    let estimated_cost = costs.finish();
    let has_unknown_pricing = days.iter().any(|day| day.has_unknown_pricing);
    let projected_tokens = (is_current && used_ratio > EPSILON && tokens.total > 0)
        .then(|| project_u64(tokens.total, used_ratio))
        .flatten();
    let projected_cost = (is_current && !has_unknown_pricing && used_ratio > EPSILON)
        .then(|| estimated_cost.and_then(|cost| project_money(cost, used_ratio)))
        .flatten();
    CodexUsageStatisticsSummary {
        tokens,
        estimated_cost,
        projected_tokens,
        projected_cost,
        day_count: u32::try_from(days.len()).unwrap_or(u32::MAX),
        has_unknown_pricing,
        has_missing_token_data: days.iter().any(|day| day.has_missing_token_data),
    }
}

#[derive(Default)]
struct ModelAggregate {
    credits: f64,
    tokens: CodexUsageStatisticsTokens,
    costs: MoneySum,
    has_unknown_pricing: bool,
    has_estimated_allocation: bool,
    has_rate_fallback: bool,
    has_missing_token_data: bool,
}

fn aggregate_models(
    days: &[DailyUsage],
    used_ratio: f64,
    is_current: bool,
) -> Vec<CodexUsageStatisticsModel> {
    let mut aggregates =
        BTreeMap::<(String, CodexUsageStatisticsServiceTier), ModelAggregate>::new();
    for model in days.iter().flat_map(|day| &day.models) {
        if model.tokens.is_empty() && model.credits <= EPSILON {
            continue;
        }
        let aggregate = aggregates
            .entry((model.model.clone(), model.service_tier))
            .or_default();
        aggregate.credits += model.credits;
        aggregate.tokens =
            aggregate
                .tokens
                .checked_add(model.tokens)
                .unwrap_or(CodexUsageStatisticsTokens {
                    uncached_input: u64::MAX,
                    cached_input: u64::MAX,
                    output: u64::MAX,
                    total: u64::MAX,
                });
        aggregate.costs.push(model.estimated_cost);
        aggregate.has_unknown_pricing |= model.has_unknown_pricing;
        aggregate.has_estimated_allocation |= model.has_estimated_allocation;
        aggregate.has_rate_fallback |= model.has_rate_fallback;
        aggregate.has_missing_token_data |= model.has_missing_token_data;
    }
    let total_credits = aggregates
        .values()
        .map(|aggregate| aggregate.credits)
        .sum::<f64>();
    let mut rows = aggregates
        .into_iter()
        .map(|((model, service_tier), aggregate)| {
            let credit_share =
                (total_credits > EPSILON).then_some(aggregate.credits / total_credits);
            let quota_share = (is_current && used_ratio > EPSILON)
                .then(|| credit_share.map(|share| share * used_ratio))
                .flatten();
            CodexUsageStatisticsModel {
                key: format!("{model}::{}", service_tier.as_str()),
                model,
                service_tier,
                credit_share,
                quota_share,
                tokens: aggregate.tokens,
                estimated_cost: aggregate.costs.finish(),
                has_unknown_pricing: aggregate.has_unknown_pricing,
                has_estimated_allocation: aggregate.has_estimated_allocation,
                has_rate_fallback: aggregate.has_rate_fallback,
                has_missing_token_data: aggregate.has_missing_token_data,
            }
        })
        .collect::<Vec<_>>();
    rows.sort_by(compare_model_rows);
    rows
}

fn compare_model_rows(
    left: &CodexUsageStatisticsModel,
    right: &CodexUsageStatisticsModel,
) -> Ordering {
    right
        .credit_share
        .unwrap_or_default()
        .total_cmp(&left.credit_share.unwrap_or_default())
        .then_with(|| cost_ticks(right.estimated_cost).cmp(&cost_ticks(left.estimated_cost)))
        .then_with(|| right.tokens.total.cmp(&left.tokens.total))
        .then_with(|| left.model.cmp(&right.model))
        .then_with(|| left.service_tier.cmp(&right.service_tier))
}

fn cost_ticks(cost: Option<Money>) -> u128 {
    cost.map(|value| value.amount().scaled())
        .unwrap_or_default()
}

#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn project_u64(value: u64, used_ratio: f64) -> Option<u64> {
    let projected = value as f64 / used_ratio;
    (projected.is_finite() && projected >= 0.0 && projected <= u64::MAX as f64)
        .then(|| projected.round() as u64)
}

#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn project_money(cost: Money, used_ratio: f64) -> Option<Money> {
    let projected = cost.amount().scaled() as f64 / used_ratio;
    if !projected.is_finite() || projected < 0.0 || projected > u128::MAX as f64 {
        return None;
    }
    let amount = Decimal::from_scaled(projected.round() as u128).ok()?;
    Some(Money::new(amount, cost.currency()))
}

fn sum_tokens(
    values: impl IntoIterator<Item = CodexUsageStatisticsTokens>,
) -> CodexUsageStatisticsTokens {
    values
        .into_iter()
        .fold(CodexUsageStatisticsTokens::default(), |total, value| {
            total
                .checked_add(value)
                .unwrap_or(CodexUsageStatisticsTokens {
                    uncached_input: u64::MAX,
                    cached_input: u64::MAX,
                    output: u64::MAX,
                    total: u64::MAX,
                })
        })
}

fn rows_by_date(payload: &Value) -> BTreeMap<NaiveDate, Value> {
    extract_daily_list(payload)
        .into_iter()
        .filter_map(|row| Some((date_value(row.get("date")?)?, row.clone())))
        .collect()
}

fn extract_daily_list(payload: &Value) -> Vec<&Value> {
    if let Some(rows) = payload.as_array() {
        return rows.iter().collect();
    }
    [
        "data",
        "items",
        "results",
        "daily",
        "daily_usage",
        "dailyWorkspaceUsageCounts",
        "daily_workspace_usage_counts",
        "workspace_usage_counts",
    ]
    .into_iter()
    .find_map(|key| payload.get(key).and_then(Value::as_array))
    .map(|rows| rows.iter().collect())
    .unwrap_or_default()
}

fn value_array(value: Option<&Value>) -> &[Value] {
    value.and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

fn token_parts(value: &Value) -> CodexUsageStatisticsTokens {
    let uncached_input = non_negative_u64(value.get("uncached_text_input_tokens"));
    let cached_input = non_negative_u64(value.get("cached_text_input_tokens"));
    let output = non_negative_u64(value.get("text_output_tokens"));
    let derived = uncached_input
        .saturating_add(cached_input)
        .saturating_add(output);
    let reported = non_negative_u64(value.get("text_total_tokens"));
    CodexUsageStatisticsTokens {
        uncached_input,
        cached_input,
        output,
        total: if reported > 0 { reported } else { derived },
    }
}

fn day_credit_weight(value: &Value) -> f64 {
    let model_credits = value_array(value.get("models"))
        .iter()
        .map(|model| non_negative_f64(model.get("credits")))
        .sum::<f64>();
    if model_credits > EPSILON {
        return model_credits;
    }
    value
        .get("product_surface_usage_values")
        .and_then(Value::as_object)
        .map(|values| {
            values
                .values()
                .map(|value| non_negative_f64(Some(value)))
                .sum()
        })
        .unwrap_or_default()
}

fn model_name(value: &Value) -> String {
    ["model", "model_id", "model_name", "name", "id"]
        .into_iter()
        .find_map(|key| value.get(key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown")
        .to_ascii_lowercase()
}

fn model_service_tier(value: Option<&Value>) -> CodexUsageStatisticsServiceTier {
    match value
        .and_then(Value::as_str)
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("fast" | "priority") => CodexUsageStatisticsServiceTier::Fast,
        Some(_) | None => CodexUsageStatisticsServiceTier::Standard,
    }
}

fn date_value(value: &Value) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value.as_str()?.get(..10)?, "%Y-%m-%d").ok()
}

fn non_negative_u64(value: Option<&Value>) -> u64 {
    let Some(value) = value else {
        return 0;
    };
    if let Some(number) = value.as_u64() {
        return number;
    }
    let normalized = value
        .as_str()
        .map(|raw| raw.replace(',', ""))
        .unwrap_or_else(|| value.to_string());
    normalized
        .trim()
        .parse::<u64>()
        .ok()
        .or_else(|| {
            normalized
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|number| number.is_finite() && *number >= 0.0)
                .map(|number| number.round() as u64)
        })
        .unwrap_or_default()
}

fn non_negative_f64(value: Option<&Value>) -> f64 {
    let Some(value) = value else {
        return 0.0;
    };
    let parsed = value.as_f64().or_else(|| {
        value
            .as_str()
            .map(|raw| raw.replace(',', ""))
            .and_then(|raw| raw.trim().parse::<f64>().ok())
    });
    parsed
        .filter(|number| number.is_finite() && *number >= 0.0)
        .unwrap_or_default()
}
