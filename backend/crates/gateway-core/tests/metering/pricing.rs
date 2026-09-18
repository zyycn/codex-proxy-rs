use gateway_core::metering::{ModelPriceOverride, PricingOverrides, TokenPrice, merge_pricing};
use serde_json::json;

#[test]
fn token_price_preserves_zero_and_exact_precision_but_rejects_unrepresentable_prices() {
    for (price, ticks) in [
        ("0", 0),
        ("0.0001", 1),
        ("2.5000", 25_000),
        ("1000000", 10_000_000_000),
    ] {
        let price: TokenPrice = price.to_owned().try_into().unwrap();
        assert_eq!(price.ticks_per_token(), ticks);
    }
    for price in ["-1", "1e3", "NaN", "0.00001", "1000000.0001", "", "+2"] {
        assert!(TokenPrice::try_from(price.to_owned()).is_err(), "{price}");
    }
    assert!(serde_json::from_value::<TokenPrice>(json!(2.5)).is_err());
}

#[test]
fn override_validation_rejects_unsupported_bands_and_excessive_multiplier() {
    let mut price: ModelPriceOverride =
        serde_json::from_value(json!({"multiplierBps": 1_000_001, "bands": {}})).unwrap();
    assert!(price.validate().is_err());
    price.multiplier_bps = 0;
    assert!(price.validate().is_ok());
    price.bands.insert(
        "unknown".to_owned(),
        serde_json::from_value(
            json!({"input":"1", "output":"1", "cacheRead":"0", "cacheWrite":"0"}),
        )
        .unwrap(),
    );
    assert!(price.validate().is_err());
}

#[test]
fn sync_layer_cannot_erase_manual_bands_or_multiplier() {
    let synced: PricingOverrides = serde_json::from_value(json!({"openai":{"model":{
        "multiplierBps":10000, "bands":{"standard":{"input":"2", "output":"10", "cacheRead":"0.2", "cacheWrite":"0"}}
    }}})).unwrap();
    let manual: PricingOverrides = serde_json::from_value(json!({"openai":{"model":{
        "multiplierBps":12345, "bands":{"fast":{"input":"6", "output":"30", "cacheRead":"0.6", "cacheWrite":"0"}}
    }}})).unwrap();
    let effective = merge_pricing(synced.clone(), &manual);
    let model = &effective["openai"]["model"];
    assert_eq!(model.multiplier_bps, 12345);
    assert_eq!(
        model.bands["standard"],
        synced["openai"]["model"].bands["standard"]
    );
    assert_eq!(model.bands["fast"], manual["openai"]["model"].bands["fast"]);
    assert_eq!(manual["openai"]["model"].bands.len(), 1);
}

#[test]
fn custom_multiplier_rounds_components_once_and_snapshot_survives_conversion() {
    use gateway_core::metering::{
        CalculatedCostAmounts, CalculatedCostBreakdown, CalculatedCostRates, CurrencyCode, Decimal,
        Money,
    };
    let money = |ticks| {
        Money::new(
            Decimal::from_scaled(ticks).unwrap(),
            CurrencyCode::new("USD").unwrap(),
        )
    };
    let original = CalculatedCostBreakdown::new(
        CalculatedCostAmounts::new(money(1), money(1), money(1), money(0), money(3), money(3)),
        CalculatedCostRates::new(
            money(1_000_000),
            money(1_000_000),
            money(1_000_000),
            money(0),
        ),
        None,
        100,
    );
    let adjusted = original.clone().with_custom_multiplier(5000).unwrap();
    assert_eq!(adjusted.total_amount().amount().scaled(), 3);
    assert_eq!(adjusted.standard_amount(), adjusted.total_amount());
    let estimate = adjusted.calculated_cost().into_estimate();
    assert_eq!(estimate.breakdown(), Some(&adjusted));
    assert_eq!(estimate.total(), Some(adjusted.total_amount()));
    assert_eq!(
        original
            .clone()
            .with_custom_multiplier(0)
            .unwrap()
            .total_amount()
            .amount()
            .scaled(),
        0
    );
    assert!(original.with_custom_multiplier(1_000_001).is_none());
}
