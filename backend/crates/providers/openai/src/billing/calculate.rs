//! OpenAI Token、缓存、托管工具和服务档位的费用计算

use gateway_core::metering::{
    CalculatedCostAmounts, CalculatedCostBreakdown, CalculatedCostRates, CurrencyCode, Decimal,
    Money,
};
use gateway_protocol::openai::events::TokenUsage;
use serde_json::Value;

use super::catalog::{
    ModelPricing, PricingTier, TokenRates, cache_write_rate, model_matches_rule, model_pricing,
    normalize_model_name, pricing_model_name,
};

const LONG_CONTEXT_THRESHOLD: u64 = 272_000;
const WEB_SEARCH_CALL_TICKS: u128 = 100_000_000;
const WEB_SEARCH_PREVIEW_NON_REASONING_CALL_TICKS: u128 = 250_000_000;
const FILE_SEARCH_CALL_TICKS: u128 = 25_000_000;

/// OpenAI 公开 Token 价格计算所需的单次用量事实
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct OpenAiBillingUsage {
    input_tokens: u64,
    output_tokens: u64,
    cached_input_tokens: u64,
    cache_write_input_tokens: u64,
    image_input_tokens: u64,
    image_output_tokens: u64,
    web_search_calls: u64,
    file_search_calls: u64,
    web_search_pricing: Option<WebSearchPricing>,
}

