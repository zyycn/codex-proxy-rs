//! 人工单价、缓存子集、思考输出及金额精度的计费合同

use gateway_core::metering::{ModelPriceOverride, Usage};
use serde_json::json;

pub(crate) fn pricing() -> ModelPriceOverride {
    serde_json::from_value(json!({
        "multiplierBps": 15000,
        "bands": {"standard": {
            "input": "2", "output": "8", "cacheRead": "0.5", "cacheWrite": "3"
        }}
    }))
    .unwrap()
}

#[test]
fn cached_input_and_thinking_output_are_billed_once_with_the_frozen_multiplier() {
    let usage = Usage {
        input_tokens: Some(100),
        output_tokens: Some(40),
        cached_input_tokens: Some(20),
        cache_write_input_tokens: Some(10),
        reasoning_output_tokens: Some(30),
        ..Usage::new()
    };
    let cost = crate::billing::calculate("unknown-model", &usage, Some(&pricing())).unwrap();
    assert_eq!(
        [
            cost.input_amount().amount().canonical(),
            cost.output_amount().amount().canonical(),
            cost.cache_read_amount().amount().canonical(),
            cost.cache_write_amount().amount().canonical(),
            cost.standard_amount().amount().canonical(),
            cost.total_amount().amount().canonical(),
        ],
        [
            "0.00021", "0.00048", "0.000015", "0.000045", "0.00075", "0.00075"
        ]
    );
    assert_eq!(cost.custom_multiplier_bps(), 15000);
}

#[test]
fn absent_prices_or_usage_do_not_fabricate_free_requests() {
    let usage = Usage {
        input_tokens: Some(100),
        output_tokens: Some(40),
        ..Usage::new()
    };
    assert!(crate::billing::calculate("unknown-model", &usage, None).is_none());
    let mut multiplier_only = pricing();
    multiplier_only.bands.clear();
    assert!(crate::billing::calculate("unknown-model", &usage, Some(&multiplier_only)).is_none());
    assert!(crate::billing::calculate("unknown-model", &Usage::new(), Some(&pricing())).is_none());
}

#[test]
fn invalid_cache_subsets_and_out_of_range_amounts_are_not_billed() {
    let usage = Usage {
        input_tokens: Some(5),
        output_tokens: Some(1),
        cached_input_tokens: Some(4),
        cache_write_input_tokens: Some(2),
        ..Usage::new()
    };
    assert!(crate::billing::calculate("unknown-model", &usage, Some(&pricing())).is_none());
    let usage = Usage {
        input_tokens: Some(u64::MAX),
        output_tokens: Some(u64::MAX),
        ..Usage::new()
    };
    assert!(crate::billing::calculate("unknown-model", &usage, Some(&pricing())).is_none());
}

#[test]
fn explicit_zero_multiplier_remains_an_observed_zero_cost() {
    let mut pricing = pricing();
    pricing.multiplier_bps = 0;
    let usage = Usage {
        input_tokens: Some(1),
        output_tokens: Some(1),
        ..Usage::new()
    };
    let cost = crate::billing::calculate("unknown-model", &usage, Some(&pricing)).unwrap();
    assert_eq!(cost.total_amount().amount().canonical(), "0");
}

#[test]
fn builtin_prices_cover_public_models_and_reference_mappings_without_prefix_guessing() {
    let defaults = crate::billing::defaults();
    assert_eq!(defaults.len(), 32);
    assert_eq!(crate::billing::source_models().len(), defaults.len());
    for (model, price) in defaults {
        price.validate().unwrap();
        assert_eq!(price.multiplier_bps, 10_000);
        let mut usage = Usage {
            input_tokens: Some(1_000_000),
            output_tokens: Some(1_000_000),
            image_output_tokens: Some(0),
            ..Usage::new()
        };
        if model == "gemini-2.5-pro" {
            usage.input_tokens = Some(100_000);
        }
        assert!(
            crate::billing::calculate(model, &usage, None).is_some(),
            "{model}"
        );
    }
    for model in [
        "tab_flash_lite_preview",
        "gemini-4-hydrogen-sonar",
        "gemini-4-hydrogen-tiered-high",
        "gemini-3.8-flash-tiered",
        "claude-opus-5-5-unknown",
        "gemini-4-neon-future",
    ] {
        assert!(!defaults.contains_key(model));
        assert!(!crate::billing::source_models().contains_key(model));
    }
    assert_eq!(
        crate::billing::source_models()["gemini-3-flash"].model,
        "gemini-3-flash-preview"
    );
    assert_eq!(
        crate::billing::source_models()["gpt-oss-120b-medium"].provider,
        "google-vertex"
    );
}

