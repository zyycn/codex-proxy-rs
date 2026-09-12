use chrono::{DateTime, Duration, Utc};
use gateway_admin::model::{
    accounts::{AccountCost, AccountUsage},
    observability::CostCoverage,
    provider_credentials::{ProviderQuota, ProviderQuotaWindow, QuotaLocalUsageAttribution},
    quota_forecast::{AccountQuotaForecast, account_quota_forecasts},
    quota_forecast_sampling::{QuotaForecastMethod, QuotaForecastSample, QuotaForecastUsage},
};

fn now() -> DateTime<Utc> {
    "2026-09-12T00:00:00Z".parse().unwrap()
}

fn window(key: &str, days: u64) -> ProviderQuotaWindow {
    ProviderQuotaWindow {
        key: key.to_owned(),
        group: if days > 7 { "monthly" } else { "shortTerm" }.to_owned(),
        label: key.to_owned(),
        limit_id: None,
        limit_name: None,
        role: None,
        local_usage_attribution: QuotaLocalUsageAttribution::AccountWide,
        window_seconds: Some(days * 86_400),
        used_percent: Some(20.0),
        reset_at: Some(now() + Duration::days(1)),
        limit_reached: false,
        local_usage: Some(AccountUsage {
            account_id: "acct_forecast".to_owned(),
            request_count: 10,
            success_count: 10,
            input_tokens: Some(900),
            output_tokens: Some(100),
            cached_tokens: Some(800),
            cache_write_tokens: None,
            reasoning_tokens: None,
            image_input_tokens: None,
            image_output_tokens: None,
            image_request_count: 0,
            image_request_failed_count: 0,
            total_tokens: Some(1_000),
            cost_coverage: CostCoverage {
                calculated_count: 10,
                ..Default::default()
            },
            costs: vec![AccountCost {
                currency: "USD".to_owned(),
                amount: "2".parse().unwrap(),
            }],
            last_used_at: Some(now()),
            request_buckets: Vec::new(),
            models: Vec::new(),
        }),
        provider_data: None,
    }
}

fn quota(windows: Vec<ProviderQuotaWindow>) -> ProviderQuota {
    ProviderQuota {
        plan_type: None,
        observed_at: Some(now()),
        refresh_token_expires_at: None,
        windows,
        limit_reached: false,
        provider_data: None,
    }
}

fn forecast(quota: &ProviderQuota) -> [AccountQuotaForecast; 2] {
    account_quota_forecasts(quota, now() - Duration::days(60), now(), &samples(quota))
}

fn samples(quota: &ProviderQuota) -> Vec<QuotaForecastSample> {
    quota
        .windows
        .iter()
        .filter_map(|window| {
            let usage = window.local_usage.as_ref()?;
            let usd = usage.costs.iter().find(|cost| cost.currency == "USD");
            Some(QuotaForecastSample {
                key: window.key.clone(),
                method: QuotaForecastMethod::Cumulative,
                start_at: window.reset_at?.checked_sub_signed(Duration::try_seconds(
                    i64::try_from(window.window_seconds?).ok()?,
                )?)?,
                end_at: quota.observed_at.unwrap_or(now()),
                baseline_percent: 0.0,
                sampled_percent: window.used_percent.unwrap_or(0.0),
                block_count: 0,
                observation_count: 0,
                pending_request_count: 0,
                discontinuous: false,
                usage: QuotaForecastUsage {
                    request_count: usage.request_count,
                    tokens: usage.total_tokens.unwrap_or(0),
                    input_tokens: usage.input_tokens.unwrap_or(0),
                    output_tokens: usage.output_tokens.unwrap_or(0),
                    cached_tokens: usage.cached_tokens.unwrap_or(0),
                    missing_token_count: u64::from(usage.total_tokens.is_none()),
                    known_cost_count: if usd.is_some() {
                        usage.cost_coverage.known_count()
                    } else {
                        0
                    },
                    unavailable_cost_count: usage.cost_coverage.partial_count
                        + usage.cost_coverage.unavailable_count,
                    usd: usd.map_or(0.0, |cost| cost.amount.as_str().parse().unwrap()),
                    excluded_request_count: 0,
                },
            })
        })
        .collect()
}

