//! 验证账号额度预测与原生模型目录的展示投影

use bytes::Bytes;
use gateway_admin::model::{
    provider_credentials::{AccountUsagePeriod, ProviderModelCatalogDocument},
    quota_forecast::{AccountQuotaForecast, AccountQuotaForecastReport, QuotaForecastSource},
};
use gateway_api::admin::accounts::{
    AccountModelCatalogData, AccountQuotaForecastData, UnsupportedModelCatalogDocument,
};
use gateway_core::operation::RawJsonPayload;

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
    };
    let mut monthly = forecast.clone();
    monthly.period = AccountUsagePeriod::Monthly;
    monthly.extrapolated = true;
    monthly.target_seconds = 30 * 86_400;
    monthly.estimated_tokens = Some(0);
    monthly.estimated_usd = Some(0.0);
    let view = AccountQuotaForecastData::from((
        AccountQuotaForecastReport {
            account_id: "acct_forecast".to_owned(),
            generated_at: now,
            forecasts: [forecast, monthly],
        },
        gateway_api::TimePresenter::new(Default::default()),
    ));
    let value = serde_json::to_value(view).unwrap();
    assert_eq!(value["accountId"], "acct_forecast");
    assert_eq!(value["generatedAt"], "2026-09-12T08:00:00+08:00");
    let week = &value["forecasts"][0];
    assert_eq!(week["estimatedTokens"], 1_000_000);
    assert_eq!(week["estimatedTokensDisplay"], "1M");
    assert!(week["estimatedUsd"].is_null());
    assert_eq!(week["estimatedUsdDisplay"], "—");
    for forecast in value["forecasts"].as_array().unwrap() {
        for field in [
            "remainingTokens",
            "remainingTokensDisplay",
            "remainingUsd",
            "remainingUsdDisplay",
        ] {
            assert!(forecast.get(field).is_none(), "unexpected field: {field}");
        }
    }
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
    assert_eq!(value["forecasts"][1]["estimatedTokens"], 0);
    assert_eq!(value["forecasts"][1]["estimatedTokensDisplay"], "0");
    assert_eq!(value["forecasts"][1]["estimatedUsd"], 0.0);
    assert_eq!(value["forecasts"][1]["estimatedUsdDisplay"], "$0.00");
    assert_eq!(value["forecasts"][1]["period"], "monthly");
    assert_eq!(value["forecasts"][1]["targetDays"], 30.0);
    assert_eq!(value["forecasts"][1]["extrapolated"], true);
    assert!(value.get("account").is_none());
}

#[test]
fn model_catalog_projection_keeps_upstream_document_and_rejects_non_codex_wire() {
    let observed_at = "2026-09-12T08:00:00Z".parse().unwrap();
    // 上游原生对象里的元数据必须原样到达客户端文件，否则 Codex 读不到推理强度和上下文窗口
    let body = serde_json::json!({
        "models": [{
            "slug": "gpt-5.6-luna",
            "display_name": "Luna",
            "context_window": 272_000,
            "supported_reasoning_levels": [{ "effort": "high" }],
        }],
    });
    let document = RawJsonPayload::new("codex", Bytes::from(serde_json::to_vec(&body).unwrap()))
        .expect("codex payload");
    let data = AccountModelCatalogData::try_from(ProviderModelCatalogDocument {
        document,
        model_count: 1,
        observed_at,
    })
    .expect("codex catalog is projectable");
    assert_eq!(data.model_count, 1);
    assert_eq!(data.catalog, body);
    assert_eq!(data.observed_at, "2026-09-12T08:00:00+00:00");

    // 只有模型 ID 的 API 目录拼不出合法的 model_catalog_json，不能降格返回给客户端
    let adapted = RawJsonPayload::new("openai", Bytes::from_static(br#"{"models":[]}"#))
        .expect("openai payload");
    assert_eq!(
        AccountModelCatalogData::try_from(ProviderModelCatalogDocument {
            document: adapted,
            model_count: 0,
            observed_at,
        })
        .err(),
        Some(UnsupportedModelCatalogDocument)
    );
}