impl OpenAiBillingUsage {
    /// 构造不含托管工具费用的 Token 用量
    #[must_use]
    pub(crate) const fn new(
        input_tokens: u64,
        output_tokens: u64,
        cached_input_tokens: u64,
        cache_write_input_tokens: u64,
    ) -> Self {
        Self {
            input_tokens,
            output_tokens,
            cached_input_tokens,
            cache_write_input_tokens,
            image_input_tokens: 0,
            image_output_tokens: 0,
            web_search_calls: 0,
            file_search_calls: 0,
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

    pub(crate) const fn with_file_search_calls(mut self, calls: u64) -> Self {
        self.file_search_calls = calls;
        self
    }
}

impl From<TokenUsage> for OpenAiBillingUsage {
    fn from(usage: TokenUsage) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cached_input_tokens: usage.cached_input_tokens,
            cache_write_input_tokens: usage.cache_write_input_tokens,
            image_input_tokens: usage.image_input_tokens,
            image_output_tokens: usage.image_output_tokens,
            web_search_calls: 0,
            file_search_calls: 0,
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
pub(super) struct TokenAmounts {
    pub(super) input_ticks: u128,
    pub(super) output_ticks: u128,
    pub(super) cache_read_ticks: u128,
    pub(super) cache_write_ticks: u128,
    pub(super) total_ticks: u128,
}

/// 按 OpenAI Provider 当前受控价格规则计算费用明细
#[must_use]
pub(crate) fn openai_billing_breakdown(
    model: &str,
    usage: OpenAiBillingUsage,
    service_tier: Option<&str>,
) -> Option<CalculatedCostBreakdown> {
    token_billing_breakdown_with_context(
        model_pricing(model),
        usage,
        service_tier,
        usage.input_tokens > LONG_CONTEXT_THRESHOLD,
        None,
    )
}

/// 使用请求开始时冻结的价格覆盖，不读取管理存储
#[must_use]
pub(crate) fn openai_billing_breakdown_with_override(
    model: &str,
    usage: OpenAiBillingUsage,
    service_tier: Option<&str>,
    pricing: Option<&gateway_core::metering::ModelPriceOverride>,
) -> Option<CalculatedCostBreakdown> {
    token_billing_breakdown_with_context(
        model_pricing(model),
        usage,
        service_tier,
        usage.input_tokens > LONG_CONTEXT_THRESHOLD,
        pricing,
    )
}

/// 其他 API 协议仅使用明确配置的标准 Token 单价，不继承 OpenAI 型号与档位规则
pub(crate) fn configured_token_billing_breakdown(
    usage: OpenAiBillingUsage,
    pricing: &gateway_core::metering::ModelPriceOverride,
) -> Option<CalculatedCostBreakdown> {
    token_billing_breakdown_with_context(None, usage, None, false, Some(pricing))
}

fn token_billing_breakdown_with_context(
    pricing: Option<ModelPricing>,
    usage: OpenAiBillingUsage,
    service_tier: Option<&str>,
    long_context: bool,
    custom: Option<&gateway_core::metering::ModelPriceOverride>,
) -> Option<CalculatedCostBreakdown> {
    if usage
        .cached_input_tokens
        .checked_add(usage.cache_write_input_tokens)?
        > usage.input_tokens
        || usage.image_input_tokens > 0
        || usage.image_output_tokens > 0
    {
        return None;
    }
    let tool_ticks = web_search_amount_ticks(usage)?
        .checked_add(u128::from(usage.file_search_calls).checked_mul(FILE_SEARCH_CALL_TICKS)?)?;
    let normalized_tier = normalize_service_tier(service_tier);
    let tier = pricing_tier(normalized_tier.as_deref())?;
    let (standard_rates, _) =
        effective_rates(pricing, custom, PricingTier::Standard, long_context)?;
    let (selected_rates, long_context_billing_applied) =
        effective_rates(pricing, custom, tier, long_context)?;
    let cache_write_percent = pricing.map_or(0, |pricing| pricing.cache_write_percent);
    let mut standard = token_amounts(
        standard_rates,
        cache_write_percent,
        usage.input_tokens,
        usage.output_tokens,
        usage.cached_input_tokens,
        usage.cache_write_input_tokens,
    )?;
    let mut selected = token_amounts(
        selected_rates,
        cache_write_percent,
        usage.input_tokens,
        usage.output_tokens,
        usage.cached_input_tokens,
        usage.cache_write_input_tokens,
    )?;
    standard.total_ticks = standard.total_ticks.checked_add(tool_ticks)?;
    selected.total_ticks = selected.total_ticks.checked_add(tool_ticks)?;
    let multiplier_percent =
        effective_multiplier_percent(selected.total_ticks, standard.total_ticks)?;
    let cache_write_rate = cache_write_rate(selected_rates, cache_write_percent)?;

    CalculatedCostBreakdown::new(
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
    )
    .with_long_context_billing(long_context_billing_applied)
    .with_custom_multiplier(custom.map_or(10_000, |pricing| pricing.multiplier_bps))
}

fn effective_rates(
    default: Option<ModelPricing>,
    custom: Option<&gateway_core::metering::ModelPriceOverride>,
    tier: PricingTier,
    long_context: bool,
) -> Option<(TokenRates, bool)> {
    let long_band = match tier {
        PricingTier::Standard => "long_standard",
        PricingTier::Fast => "long_fast",
        PricingTier::Flex => "long_flex",
        PricingTier::Ultrafast => "long_ultrafast",
    };
    let uses_long = long_context
        && (default.is_some_and(|p| p.long_standard.is_configured() || p.unpriced_long_context)
            || custom.is_some_and(|p| p.bands.contains_key(long_band)));
    let band = match (tier, uses_long) {
        (PricingTier::Standard, false) => "standard",
        (PricingTier::Fast, false) => "fast",
        (PricingTier::Flex, false) => "flex",
        (PricingTier::Ultrafast, false) => "ultrafast",
        (PricingTier::Standard, true) => "long_standard",
        (PricingTier::Fast, true) => "long_fast",
        (PricingTier::Flex, true) => "long_flex",
        (PricingTier::Ultrafast, true) => "long_ultrafast",
    };
    if let Some(rates) = custom.and_then(|p| p.bands.get(band)) {
        return Some((
            TokenRates {
                input_ticks: rates.input.ticks_per_token(),
                output_ticks: rates.output.ticks_per_token(),
                cache_read_ticks: rates.cache_read.ticks_per_token(),
                cache_write_ticks: Some(rates.cache_write.ticks_per_token()),
                explicit_cache: true,
            },
            uses_long,
        ));
    }
    let default = default?;
    Some((
        default.rates(tier, long_context)?,
        long_context && default.long_standard.is_configured(),
    ))
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
        // 这两个模型将非预览搜索内容按固定 8K 输入块计费
        // 响应缺少足够明细，无法排除与普通输入重复计费，因此保留为未定价
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
    let model = pricing_model_name(&normalized);
    model.starts_with("gpt-5")
        || matches!(
            model,
            "gpt-6.2-astra"
                | "gpt-6.2-sol"
                | "gpt-6-astra"
                | "gpt-6.1-sol"
                | "gpt-6.1-terra"
                | "gpt-6-sol"
                | "gpt-6-luna"
        )
        || model.starts_with("o1")
        || model.starts_with("o3")
        || model.starts_with("o4")
}

fn pricing_tier(service_tier: Option<&str>) -> Option<PricingTier> {
    match service_tier {
        None | Some("default" | "standard") => Some(PricingTier::Standard),
        Some("flex") => Some(PricingTier::Flex),
        Some("fast" | "priority") => Some(PricingTier::Fast),
        Some("ultrafast") => Some(PricingTier::Ultrafast),
        Some(_) => None,
    }
}

fn web_search_amount_ticks(usage: OpenAiBillingUsage) -> Option<u128> {
    if usage.web_search_calls == 0 {
        return Some(0);
    }
    u128::from(usage.web_search_calls).checked_mul(usage.web_search_pricing?.price_per_call_ticks())
}

pub(super) fn token_amounts(
    rates: TokenRates,
    cache_write_percent: u32,
    input_tokens: u64,
    output_tokens: u64,
    cached_input_tokens: u64,
    cache_write_input_tokens: u64,
) -> Option<TokenAmounts> {
    if cached_input_tokens.checked_add(cache_write_input_tokens)? > input_tokens {
        return None;
    }
    let billed_cache_read = if rates.explicit_cache || rates.cache_read_ticks > 0 {
        cached_input_tokens
    } else {
        0
    };
    let cache_write_rate = cache_write_rate(rates, cache_write_percent)?;
    let billed_cache_write = if rates.explicit_cache || cache_write_rate > 0 {
        cache_write_input_tokens
    } else {
        0
    };
    let uncached_input = input_tokens
        .checked_sub(billed_cache_read)?
        .checked_sub(billed_cache_write)?;
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

/// 规范化请求或响应携带的服务档位，供观测与计费共用
pub(crate) fn normalize_service_tier(service_tier: Option<&str>) -> Option<String> {
    service_tier
        .map(str::trim)
        .filter(|value| {
            !value.is_empty() && value.len() <= 64 && !value.chars().any(char::is_control)
        })
        .map(str::to_ascii_lowercase)
}

pub(super) fn usd_money(ticks: u128) -> Option<Money> {
    Some(Money::new(
        Decimal::from_scaled(ticks).ok()?,
        CurrencyCode::new("USD").ok()?,
    ))
}

pub(super) fn usd_price_per_million(per_token_ticks: u128) -> Option<Money> {
    usd_money(per_token_ticks.checked_mul(1_000_000)?)
}