#[test]
fn builtin_prices_are_used_without_manual_configuration_and_preserve_multiplier_only_overrides() {
    let usage = Usage {
        input_tokens: Some(100),
        output_tokens: Some(40),
        cached_input_tokens: Some(20),
        reasoning_output_tokens: Some(30),
        ..Usage::new()
    };
    let cost = crate::billing::calculate("gemini-2.5-flash", &usage, None).unwrap();
    assert_eq!(cost.total_amount().amount().canonical(), "0.0001246");
    let override_price = ModelPriceOverride {
        multiplier_bps: 20_000,
        bands: Default::default(),
    };
    let frozen =
        crate::billing::effective_pricing("gemini-2.5-flash", Some(&override_price)).unwrap();
    assert_eq!(
        crate::billing::calculate("gemini-2.5-flash", &usage, Some(&frozen))
            .unwrap()
            .total_amount()
            .amount()
            .canonical(),
        "0.0002492"
    );
}

#[test]
fn gemini_pro_switches_all_rates_only_above_200k_input_tokens() {
    for (input, long, total) in [(200_000, false, "0.25"), (200_001, true, "0.5000025")] {
        let usage = Usage {
            input_tokens: Some(input),
            output_tokens: Some(0),
            ..Usage::new()
        };
        let cost = crate::billing::calculate("gemini-2.5-pro", &usage, None).unwrap();
        assert_eq!(cost.long_context_billing_applied(), long);
        assert_eq!(cost.total_amount().amount().canonical(), total);
    }
}

#[test]
fn image_output_and_thinking_use_distinct_prices_and_require_modality_counts() {
    let mut usage = Usage {
        input_tokens: Some(10),
        output_tokens: Some(100),
        image_output_tokens: Some(80),
        reasoning_output_tokens: Some(20),
        ..Usage::new()
    };
    let cost = crate::billing::calculate("gemini-3.1-flash-image", &usage, None).unwrap();
    assert_eq!(cost.total_amount().amount().canonical(), "0.004865");
    assert_eq!(cost.output_price_per_million().amount().canonical(), "48.6");
    usage.image_output_tokens = None;
    assert!(crate::billing::calculate("gemini-3.1-flash-image", &usage, None).is_none());
    usage.image_output_tokens = Some(101);
    assert!(crate::billing::calculate("gemini-3.1-flash-image", &usage, None).is_none());
}

#[test]
fn custom_image_input_price_respects_image_breakdown_and_ambiguous_cached_subsets() {
    let mut price = crate::billing::defaults()["gemini-3.1-flash-image"].clone();
    price.bands.get_mut("image").unwrap().input = "1".to_owned().try_into().unwrap();
    let mut usage = Usage {
        input_tokens: Some(10),
        output_tokens: Some(0),
        image_input_tokens: Some(4),
        image_output_tokens: Some(0),
        ..Usage::new()
    };
    let cost = crate::billing::calculate("gemini-3.1-flash-image", &usage, Some(&price)).unwrap();
    assert_eq!(cost.total_amount().amount().canonical(), "0.000007");
    usage.cached_input_tokens = Some(2);
    assert!(crate::billing::calculate("gemini-3.1-flash-image", &usage, Some(&price)).is_none());
}

#[test]
fn audio_and_video_requests_or_observed_audio_are_not_priced_as_text() {
    for value in [
        json!({"contents":[{"parts":[{"inlineData":{"mimeType":"audio/wav"}}]}]}),
        json!({"contents":[{"parts":[{"fileData":{"mimeType":"video/mp4"}}]}]}),
        json!({"usageMetadata":{"promptTokensDetails":[{"modality":"AUDIO","tokenCount":2}]}}),
    ] {
        assert!(!crate::billing::supports_pricing(&value));
    }
    assert!(crate::billing::supports_pricing(
        &json!({"contents":[{"parts":[{"text":"hello"},{"inlineData":{"mimeType":"image/png"}}]}]})
    ));
}
