//! OpenAI 内置价目、型号匹配、服务档位和管理价目投影

pub(crate) const SUPPORTED_PRICING_BANDS: &[&str] = &[
    "standard",
    "fast",
    "ultrafast",
    "flex",
    "long_standard",
    "long_fast",
    "long_ultrafast",
    "long_flex",
    "image",
];

use gateway_core::metering::Decimal;

#[derive(Clone, Copy)]
pub(super) struct TokenRates {
    pub(super) explicit_cache: bool,
    pub(super) cache_write_ticks: Option<u128>,
    pub(super) input_ticks: u128,
    pub(super) output_ticks: u128,
    pub(super) cache_read_ticks: u128,
}

impl TokenRates {
    const ZERO: Self = Self::new(0, 0, 0);

    /// 参数单位为 USD / 1M Token 的万分之一，数值也恰好等于单 Token 的 USD ticks
    pub(super) const fn new(input_ticks: u128, output_ticks: u128, cache_read_ticks: u128) -> Self {
        Self {
            input_ticks,
            output_ticks,
            cache_read_ticks,
            explicit_cache: false,
            cache_write_ticks: None,
        }
    }

    pub(super) const fn is_configured(self) -> bool {
        self.explicit_cache
            || self.input_ticks > 0
            || self.output_ticks > 0
            || self.cache_read_ticks > 0
    }
}

#[derive(Clone, Copy)]
pub(super) struct ModelPricing {
    standard: TokenRates,
    flex: TokenRates,
    fast: TokenRates,
    ultrafast: TokenRates,
    pub(super) long_standard: TokenRates,
    long_flex: TokenRates,
    long_fast: TokenRates,
    long_ultrafast: TokenRates,
    pub(super) cache_write_percent: u32,
    pub(super) unpriced_long_context: bool,
}

impl ModelPricing {
    const fn new(input: u128, output: u128, cache_read: u128) -> Self {
        Self {
            standard: TokenRates::new(input, output, cache_read),
            flex: TokenRates::ZERO,
            fast: TokenRates::ZERO,
            ultrafast: TokenRates::ZERO,
            long_standard: TokenRates::ZERO,
            long_flex: TokenRates::ZERO,
            long_fast: TokenRates::ZERO,
            long_ultrafast: TokenRates::ZERO,
            cache_write_percent: 0,
            unpriced_long_context: false,
        }
    }

    const fn with_flex(mut self, input: u128, output: u128, cache_read: u128) -> Self {
        self.flex = TokenRates::new(input, output, cache_read);
        self
    }

    const fn with_fast(mut self, input: u128, output: u128, cache_read: u128) -> Self {
        self.fast = TokenRates::new(input, output, cache_read);
        self
    }

    const fn with_ultrafast(mut self, input: u128, output: u128, cache_read: u128) -> Self {
        self.ultrafast = TokenRates::new(input, output, cache_read);
        self
    }

    const fn with_long(mut self, input: u128, output: u128, cache_read: u128) -> Self {
        self.long_standard = TokenRates::new(input, output, cache_read);
        self
    }

    const fn with_long_flex(mut self, input: u128, output: u128, cache_read: u128) -> Self {
        self.long_flex = TokenRates::new(input, output, cache_read);
        self
    }

    const fn with_long_fast(mut self, input: u128, output: u128, cache_read: u128) -> Self {
        self.long_fast = TokenRates::new(input, output, cache_read);
        self
    }

    const fn with_long_ultrafast(mut self, input: u128, output: u128, cache_read: u128) -> Self {
        self.long_ultrafast = TokenRates::new(input, output, cache_read);
        self
    }

    const fn with_cache_write(mut self, percent: u32) -> Self {
        self.cache_write_percent = percent;
        self
    }

    const fn with_unpriced_long_context(mut self) -> Self {
        self.unpriced_long_context = true;
        self
    }

