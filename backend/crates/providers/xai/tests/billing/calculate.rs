//! xAI 缓存、上下文和 Priority 档位的费用合同

use crate::billing::grok_billing_breakdown_with_tier;

#[test]
fn billing_breakdown_should_preserve_token_components() {
    let breakdown = grok_billing_breakdown_with_tier("grok-4.5", 100, 10, 25, None)
        .expect("known Grok pricing");

    assert_eq!(breakdown.input_amount().amount().scaled(), 1_500_000);
    assert_eq!(breakdown.cache_read_amount().amount().scaled(), 75_000);
    assert_eq!(breakdown.output_amount().amount().scaled(), 600_000);
    assert_eq!(breakdown.total_amount().amount().scaled(), 2_175_000);
    assert_eq!(breakdown.service_tier(), Some("default"));
}

#[test]
fn billing_should_price_grok_46_cache_and_priority_at_the_context_boundary() {
    // 短、长上下文分别检查普通输入、缓存、输出的单价，Priority 只采用已确认档位
    for (input, tier, expected) in [
        (199_999, None, ["2", "0.5", "6"]),
        (200_000, None, ["4", "1", "12"]),
        (199_999, Some("priority"), ["4", "1", "12"]),
        (200_000, Some("priority"), ["8", "2", "24"]),
    ] {
        let billing = grok_billing_breakdown_with_tier("grok-4.6", input, 10, 20, tier)
            .expect("Grok 4.6 价格");
        let rates = [
            billing.input_price_per_million(),
            billing.cache_read_price_per_million(),
            billing.output_price_per_million(),
        ]
        .map(|money| {
            money
                .amount()
                .to_string()
                .trim_end_matches('0')
                .trim_end_matches('.')
                .to_owned()
        });
        assert_eq!(rates, expected);
        assert_eq!(billing.long_context_billing_applied(), input >= 200_000);
    }
    assert!(grok_billing_breakdown_with_tier("grok-4.6", 10, 1, 0, Some("future")).is_none());
    let priority = grok_billing_breakdown_with_tier("grok-4.6", 100, 10, 20, Some("priority"))
        .expect("Priority 价格");
    assert_eq!(priority.standard_amount().amount().scaled(), 2_300_000);
    assert_eq!(priority.total_amount().amount().scaled(), 4_600_000);
    assert_eq!(priority.multiplier_percent(), 200);
}
