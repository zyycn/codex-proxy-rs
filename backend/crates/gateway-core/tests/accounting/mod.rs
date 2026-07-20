use std::str::FromStr;

use gateway_core::accounting::{
    CalculatedCost, CalculatedCostAmounts, CalculatedCostBreakdown, CalculatedCostRates,
    CostEstimate, CostEstimateStatus, CostSource, CurrencyCode, Decimal, Money,
    ProviderReportedCost, Usage,
};

#[test]
fn decimal_should_render_with_database_scale() {
    let decimal = Decimal::from_str("12.34").expect("valid decimal");
    assert_eq!(decimal.to_string(), "12.3400000000");
}

#[test]
fn provider_usd_ticks_should_map_exactly_to_decimal_scale() {
    let estimate = ProviderReportedCost::from_usd_ticks(12_345_678_901)
        .expect("valid provider ticks")
        .into_estimate();
    assert_eq!(estimate.status(), CostEstimateStatus::Known);
    assert_eq!(estimate.source(), CostSource::ProviderReported);
    let total = estimate.total().expect("provider total");
    assert_eq!(total.amount().scaled(), 12_345_678_901);
    assert_eq!(total.currency().as_str(), "USD");
}

#[test]
fn calculated_usd_ticks_should_be_marked_as_calculated() {
    let estimate = CalculatedCost::from_usd_ticks(12_345)
        .expect("valid calculated cost")
        .into_estimate();

    assert_eq!(estimate.source(), CostSource::Calculated);
    assert_eq!(
        estimate.total().map(|total| total.amount().scaled()),
        Some(12_345)
    );
}

#[test]
fn calculated_breakdown_should_preserve_runtime_components_without_changing_total_contract() {
    let usd = CurrencyCode::new("USD").expect("USD currency");
    let money = |ticks| Money::new(Decimal::from_scaled(ticks).expect("valid ticks"), usd);
    let breakdown = CalculatedCostBreakdown::new(
        CalculatedCostAmounts::new(
            money(100),
            money(200),
            money(30),
            money(0),
            money(330),
            money(330),
        ),
        CalculatedCostRates::new(
            money(10_000_000_000),
            money(20_000_000_000),
            money(3_000_000_000),
            money(0),
        ),
        Some("default".to_owned()),
        100,
    );

    assert_eq!(breakdown.input_amount().amount().scaled(), 100);
    assert_eq!(breakdown.standard_amount(), breakdown.total_amount());
    assert_eq!(breakdown.service_tier(), Some("default"));
    assert_eq!(breakdown.multiplier_percent(), 100);
    assert_eq!(breakdown.calculated_cost().total().amount().scaled(), 330);
}

#[test]
fn decimal_should_reject_more_than_ten_fraction_digits() {
    assert!(Decimal::from_str("1.00000000001").is_err());
}

#[test]
fn unknown_cost_should_not_fabricate_zero() {
    assert_eq!(CostEstimate::unavailable().total(), None);
}

#[test]
fn provider_reported_cost_should_preserve_exact_total() {
    let total = Money::new(
        Decimal::from_str("0.00125").expect("valid amount"),
        CurrencyCode::new("USD").expect("valid currency"),
    );
    let estimate = CostEstimate::priced(
        CostEstimateStatus::Known,
        CostSource::ProviderReported,
        total,
    )
    .expect("reported total is valid");

    assert_eq!(estimate.total(), Some(total));
}

#[test]
fn unavailable_source_should_reject_amount() {
    let total = Money::new(
        Decimal::ZERO,
        CurrencyCode::new("USD").expect("valid currency"),
    );
    assert!(
        CostEstimate::priced(CostEstimateStatus::Known, CostSource::Unavailable, total).is_err()
    );
}

#[test]
fn usage_total_should_remain_independent() {
    let usage = Usage {
        input_tokens: Some(10),
        output_tokens: Some(5),
        total_tokens: Some(99),
        ..Usage::new()
    };

    assert_eq!(usage.total_tokens, Some(99));
}

#[test]
fn usage_merge_should_only_replace_observed_fields() {
    let mut usage = Usage {
        input_tokens: Some(10),
        ..Usage::new()
    };
    usage.merge(&Usage {
        output_tokens: Some(5),
        ..Usage::new()
    });

    assert_eq!(
        (usage.input_tokens, usage.output_tokens),
        (Some(10), Some(5))
    );
}