    pub(super) fn rates(self, tier: PricingTier, long_context: bool) -> Option<TokenRates> {
        if long_context && self.unpriced_long_context {
            return None;
        }
        // 只有公布长上下文价格的模型才在阈值处切换费率
        // 若短上下文价格覆盖全部支持范围，长上下文栏的横线不代表额外的不可用档位
        let uses_long_rates = long_context && self.long_standard.is_configured();
        let rates = match (tier, uses_long_rates) {
            (PricingTier::Standard, false) => self.standard,
            (PricingTier::Flex, false) => self.flex,
            (PricingTier::Fast, false) => self.fast,
            (PricingTier::Ultrafast, false) => self.ultrafast,
            (PricingTier::Standard, true) => self.long_standard,
            (PricingTier::Flex, true) => self.long_flex,
            (PricingTier::Fast, true) => self.long_fast,
            (PricingTier::Ultrafast, true) => self.long_ultrafast,
        };
        rates.is_configured().then_some(rates)
    }
}

#[derive(Clone, Copy)]
pub(super) enum PricingTier {
    Standard,
    Flex,
    Fast,
    Ultrafast,
}

#[derive(Clone, Copy)]
struct PricingRule {
    model: &'static str,
    pricing: ModelPricing,
}

// 价格来源：https://developers.openai.com/api/docs/pricing
// 只登记已核验的常规价格及服务档位；临时优惠不写入内置价目
// 原有规则于 2026-09-09 核验，GPT-6.1 Sol 于 2026-09-29 核验，其余新增型号见对应条目
// Astra 与 GPT-6.1 Sol 的 Ultrafast 档位于 2026-10-12 对照官方价目表核验
// 已按 https://developers.openai.com/api/docs/deprecations 核验至 2026-09-13，
// 移除已关闭的型号；仅宣布弃用但尚未到关闭日期的型号继续保留
const PRICING_RULES: &[PricingRule] = &[
    // GPT-6.2 的 Ultrafast 使用官方价目表中的 6 倍常规价，核验日期 2026-11-18
    PricingRule {
        model: "gpt-6.2-astra",
        pricing: ModelPricing::new(80_000, 400_000, 4_000)
            .with_cache_write(125)
            .with_fast(160_000, 800_000, 8_000)
            .with_ultrafast(480_000, 2_400_000, 24_000)
            .with_long(160_000, 600_000, 8_000)
            .with_long_fast(320_000, 1_200_000, 16_000)
            .with_long_ultrafast(960_000, 3_600_000, 48_000),
    },
    PricingRule {
        model: "gpt-6.2-sol",
        pricing: ModelPricing::new(20_000, 100_000, 1_000)
            .with_cache_write(125)
            .with_fast(40_000, 200_000, 2_000)
            .with_ultrafast(120_000, 600_000, 6_000)
            .with_long(40_000, 150_000, 2_000)
            .with_long_fast(80_000, 300_000, 4_000)
            .with_long_ultrafast(240_000, 900_000, 12_000),
    },
    // Astra：https://developers.openai.com/api/docs/models/gpt-6-astra
    // 已于 2026-09-09 对照官方价目表核验
    PricingRule {
        model: "gpt-6-astra",
        pricing: ModelPricing::new(100_000, 500_000, 10_000)
            .with_cache_write(125)
            .with_flex(50_000, 250_000, 5_000)
            .with_fast(200_000, 1_000_000, 20_000)
            .with_ultrafast(600_000, 3_000_000, 60_000)
            .with_long(200_000, 750_000, 20_000)
            .with_long_flex(100_000, 375_000, 10_000)
            .with_long_fast(400_000, 1_500_000, 40_000)
            .with_long_ultrafast(1_200_000, 4_500_000, 120_000),
    },
    PricingRule {
        model: "gpt-6.1-sol",
        pricing: ModelPricing::new(20_000, 100_000, 1_000)
            .with_cache_write(125)
            .with_flex(10_000, 50_000, 500)
            .with_fast(40_000, 200_000, 2_000)
            .with_ultrafast(120_000, 600_000, 6_000)
            .with_long(40_000, 150_000, 2_000)
            .with_long_flex(20_000, 75_000, 1_000)
            .with_long_fast(80_000, 300_000, 4_000)
            .with_long_ultrafast(240_000, 900_000, 12_000),
    },
    // https://developers.openai.com/api/docs/models/gpt-6.1-terra，核验日期 2026-10-27
    PricingRule {
        model: "gpt-6.1-terra",
        pricing: ModelPricing::new(10_000, 50_000, 500)
            .with_cache_write(125)
            .with_flex(5_000, 25_000, 250)
            .with_fast(20_000, 100_000, 1_000)
            .with_long(20_000, 75_000, 1_000)
            .with_long_flex(10_000, 37_500, 500)
            .with_long_fast(40_000, 150_000, 2_000),
    },
    // https://developers.openai.com/api/docs/models/gpt-6-sol，核验日期 2026-09-24
    PricingRule {
        model: "gpt-6-sol",
        pricing: ModelPricing::new(20_000, 100_000, 2_000)
            .with_cache_write(125)
            .with_flex(10_000, 50_000, 1_000)
            .with_fast(40_000, 200_000, 4_000)
            .with_long(40_000, 150_000, 4_000)
            .with_long_flex(20_000, 75_000, 2_000)
            .with_long_fast(80_000, 300_000, 8_000),
    },
    // https://developers.openai.com/api/docs/models/gpt-6-luna，核验日期 2026-09-24
    PricingRule {
        model: "gpt-6-luna",
        pricing: ModelPricing::new(1_000, 5_000, 100)
            .with_cache_write(125)
            .with_flex(500, 2_500, 50)
            .with_fast(2_000, 10_000, 200)
            .with_long(2_000, 7_500, 200)
            .with_long_flex(1_000, 3_750, 100)
            .with_long_fast(4_000, 15_000, 400),
    },
    PricingRule {
        model: "gpt-5.6-sol",
        pricing: ModelPricing::new(50_000, 300_000, 5_000)
            .with_cache_write(125)
            .with_flex(25_000, 150_000, 2_500)
            .with_fast(100_000, 600_000, 10_000)
            .with_long(100_000, 450_000, 10_000)
            .with_long_flex(50_000, 225_000, 5_000)
            .with_long_fast(200_000, 900_000, 20_000),
    },
    PricingRule {
        model: "gpt-5.6-terra",
        pricing: ModelPricing::new(20_000, 120_000, 2_000)
            .with_cache_write(125)
            .with_flex(10_000, 60_000, 1_000)
            .with_fast(40_000, 240_000, 4_000)
            .with_long(40_000, 180_000, 4_000)
            .with_long_flex(20_000, 90_000, 2_000)
            .with_long_fast(80_000, 360_000, 8_000),
    },
    PricingRule {
        model: "gpt-5.6-luna",
        pricing: ModelPricing::new(2_000, 12_000, 200)
            .with_cache_write(125)
            .with_flex(1_000, 6_000, 100)
            .with_fast(4_000, 24_000, 400)
            .with_long(4_000, 18_000, 400)
            .with_long_flex(2_000, 9_000, 200)
            .with_long_fast(8_000, 36_000, 800),
    },
    PricingRule {
        model: "gpt-5.6",
        pricing: ModelPricing::new(50_000, 300_000, 5_000)
            .with_cache_write(125)
            .with_flex(25_000, 150_000, 2_500)
            .with_fast(100_000, 600_000, 10_000)
            .with_long(100_000, 450_000, 10_000)
            .with_long_flex(50_000, 225_000, 5_000)
            .with_long_fast(200_000, 900_000, 20_000),
    },
    PricingRule {
        model: "gpt-5.5-pro",
        pricing: ModelPricing::new(300_000, 1_800_000, 0)
            .with_flex(150_000, 900_000, 0)
            .with_long(600_000, 2_700_000, 0),
    },
    PricingRule {
        model: "gpt-5.5",
        pricing: ModelPricing::new(50_000, 300_000, 5_000)
            .with_flex(25_000, 150_000, 2_500)
            .with_fast(125_000, 750_000, 12_500)
            .with_long(100_000, 450_000, 10_000)
            .with_long_flex(50_000, 225_000, 5_000),
    },
    PricingRule {
        model: "gpt-5.4-mini",
        pricing: ModelPricing::new(7_500, 45_000, 750)
            .with_flex(3_750, 22_500, 375)
            .with_fast(15_000, 90_000, 1_500),
    },
    PricingRule {
        model: "gpt-5.4-nano",
        pricing: ModelPricing::new(2_000, 12_500, 200).with_flex(1_000, 6_250, 100),
    },
    PricingRule {
        model: "gpt-5.4-pro",
        pricing: ModelPricing::new(300_000, 1_800_000, 0)
            .with_flex(150_000, 900_000, 0)
            .with_long(600_000, 2_700_000, 0)
            .with_long_flex(300_000, 1_350_000, 0),
    },
    PricingRule {
        model: "gpt-5.4",
        pricing: ModelPricing::new(25_000, 150_000, 2_500)
            .with_flex(12_500, 75_000, 1_300)
            .with_fast(50_000, 300_000, 5_000)
            .with_long(50_000, 225_000, 5_000)
            .with_long_flex(25_000, 112_500, 2_500),
    },
    PricingRule {
        model: "gpt-5.3-codex",
        pricing: ModelPricing::new(17_500, 140_000, 1_750).with_fast(35_000, 280_000, 3_500),
    },
    PricingRule {
        model: "gpt-5.2-pro",
        pricing: ModelPricing::new(210_000, 1_680_000, 0),
    },
    PricingRule {
        model: "gpt-5.2",
        pricing: ModelPricing::new(17_500, 140_000, 1_750)
            .with_flex(8_750, 70_000, 875)
            .with_fast(35_000, 280_000, 3_500),
    },
    PricingRule {
        model: "gpt-5.1",
        pricing: ModelPricing::new(12_500, 100_000, 1_250)
            .with_flex(6_250, 50_000, 625)
            .with_fast(25_000, 200_000, 2_500),
    },
    PricingRule {
        model: "gpt-5-mini",
        pricing: ModelPricing::new(2_500, 20_000, 250)
            .with_flex(1_250, 10_000, 125)
            .with_fast(4_500, 36_000, 450),
    },
    PricingRule {
        model: "gpt-5-nano",
        pricing: ModelPricing::new(500, 4_000, 50).with_flex(250, 2_000, 25),
    },
    PricingRule {
        model: "gpt-5-pro",
        pricing: ModelPricing::new(150_000, 1_200_000, 0),
    },
    PricingRule {
        model: "gpt-5",
        pricing: ModelPricing::new(12_500, 100_000, 1_250)
            .with_flex(6_250, 50_000, 625)
            .with_fast(25_000, 200_000, 2_500),
    },
    PricingRule {
        model: "gpt-4.1-mini",
        pricing: ModelPricing::new(4_000, 16_000, 1_000).with_fast(7_000, 28_000, 1_750),
    },
    PricingRule {
        model: "gpt-4.1-nano",
        pricing: ModelPricing::new(1_000, 4_000, 250).with_fast(2_000, 8_000, 500),
    },
    PricingRule {
        model: "gpt-4.1",
        pricing: ModelPricing::new(20_000, 80_000, 5_000).with_fast(35_000, 140_000, 8_750),
    },
    PricingRule {
        model: "gpt-4o-2024-05-13",
        pricing: ModelPricing::new(50_000, 150_000, 0).with_fast(87_500, 262_500, 0),
    },
    PricingRule {
        model: "gpt-4o-mini",
        pricing: ModelPricing::new(1_500, 6_000, 750).with_fast(2_500, 10_000, 1_250),
    },
    PricingRule {
        model: "gpt-4o",
        pricing: ModelPricing::new(25_000, 100_000, 12_500).with_fast(42_500, 170_000, 21_250),
    },
    PricingRule {
        model: "o1-pro",
        pricing: ModelPricing::new(1_500_000, 6_000_000, 0),
    },
    PricingRule {
        model: "o1",
        pricing: ModelPricing::new(150_000, 600_000, 75_000),
    },
    PricingRule {
        model: "o3-pro",
        pricing: ModelPricing::new(200_000, 800_000, 0),
    },
    PricingRule {
        model: "o3-mini",
        pricing: ModelPricing::new(11_000, 44_000, 5_500),
    },
    PricingRule {
        model: "o3",
        pricing: ModelPricing::new(20_000, 80_000, 5_000)
            .with_flex(10_000, 40_000, 2_500)
            .with_fast(35_000, 140_000, 8_750),
    },
    PricingRule {
        model: "o4-mini",
        pricing: ModelPricing::new(11_000, 44_000, 2_750)
            .with_flex(5_500, 22_000, 1_380)
            .with_fast(20_000, 80_000, 5_000),
    },
    PricingRule {
        model: "gpt-4-turbo",
        pricing: ModelPricing::new(100_000, 300_000, 0),
    },
    PricingRule {
        model: "gpt-4",
        pricing: ModelPricing::new(300_000, 600_000, 0),
    },
    PricingRule {
        model: "gpt-3.5-turbo-instruct",
        pricing: ModelPricing::new(15_000, 20_000, 0),
    },
    PricingRule {
        model: "gpt-3.5-turbo-1106",
        pricing: ModelPricing::new(10_000, 20_000, 0),
    },
    PricingRule {
        model: "gpt-3.5-turbo",
        pricing: ModelPricing::new(5_000, 15_000, 0),
    },
    // 各变体独立采用已公开的价格与档位，不能继承父型号的所有档位
    // Cyber 长上下文的官方来源存在差异，该区间暂不估价
    PricingRule {
        model: "gpt-5.6-cyber",
        pricing: ModelPricing::new(125_000, 750_000, 12_500)
            .with_cache_write(125)
            .with_unpriced_long_context(),
    },
    PricingRule {
        model: "gpt-5.5-cyber",
        pricing: ModelPricing::new(125_000, 750_000, 12_500).with_unpriced_long_context(),
    },
    PricingRule {
        model: "chat-latest",
        pricing: ModelPricing::new(50_000, 300_000, 5_000),
    },
];

