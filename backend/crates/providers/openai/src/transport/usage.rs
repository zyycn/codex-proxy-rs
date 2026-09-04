use gateway_core::metering::{
    CalculatedCostAmounts, CalculatedCostBreakdown, CalculatedCostRates, CurrencyCode, Decimal,
    Money,
};
use gateway_protocol::openai::events::{TokenUsage, retry_after_seconds_from_body};
use reqwest::StatusCode;
use serde_json::Value;

use super::{
    CodexBackendClient, CodexClientError, CodexClientResult, CodexRequestContext,
    client::{read_capped_response_body, retry_after_seconds, truncate_for_error},
    endpoints::usage_endpoint_url,
    response_meta,
};

const LONG_CONTEXT_THRESHOLD: u64 = 272_000;
const WEB_SEARCH_CALL_TICKS: u128 = 100_000_000;
const WEB_SEARCH_PREVIEW_NON_REASONING_CALL_TICKS: u128 = 250_000_000;

/// OpenAI 公开 Token 价格计算所需的单次用量事实。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OpenAiBillingUsage {
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    cache_write_tokens: u64,
    image_input_tokens: u64,
    image_output_tokens: u64,
    web_search_calls: u64,
    web_search_pricing: Option<WebSearchPricing>,
}

impl OpenAiBillingUsage {
    /// 构造不含托管工具费用的 Token 用量。
    #[must_use]
    pub const fn new(
        input_tokens: u64,
        output_tokens: u64,
        cached_tokens: u64,
        cache_write_tokens: u64,
    ) -> Self {
        Self {
            input_tokens,
            output_tokens,
            cached_tokens,
            cache_write_tokens,
            image_input_tokens: 0,
            image_output_tokens: 0,
            web_search_calls: 0,
            web_search_pricing: None,
        }
    }

    pub(crate) const fn with_web_search_calls(
        mut self,
        calls: u64,
        pricing: Option<WebSearchPricing>,
    ) -> Self {
        self.web_search_calls = calls;
        self.web_search_pricing = pricing;
        self
    }
}

impl From<TokenUsage> for OpenAiBillingUsage {
    fn from(usage: TokenUsage) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cached_tokens: usage.cached_tokens,
            cache_write_tokens: usage.cache_write_tokens,
            image_input_tokens: usage.image_input_tokens,
            image_output_tokens: usage.image_output_tokens,
            web_search_calls: 0,
            web_search_pricing: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WebSearchPricing {
    Standard,
    PreviewNonReasoning,
}

impl WebSearchPricing {
    const fn price_per_call_ticks(self) -> u128 {
        match self {
            Self::Standard => WEB_SEARCH_CALL_TICKS,
            Self::PreviewNonReasoning => WEB_SEARCH_PREVIEW_NON_REASONING_CALL_TICKS,
        }
    }
}

#[derive(Clone, Copy)]
struct TokenRates {
    input_ticks: u128,
    output_ticks: u128,
    cache_read_ticks: u128,
}

impl TokenRates {
    const ZERO: Self = Self::new(0, 0, 0);

    /// 参数单位为 USD / 1M Token 的万分之一，数值也恰好等于单 Token 的 USD ticks。
    const fn new(input_ticks: u128, output_ticks: u128, cache_read_ticks: u128) -> Self {
        Self {
            input_ticks,
            output_ticks,
            cache_read_ticks,
        }
    }

