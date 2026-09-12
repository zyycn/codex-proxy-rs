use gateway_admin::model::{
    provider_credentials::AccountUsagePeriod,
    quota_forecast::{AccountQuotaForecast, AccountQuotaForecastReport, QuotaForecastSource},
    quota_forecast_sampling::QuotaForecastMethod,
};
use gateway_api::admin::accounts::AccountQuotaForecastData;

#[test]
fn quota_forecast_projection_preserves_null_zero_and_sample_metadata() {
    let now = "2026-09-12T00:00:00Z".parse().unwrap();
    let forecast = AccountQuotaForecast {
        period: AccountUsagePeriod::Weekly,
        target_seconds: 7 * 86_400,
        extrapolated: false,
        source: Some(QuotaForecastSource {
            key: "week".to_owned(),
            label: "周额度".to_owned(),
            window_seconds: 7 * 86_400,
            used_percent: Some(100.0),
            observed_at: Some(now),
            start_at: Some(now),
            reset_at: now,
            request_count: 1_024,
            tokens: Some(1_000_000),
            input_tokens: Some(900_000),
            output_tokens: Some(100_000),
            cached_tokens: Some(800_000),
            known_cost_count: 1_000,
            partial_cost_count: 24,
            unavailable_cost_count: 0,
            usd: Some(0.1234),
            sample_start_at: Some(now),
            baseline_percent: 85.0,
            sampled_percent: Some(15.0),
            block_count: 3,
            observation_count: 96,
            missing_token_count: 0,
            excluded_request_count: 4,
            pending_request_count: 2,
        }),
        unavailable_reason: None,
        low_sample: false,
        incomplete_cost: true,
        incomplete_tokens: false,
        method: QuotaForecastMethod::Incremental,
        estimated_tokens: Some(1_000_000),
        estimated_usd: None,
        remaining_tokens: Some(0),
        remaining_usd: None,
    };
    let mut monthly = forecast.clone();
    monthly.period = AccountUsagePeriod::Monthly;
    monthly.extrapolated = true;
    monthly.target_seconds = 30 * 86_400;
    let view = AccountQuotaForecastData::from(AccountQuotaForecastReport {
        account_id: "acct_forecast".to_owned(),
        generated_at: now,
        forecasts: [forecast, monthly],
    });
    let value = serde_json::to_value(view).unwrap();
    assert_eq!(value["accountId"], "acct_forecast");
    assert_eq!(value["generatedAt"], "2026-09-12T08:00:00+08:00");
    let week = &value["forecasts"][0];
    assert_eq!(week["estimatedTokens"], 1_000_000);
    assert_eq!(week["estimatedTokensDisplay"], "1M");
    assert!(week["estimatedUsd"].is_null());
    assert_eq!(week["estimatedUsdDisplay"], "—");
    assert_eq!(week["remainingTokensDisplay"], "0");
    assert_eq!(week["source"]["requestCountDisplay"], "1,024");
    assert_eq!(week["source"]["usdDisplay"], "$0.1234");
    assert_eq!(week["source"]["partialCostCount"], 24);
    assert_eq!(week["method"], "incremental");
    assert_eq!(week["source"]["sampledPercentDisplay"], "15.0 个百分点");
    assert_eq!(week["source"]["pendingRequestCount"], 2);
    assert!(week["source"].get("providerData").is_none());
    assert_eq!(value["forecasts"][1]["period"], "monthly");
    assert_eq!(value["forecasts"][1]["targetDays"], 30.0);
    assert_eq!(value["forecasts"][1]["extrapolated"], true);
    assert!(value.get("account").is_none());
}