pub(super) const IMAGE_MODELS: &[&str] = &[
    "gpt-image-2",
    "gpt-image-2-2026-04-21",
    "gpt-image-2.5-sunburst",
    "gpt-image-2.5-sunburst-2026-09-08",
    "gpt-image-2.5-flare",
    "gpt-image-2.5-flare-2026-09-08",
];
pub(super) const IMAGE_TEXT_RATES: TokenRates = TokenRates::new(50_000, 0, 12_500);
pub(super) const IMAGE_TOKEN_RATES: TokenRates = TokenRates::new(80_000, 300_000, 20_000);

pub(super) fn model_pricing(model: &str) -> Option<ModelPricing> {
    let normalized = normalize_model_name(model);
    let model = pricing_model_name(&normalized);
    PRICING_RULES
        .iter()
        .find(|rule| model == rule.model)
        .map(|rule| rule.pricing)
}

pub(super) fn normalize_model_name(model: &str) -> String {
    model
        .trim()
        .trim_start_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

// 仅已核验的别名和快照可以共用价格；未知后缀、未来日期和微调模型
// 不得直接继承父型号的价格
pub(super) fn pricing_model_name(model: &str) -> &str {
    match model {
        "gpt-3.5-turbo-0125" => "gpt-3.5-turbo",
        "gpt-4-0613" => "gpt-4",
        "gpt-4-turbo-2024-04-09" => "gpt-4-turbo",
        "gpt-4.1-2025-04-14" => "gpt-4.1",
        "gpt-4.1-mini-2025-04-14" => "gpt-4.1-mini",
        "gpt-4.1-nano-2025-04-14" => "gpt-4.1-nano",
        "gpt-4o-2024-08-06" => "gpt-4o",
        "gpt-4o-2024-11-20" => "gpt-4o",
        "gpt-4o-mini-2024-07-18" => "gpt-4o-mini",
        "gpt-5-2025-08-07" => "gpt-5",
        "gpt-5-mini-2025-08-07" => "gpt-5-mini",
        "gpt-5-nano-2025-08-07" => "gpt-5-nano",
        "gpt-5-pro-2025-10-06" => "gpt-5-pro",
        "gpt-5.1-2025-11-13" => "gpt-5.1",
        "gpt-5.2-2025-12-11" => "gpt-5.2",
        "gpt-5.2-pro-2025-12-11" => "gpt-5.2-pro",
        "gpt-5.4-2026-03-05" => "gpt-5.4",
        "gpt-5.4-mini-2026-03-17" => "gpt-5.4-mini",
        "gpt-5.4-nano-2026-03-17" => "gpt-5.4-nano",
        "gpt-5.4-pro-2026-03-05" => "gpt-5.4-pro",
        "gpt-5.5-2026-04-23" => "gpt-5.5",
        "gpt-5.5-pro-2026-04-23" => "gpt-5.5-pro",
        "gpt-daybreak-blue-latest" => "gpt-5.6-sol",
        "gpt-daybreak-red-latest" => "gpt-5.6-cyber",
        "o1-2024-12-17" => "o1",
        "o1-pro-2025-03-19" => "o1-pro",
        "o3-2025-04-16" => "o3",
        "o3-mini-2025-01-31" => "o3-mini",
        "o3-pro-2025-06-10" => "o3-pro",
        "o4-mini-2025-04-16" => "o4-mini",
        _ => model,
    }
}

pub(super) fn model_matches_rule(model: &str, rule: &str) -> bool {
    pricing_model_name(model) == rule
}

pub(super) fn cache_write_rate(rates: TokenRates, percent: u32) -> Option<u128> {
    if let Some(ticks) = rates.cache_write_ticks {
        return Some(ticks);
    }
    if percent == 0 {
        return Some(0);
    }
    apply_percent(rates.input_ticks, percent)
}

fn apply_percent(value: u128, percent: u32) -> Option<u128> {
    value
        .checked_mul(u128::from(percent))?
        .checked_add(50)
        .map(|scaled| scaled / 100)
}

pub(crate) fn pricing_catalog()
-> std::collections::BTreeMap<String, gateway_core::metering::ModelPriceOverride> {
    use gateway_core::metering::{ModelPriceOverride, TokenPrice, TokenPriceOverride};
    let price = |ticks: u128| -> Option<TokenPrice> {
        Decimal::from_scaled(ticks.checked_mul(1_000_000)?)
            .ok()?
            .canonical()
            .try_into()
            .ok()
    };
    let mut catalog: std::collections::BTreeMap<_, _> = PRICING_RULES
        .iter()
        .map(|rule| {
            let pricing = rule.pricing;
            let bands = [
                ("standard", pricing.standard),
                ("fast", pricing.fast),
                ("ultrafast", pricing.ultrafast),
                ("flex", pricing.flex),
                ("long_standard", pricing.long_standard),
                ("long_fast", pricing.long_fast),
                ("long_ultrafast", pricing.long_ultrafast),
                ("long_flex", pricing.long_flex),
            ]
            .into_iter()
            .filter(|(_, rates)| rates.is_configured())
            .filter_map(|(name, rates)| {
                Some((
                    name.to_owned(),
                    TokenPriceOverride {
                        input: price(rates.input_ticks)?,
                        output: price(rates.output_ticks)?,
                        cache_read: price(if rates.cache_read_ticks == 0 {
                            rates.input_ticks
                        } else {
                            rates.cache_read_ticks
                        })?,
                        // 内置表没有独立缓存写入费时，这部分 Token 原本按输入计费
                        // 管理页复制来源档位不能把它改成显式免费
                        cache_write: price(if pricing.cache_write_percent == 0 {
                            rates.input_ticks
                        } else {
                            cache_write_rate(rates, pricing.cache_write_percent)?
                        })?,
                    },
                ))
            })
            .collect();
            (
                rule.model.to_owned(),
                ModelPriceOverride {
                    multiplier_bps: 10_000,
                    bands,
                },
            )
        })
        .collect();
    for model in IMAGE_MODELS {
        let bands = [("standard", IMAGE_TEXT_RATES), ("image", IMAGE_TOKEN_RATES)]
            .into_iter()
            .map(|(band, rates)| {
                (
                    band.to_owned(),
                    TokenPriceOverride {
                        input: price(rates.input_ticks).expect("内置图像价格合法"),
                        output: price(rates.output_ticks).expect("内置图像价格合法"),
                        cache_read: price(rates.cache_read_ticks).expect("内置图像价格合法"),
                        cache_write: price(0).expect("零价格合法"),
                    },
                )
            })
            .collect();
        catalog.insert(
            (*model).to_owned(),
            ModelPriceOverride {
                multiplier_bps: 10_000,
                bands,
            },
        );
    }
    catalog
}
