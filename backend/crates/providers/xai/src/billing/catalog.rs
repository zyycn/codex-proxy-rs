//! xAI 内置模型价目、上下文档位与管理价目投影

pub(crate) const SUPPORTED_PRICING_BANDS: &[&str] =
    &["standard", "fast", "long_standard", "long_fast"];

use gateway_core::metering::Decimal;

#[derive(Clone, Copy)]
pub(super) struct TokenRates {
    pub(super) input_ticks: u128,
    pub(super) cached_input_ticks: u128,
    pub(super) output_ticks: u128,
}

#[derive(Clone, Copy)]
pub(super) struct ModelPricing {
    pub(super) short: TokenRates,
    pub(super) long: TokenRates,
}

// 价格来源：https://docs.x.ai/developers/pricing，核验日期 2026-09-09
const GROK_46_PRICING: ModelPricing = ModelPricing {
    short: TokenRates {
        input_ticks: 20_000,
        cached_input_ticks: 5_000,
        output_ticks: 60_000,
    },
    long: TokenRates {
        input_ticks: 40_000,
        cached_input_ticks: 10_000,
        output_ticks: 120_000,
    },
};

const GROK_45_PRICING: ModelPricing = ModelPricing {
    short: TokenRates {
        input_ticks: 20_000,
        cached_input_ticks: 3_000,
        output_ticks: 60_000,
    },
    long: TokenRates {
        input_ticks: 40_000,
        cached_input_ticks: 6_000,
        output_ticks: 120_000,
    },
};

const GROK_BUILD_PRICING: ModelPricing = ModelPricing {
    short: TokenRates {
        input_ticks: 10_000,
        cached_input_ticks: 2_000,
        output_ticks: 20_000,
    },
    long: TokenRates {
        input_ticks: 20_000,
        cached_input_ticks: 4_000,
        output_ticks: 40_000,
    },
};

const GROK_43_PRICING: ModelPricing = ModelPricing {
    short: TokenRates {
        input_ticks: 12_500,
        cached_input_ticks: 2_000,
        output_ticks: 25_000,
    },
    long: TokenRates {
        input_ticks: 25_000,
        cached_input_ticks: 4_000,
        output_ticks: 50_000,
    },
};

const PRICING_RULES: &[(&[&str], ModelPricing)] = &[
    (&["grok-4.6"], GROK_46_PRICING),
    (&["grok-4.5", "grok-4.5-build-free"], GROK_45_PRICING),
    (
        &[
            "grok-build-0.1",
            "grok-code-fast-1",
            "grok-code-fast-1-0825",
        ],
        GROK_BUILD_PRICING,
    ),
    (
        &[
            "grok-4.3",
            "grok-4.20-multi-agent-0309",
            "grok-4.20-multi-agent",
            "grok-4.20-multi-agent-experimental-beta-0304",
            "grok-4.20-multi-agent-beta-0309",
            "grok-4.20-0309-reasoning",
            "grok-4.20",
            "grok-4.20-reasoning",
            "grok-4.20-0309",
            "grok-4.20-beta-0309-reasoning",
            "grok-4.20-beta-0309",
            "grok-4.20-experimental-beta-0304-reasoning",
            "grok-4.20-experimental-beta-0304",
            "grok-4.20-0309-non-reasoning",
            "grok-4.20-non-reasoning",
            "grok-4.20-experimental-beta-0304-non-reasoning",
            "grok-4.20-beta-0309-non-reasoning",
        ],
        GROK_43_PRICING,
    ),
];

pub(super) fn model_pricing(model: &str) -> Option<ModelPricing> {
    PRICING_RULES
        .iter()
        .find(|(models, _)| models.contains(&model))
        .map(|(_, price)| *price)
}

pub(crate) fn pricing_catalog() -> gateway_admin::model::pricing::ProviderPricingCatalog {
    use gateway_core::metering::{ModelPriceOverride, TokenPrice, TokenPriceOverride};
    let price = |ticks: u128| -> TokenPrice {
        Decimal::from_scaled(ticks * 1_000_000)
            .expect("内置价格在范围内")
            .canonical()
            .try_into()
            .expect("内置价格精度合法")
    };
    PRICING_RULES
        .iter()
        .flat_map(|(models, pricing)| {
            models.iter().map(|model| {
                let bands = [
                    ("standard", pricing.short, 1),
                    ("fast", pricing.short, 2),
                    ("long_standard", pricing.long, 1),
                    ("long_fast", pricing.long, 2),
                ]
                .into_iter()
                .map(|(band, rates, multiplier)| {
                    (
                        band.to_owned(),
                        TokenPriceOverride {
                            input: price(rates.input_ticks * multiplier),
                            output: price(rates.output_ticks * multiplier),
                            cache_read: price(rates.cached_input_ticks * multiplier),
                            cache_write: price(rates.input_ticks * multiplier),
                        },
                    )
                })
                .collect();
                (
                    (*model).to_owned(),
                    ModelPriceOverride {
                        multiplier_bps: 10_000,
                        bands,
                    },
                )
            })
        })
        .collect()
}
