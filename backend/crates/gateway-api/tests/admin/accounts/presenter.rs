use gateway_admin::model::{
    provider_credentials::AccountUsagePeriod,
    quota_forecast::{AccountQuotaForecast, AccountQuotaForecastReport, QuotaForecastSource},
};
use gateway_api::admin::accounts::AccountQuotaForecastData;

#[test]
fn quota_forecast_projection_only_exposes_capacity_and_preserves_null_zero() {
    let now = "2026-09-12T00:00:00Z".parse().unwrap();
    let forecast = AccountQuotaForecast {
        period: AccountUsagePeriod::Weekly,
        target_seconds: 7 * 86_400,
        extrapolated: false,
        source: Some(QuotaForecastSource {
            label: "周额度".to_owned(),
            used_percent: Some(100.0),
            observed_at: Some(now),
            reset_at: now,
            tokens: Some(1_000_000),
            usd: Some(0.1234),
        }),
        unavailable_reason: None,
        low_sample: false,
        incomplete_cost: true,
        incomplete_tokens: false,
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
    assert_eq!(
        week["source"],
        serde_json::json!({
            "label": "周额度",
            "usedPercent": 100.0,
            "usedPercentDisplay": "100.0%",
            "observedAt": "2026-09-12T08:00:00+08:00",
            "observedAtDisplay": "2026-09-12 08:00:00",
            "resetAt": "2026-09-12T08:00:00+08:00",
            "tokensDisplay": "1M",
            "usdDisplay": "$0.1234"
        })
    );
    assert!(week.get("method").is_none());
    assert!(week.get("methodDisplay").is_none());
    assert!(value.get("generatedAtDisplay").is_none());
    assert_eq!(value["forecasts"][1]["period"], "monthly");
    assert_eq!(value["forecasts"][1]["targetDays"], 30.0);
    assert_eq!(value["forecasts"][1]["extrapolated"], true);
    assert!(value.get("account").is_none());
}