    const fn is_configured(self) -> bool {
        self.input_ticks > 0 || self.output_ticks > 0 || self.cache_read_ticks > 0
    }
}

#[derive(Clone, Copy)]
struct ModelPricing {
    standard: TokenRates,
    flex: TokenRates,
    fast: TokenRates,
    long_standard: TokenRates,
    long_flex: TokenRates,
    long_fast: TokenRates,
    cache_write_percent: u32,
}

impl ModelPricing {
    const fn new(input: u128, output: u128, cache_read: u128) -> Self {
        Self {
            standard: TokenRates::new(input, output, cache_read),
            flex: TokenRates::ZERO,
            fast: TokenRates::ZERO,
            long_standard: TokenRates::ZERO,
            long_flex: TokenRates::ZERO,
            long_fast: TokenRates::ZERO,
            cache_write_percent: 0,
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

    const fn with_cache_write(mut self, percent: u32) -> Self {
        self.cache_write_percent = percent;
        self
    }

    fn rates(self, tier: PricingTier, long_context: bool) -> Option<TokenRates> {
        // Only models with a published long-context column switch at the threshold.
        // A dash in that column is not a second, unavailable price tier for models
        // whose entire supported context is covered by the short-context price.
        let uses_long_rates = long_context && self.long_standard.is_configured();
        let rates = match (tier, uses_long_rates) {
            (PricingTier::Standard, false) => self.standard,
            (PricingTier::Flex, false) => self.flex,
            (PricingTier::Fast, false) => self.fast,
            (PricingTier::Standard, true) => self.long_standard,
            (PricingTier::Flex, true) => self.long_flex,
            (PricingTier::Fast, true) => self.long_fast,
        };
        rates.is_configured().then_some(rates)
    }
}

#[derive(Clone, Copy)]
enum PricingTier {
    Standard,
    Flex,
    Fast,
}

#[derive(Clone, Copy)]
struct PricingRule {
    model: &'static str,
    pricing: ModelPricing,
}

// Source: https://developers.openai.com/api/docs/pricing (verified 2026-08-14).
const PRICING_RULES: &[PricingRule] = &[
    // Astra: https://developers.openai.com/api/docs/models/gpt-6-astra
    // Rates verified against the pricing table on 2026-09-05.
    PricingRule {
        model: "gpt-6-astra",
        pricing: ModelPricing::new(100_000, 500_000, 10_000)
            .with_cache_write(125)
            .with_flex(50_000, 250_000, 5_000)
            .with_fast(200_000, 1_000_000, 20_000)
            .with_long(200_000, 750_000, 20_000)
            .with_long_flex(100_000, 375_000, 10_000)
            .with_long_fast(400_000, 1_500_000, 40_000),
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
];

const UNPRICED_MODELS: &[&str] = &["gpt-5.3-codex-spark"];

#[derive(Clone, Copy)]
struct TokenAmounts {
    input_ticks: u128,
    output_ticks: u128,
    cache_read_ticks: u128,
    cache_write_ticks: u128,
    total_ticks: u128,
}

/// 按 OpenAI Provider 当前受控价格规则计算费用明细。
#[must_use]
pub fn openai_billing_breakdown(
    model: &str,
    usage: OpenAiBillingUsage,
    service_tier: Option<&str>,
) -> Option<CalculatedCostBreakdown> {
    openai_billing_breakdown_with_context(
        model,
        usage,
        service_tier,
        usage.input_tokens > LONG_CONTEXT_THRESHOLD,
    )
}

fn openai_billing_breakdown_with_context(
    model: &str,
    usage: OpenAiBillingUsage,
    service_tier: Option<&str>,
    long_context: bool,
) -> Option<CalculatedCostBreakdown> {
    if usage.cached_tokens > usage.input_tokens
        || usage.image_input_tokens > 0
        || usage.image_output_tokens > 0
    {
        return None;
    }
    let web_search_ticks = web_search_amount_ticks(usage)?;
    let pricing = model_pricing(model)?;
    let normalized_tier = normalize_service_tier(service_tier);
    let tier = pricing_tier(normalized_tier.as_deref())?;
    let standard_rates = pricing.rates(PricingTier::Standard, long_context)?;
    let selected_rates = pricing.rates(tier, long_context)?;
    let mut standard = token_amounts(
        standard_rates,
        pricing.cache_write_percent,
        usage.input_tokens,
        usage.output_tokens,
        usage.cached_tokens,
        usage.cache_write_tokens,
    )?;
    let mut selected = token_amounts(
        selected_rates,
        pricing.cache_write_percent,
        usage.input_tokens,
        usage.output_tokens,
        usage.cached_tokens,
        usage.cache_write_tokens,
    )?;
    standard.total_ticks = standard.total_ticks.checked_add(web_search_ticks)?;
    selected.total_ticks = selected.total_ticks.checked_add(web_search_ticks)?;
    let multiplier_percent =
        effective_multiplier_percent(selected.total_ticks, standard.total_ticks)?;
    let cache_write_rate = cache_write_rate(selected_rates, pricing.cache_write_percent)?;

    Some(CalculatedCostBreakdown::new(
        CalculatedCostAmounts::new(
            usd_money(selected.input_ticks)?,
            usd_money(selected.output_ticks)?,
            usd_money(selected.cache_read_ticks)?,
            usd_money(selected.cache_write_ticks)?,
            usd_money(standard.total_ticks)?,
            usd_money(selected.total_ticks)?,
        ),
        CalculatedCostRates::new(
            usd_price_per_million(selected_rates.input_ticks)?,
            usd_price_per_million(selected_rates.output_ticks)?,
            usd_price_per_million(selected_rates.cache_read_ticks)?,
            usd_price_per_million(cache_write_rate)?,
        ),
        Some(normalized_tier.unwrap_or_else(|| "default".to_owned())),
        multiplier_percent,
    ))
}

fn model_pricing(model: &str) -> Option<ModelPricing> {
    let normalized = normalize_model_name(model);
    if UNPRICED_MODELS
        .iter()
        .any(|rule| model_matches_rule(&normalized, rule))
    {
        return None;
    }
    PRICING_RULES
        .iter()
        .filter(|rule| model_matches_rule(&normalized, rule.model))
        .max_by_key(|rule| rule.model.len())
        .map(|rule| rule.pricing)
}

pub(crate) fn web_search_pricing(model: &str, tools: Option<&[Value]>) -> Option<WebSearchPricing> {
    let mut standard = false;
    let mut preview = false;
    for tool_type in tools
        .into_iter()
        .flatten()
        .filter_map(|tool| tool.get("type").and_then(Value::as_str))
    {
        if tool_type == "web_search_preview" || tool_type.starts_with("web_search_preview_") {
            preview = true;
        } else if tool_type == "web_search" || tool_type.starts_with("web_search_") {
            standard = true;
        }
    }
    match (standard, preview) {
        // These two models bill non-preview search content as a fixed 8K input block.
        // The response does not expose enough detail to replace that block without
        // double-counting ordinary input, so leave the request unpriced.
        (true, false) if fixed_block_web_search_model(model) => None,
        (true, false) => Some(WebSearchPricing::Standard),
        (false, true) if reasoning_model(model) => Some(WebSearchPricing::Standard),
        (false, true) => Some(WebSearchPricing::PreviewNonReasoning),
        (false, false) | (true, true) => None,
    }
}

fn fixed_block_web_search_model(model: &str) -> bool {
    let normalized = normalize_model_name(model);
    ["gpt-4o-mini", "gpt-4.1-mini"]
        .iter()
        .any(|rule| model_matches_rule(&normalized, rule))
}

fn reasoning_model(model: &str) -> bool {
    let normalized = normalize_model_name(model);
    normalized.starts_with("gpt-5")
        || model_matches_rule(&normalized, "gpt-6-astra")
        || normalized.starts_with("o1")
        || normalized.starts_with("o3")
        || normalized.starts_with("o4")
}

fn pricing_tier(service_tier: Option<&str>) -> Option<PricingTier> {
    match service_tier {
        None | Some("auto" | "default" | "standard") => Some(PricingTier::Standard),
        Some("flex") => Some(PricingTier::Flex),
        Some("fast" | "priority") => Some(PricingTier::Fast),
        Some(_) => None,
    }
}

fn web_search_amount_ticks(usage: OpenAiBillingUsage) -> Option<u128> {
    if usage.web_search_calls == 0 {
        return Some(0);
    }
    u128::from(usage.web_search_calls).checked_mul(usage.web_search_pricing?.price_per_call_ticks())
}

fn token_amounts(
    rates: TokenRates,
    cache_write_percent: u32,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    cache_write_tokens: u64,
) -> Option<TokenAmounts> {
    let billed_cache_read = if rates.cache_read_ticks > 0 {
        cached_tokens.min(input_tokens)
    } else {
        0
    };
    let cache_write_rate = cache_write_rate(rates, cache_write_percent)?;
    let billed_cache_write = if cache_write_rate > 0 {
        cache_write_tokens.min(input_tokens.saturating_sub(billed_cache_read))
    } else {
        0
    };
    let uncached_input = input_tokens
        .saturating_sub(billed_cache_read)
        .saturating_sub(billed_cache_write);
    let input_ticks = u128::from(uncached_input).checked_mul(rates.input_ticks)?;
    let output_ticks = u128::from(output_tokens).checked_mul(rates.output_ticks)?;
    let cache_read_ticks = u128::from(billed_cache_read).checked_mul(rates.cache_read_ticks)?;
    let cache_write_ticks = u128::from(billed_cache_write).checked_mul(cache_write_rate)?;
    let total_ticks = input_ticks
        .checked_add(output_ticks)?
        .checked_add(cache_read_ticks)?
        .checked_add(cache_write_ticks)?;
    Some(TokenAmounts {
        input_ticks,
        output_ticks,
        cache_read_ticks,
        cache_write_ticks,
        total_ticks,
    })
}

fn cache_write_rate(rates: TokenRates, percent: u32) -> Option<u128> {
    if percent == 0 {
        return Some(0);
    }
    apply_percent(rates.input_ticks, percent)
}

fn effective_multiplier_percent(total: u128, standard: u128) -> Option<u32> {
    if standard == 0 {
        return Some(100);
    }
    let rounded = total
        .checked_mul(100)?
        .checked_add(standard / 2)?
        .checked_div(standard)?;
    u32::try_from(rounded).ok()
}

fn normalize_model_name(model: &str) -> String {
    model
        .trim()
        .trim_start_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn model_matches_rule(model: &str, rule: &str) -> bool {
    if model == rule {
        return true;
    }
    model
        .strip_prefix(rule)
        .is_some_and(|suffix| matches!(suffix.as_bytes().first(), Some(b'-' | b'.' | b':')))
}

/// 规范化请求或响应携带的服务档位，供观测与计费共用。
pub(crate) fn normalize_service_tier(service_tier: Option<&str>) -> Option<String> {
    service_tier
        .map(str::trim)
        .filter(|value| {
            !value.is_empty() && value.len() <= 64 && !value.chars().any(char::is_control)
        })
        .map(str::to_ascii_lowercase)
}

fn apply_percent(value: u128, percent: u32) -> Option<u128> {
    value
        .checked_mul(u128::from(percent))?
        .checked_add(50)
        .map(|scaled| scaled / 100)
}

fn usd_money(ticks: u128) -> Option<Money> {
    Some(Money::new(
        Decimal::from_scaled(ticks).ok()?,
        CurrencyCode::new("USD").ok()?,
    ))
}

fn usd_price_per_million(per_token_ticks: u128) -> Option<Money> {
    usd_money(per_token_ticks.checked_mul(1_000_000)?)
}

/// 单次 Codex usage 响应允许保留和解析的最大字节数。
pub const MAX_CODEX_USAGE_BODY_BYTES: usize = 1024 * 1024;

impl CodexBackendClient {
    /// 获取 Codex usage JSON。
    pub async fn fetch_usage(&self, context: CodexRequestContext<'_>) -> CodexClientResult<Value> {
        let headers = self.usage_request_headers(context)?;
        let response = self
            .client
            .get(usage_endpoint_url(&self.base_url))
            .headers(headers)
            .send()
            .await?;
        let status = response.status();
        let diagnostics = response_meta::diagnostics(Some(status.as_u16()), response.headers());
        let retry_after_seconds = retry_after_seconds(response.headers(), None);
        let body = read_capped_response_body(response, MAX_CODEX_USAGE_BODY_BYTES).await?;
        if body.limit_exceeded() {
            return Err(CodexClientError::Upstream {
                status: if status.is_success() {
                    StatusCode::BAD_GATEWAY
                } else {
                    status
                },
                retry_after_seconds,
                body: "upstream usage response exceeded the body limit".to_owned(),
                client_response: None,
                diagnostics: Box::new(diagnostics),
                set_cookie_headers: Vec::new(),
                rate_limit_headers: Vec::new(),
                transport: super::client::CodexBackendTransport::HttpSse,
                transport_metrics: Box::default(),
                send_phase: super::diagnostics::CodexUpstreamSendPhase::AfterPayload,
            });
        }
        let body = body.into_string();

        if !status.is_success() {
            return Err(CodexClientError::Upstream {
                status,
                retry_after_seconds: retry_after_seconds
                    .or_else(|| retry_after_seconds_from_body(&body)),
                body,
                client_response: None,
                diagnostics: Box::new(diagnostics),
                set_cookie_headers: Vec::new(),
                rate_limit_headers: Vec::new(),
                transport: super::client::CodexBackendTransport::HttpSse,
                transport_metrics: Box::default(),
                send_phase: super::diagnostics::CodexUpstreamSendPhase::AfterPayload,
            });
        }

        match serde_json::from_str::<Value>(&body) {
            Ok(parsed) if is_usage_response(&parsed) => Ok(parsed),
            _ => Err(CodexClientError::Upstream {
                status: StatusCode::BAD_GATEWAY,
                retry_after_seconds: None,
                body: format!("invalid usage response: {}", truncate_for_error(&body)),
                client_response: None,
                diagnostics: Box::new(diagnostics),
                set_cookie_headers: Vec::new(),
                rate_limit_headers: Vec::new(),
                transport: super::client::CodexBackendTransport::HttpSse,
                transport_metrics: Box::default(),
                send_phase: super::diagnostics::CodexUpstreamSendPhase::AfterPayload,
            }),
        }
    }
}

fn is_usage_response(value: &Value) -> bool {
    value.as_object().is_some_and(|object| {
        object.get("rate_limit").is_some_and(Value::is_object)
            || object
                .get("additional_rate_limits")
                .is_some_and(Value::is_array)
            || object.get("spend_control").is_some_and(Value::is_object)
            || object.get("credits").is_some_and(Value::is_object)
    })
}