#[test]
fn forecasts_use_actual_periods_and_do_not_extrapolate_remaining_capacity() {
    let [week, month] = forecast(&quota(vec![window("week", 7)]));
    assert_eq!(week.estimated_tokens, Some(5_000));
    assert_eq!(week.estimated_usd, Some(10.0));
    assert_eq!(week.remaining_tokens, Some(4_000));
    assert_eq!(week.remaining_usd, Some(8.0));
    assert!(!week.extrapolated);
    assert!(month.extrapolated);
    assert_eq!(month.estimated_tokens, Some(21_429));
    assert!((month.estimated_usd.unwrap() - 10.0 * 30.0 / 7.0).abs() < 1e-10);
    assert_eq!(month.remaining_tokens, week.remaining_tokens);
    assert_eq!(month.remaining_usd, week.remaining_usd);
}

#[test]
fn corresponding_actual_window_takes_priority_and_retains_its_duration() {
    let [week, month] = forecast(&quota(vec![window("month", 31), window("week", 7)]));
    assert_eq!(week.source.unwrap().label, "week");
    assert_eq!(month.source.unwrap().label, "month");
    assert!(!month.extrapolated);
    assert_eq!(month.target_seconds, 31 * 86_400);
    assert_eq!(month.estimated_tokens, Some(5_000));
    let [week, _] = forecast(&quota(vec![window("month", 31)]));
    assert!(week.extrapolated);
    assert_eq!(week.estimated_tokens, Some(1_129));
}

#[test]
fn short_or_model_specific_windows_are_not_account_capacity() {
    let mut model = window("model", 7);
    model.local_usage_attribution = QuotaLocalUsageAttribution::Unavailable;
    for item in forecast(&quota(vec![window("day", 1), model])) {
        assert!(item.source.is_none());
        assert!(item.unavailable_reason.is_some());
        assert!(item.estimated_tokens.is_none());
    }
}

#[test]
fn invalid_or_tiny_percentages_cannot_produce_estimates() {
    for percent in [
        None,
        Some(f64::NAN),
        Some(f64::INFINITY),
        Some(-1.0),
        Some(101.0),
        Some(0.0),
        Some(0.5),
        Some(1.0),
        Some(4.9),
    ] {
        let mut sample = window("week", 7);
        sample.used_percent = percent;
        let [week, _] = forecast(&quota(vec![sample]));
        assert!(week.unavailable_reason.is_some(), "{percent:?}");
        assert!(week.estimated_tokens.is_none());
        assert!(week.estimated_usd.is_none());
    }
}

#[test]
fn low_samples_warn_and_exhaustion_uses_raw_not_display_percentages() {
    let mut sample = window("week", 7);
    sample.used_percent = Some(5.0);
    sample.limit_reached = true;
    let mut source = quota(vec![sample]);
    source.limit_reached = true;
    let [week, _] = forecast(&source);
    assert!(week.low_sample);
    assert_eq!(week.estimated_tokens, Some(20_000));
    source.windows[0].used_percent = Some(100.0);
    let [week, _] = forecast(&source);
    assert!(!week.low_sample);
    assert_eq!(week.remaining_tokens, Some(0));
    assert_eq!(week.remaining_usd, Some(0.0));
}

#[test]
fn stale_future_or_missing_snapshots_do_not_predict() {
    for observed in [
        None,
        Some(now() - Duration::days(8)),
        Some(now() + Duration::seconds(1)),
    ] {
        let mut source = quota(vec![window("week", 7)]);
        source.observed_at = observed;
        assert!(forecast(&source)[0].unavailable_reason.is_some());
    }
    let mut sample = window("week", 7);
    sample.reset_at = Some(now());
    assert!(
        forecast(&quota(vec![sample]))[0]
            .unavailable_reason
            .is_some()
    );
}

#[test]
fn partial_account_history_or_missing_usage_keep_diagnostics_without_estimates() {
    let source = quota(vec![window("week", 7)]);
    let [week, _] =
        account_quota_forecasts(&source, now() - Duration::days(1), now(), &samples(&source));
    assert!(week.unavailable_reason.unwrap().contains("记录不完整"));
    assert!(week.estimated_tokens.is_none());
    let mut sample = window("week", 7);
    sample.local_usage = None;
    let [week, _] = forecast(&quota(vec![sample]));
    assert!(week.source.is_some());
    assert!(
        week.unavailable_reason
            .unwrap()
            .contains("没有网关用量记录")
    );
}

#[test]
fn incremental_sample_supports_mid_cycle_accounts_and_remaining_uses_current_percent() {
    let source = quota(vec![window("week", 7)]);
    let mut sample = samples(&source).remove(0);
    sample.method = QuotaForecastMethod::Incremental;
    sample.start_at = now() - Duration::hours(6);
    sample.baseline_percent = 10.0;
    sample.sampled_percent = 10.0;
    sample.block_count = 2;
    let [week, month] =
        account_quota_forecasts(&source, now() - Duration::days(1), now(), &[sample]);
    assert_eq!(week.estimated_tokens, Some(10_000));
    assert_eq!(week.remaining_tokens, Some(8_000));
    assert_eq!(month.remaining_tokens, Some(8_000));
    assert!(week.unavailable_reason.is_none());
    assert!(!week.low_sample);
}

#[test]
fn missing_tokens_suppress_token_estimates_without_discarding_complete_costs() {
    let source = quota(vec![window("week", 7)]);
    let mut sample = samples(&source).remove(0);
    sample.usage.missing_token_count = 1;
    let [week, _] = account_quota_forecasts(&source, now() - Duration::days(60), now(), &[sample]);
    assert!(week.incomplete_tokens);
    assert_eq!(week.estimated_tokens, None);
    assert_eq!(week.remaining_tokens, None);
    assert_eq!(week.estimated_usd, Some(10.0));
    assert_eq!(week.source.unwrap().tokens, Some(1_000));
}

#[test]
fn discontinuous_samples_never_fall_back_to_cumulative_predictions() {
    let source = quota(vec![window("week", 7)]);
    let mut sample = samples(&source).remove(0);
    sample.discontinuous = true;
    let [week, _] = account_quota_forecasts(&source, now() - Duration::days(60), now(), &[sample]);
    assert!(week.unavailable_reason.unwrap().contains("不连续"));
    assert!(week.estimated_tokens.is_none());
}

#[test]
fn partial_or_unknown_costs_suppress_only_money_estimates() {
    for coverage in [
        CostCoverage {
            calculated_count: 9,
            partial_count: 1,
            ..Default::default()
        },
        CostCoverage {
            calculated_count: 9,
            unavailable_count: 1,
            ..Default::default()
        },
        CostCoverage::default(),
    ] {
        let mut sample = window("week", 7);
        sample.local_usage.as_mut().unwrap().cost_coverage = coverage;
        let [week, _] = forecast(&quota(vec![sample]));
        assert!(week.incomplete_cost);
        assert!(week.unavailable_reason.is_none());
        assert!(week.estimated_usd.is_none());
        assert!(week.remaining_usd.is_none());
        assert_eq!(week.estimated_tokens, Some(5_000));
    }
}

#[test]
fn zero_known_cost_is_valid_but_missing_usd_is_not_invented() {
    let mut sample = window("week", 7);
    sample.local_usage.as_mut().unwrap().costs[0].amount = "0".parse().unwrap();
    assert_eq!(
        forecast(&quota(vec![sample.clone()]))[0].estimated_usd,
        Some(0.0)
    );
    sample.local_usage.as_mut().unwrap().costs[0].currency = "EUR".to_owned();
    assert_eq!(forecast(&quota(vec![sample]))[0].estimated_usd, None);
}

#[test]
fn oversized_window_or_estimate_is_unavailable_without_panics_or_saturation() {
    let mut sample = window("month", 30);
    sample.window_seconds = Some(i64::MAX as u64);
    assert!(
        forecast(&quota(vec![sample]))[1]
            .unavailable_reason
            .is_some()
    );
    let mut sample = window("week", 7);
    sample.local_usage.as_mut().unwrap().total_tokens = Some(u64::MAX);
    let [week, _] = forecast(&quota(vec![sample]));
    assert!(week.estimated_tokens.is_none());
    assert_eq!(week.estimated_usd, Some(10.0));
}
