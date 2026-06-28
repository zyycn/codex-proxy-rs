//! 管理端用量计费规则。

const LONG_CONTEXT_THRESHOLD: u64 = 272_000;

#[derive(Debug, Clone, Copy)]
struct ModelPricing {
    input_price_per_mtoken: f64,
    input_price_per_mtoken_priority: f64,
    output_price_per_mtoken: f64,
    output_price_per_mtoken_priority: f64,
    cache_read_price_per_mtoken: f64,
    cache_read_price_per_mtoken_priority: f64,
    long_input_price_per_mtoken: f64,
    long_input_price_per_mtoken_priority: f64,
    long_output_price_per_mtoken: f64,
    long_output_price_per_mtoken_priority: f64,
    long_cache_read_price_per_mtoken: f64,
    long_cache_read_price_per_mtoken_priority: f64,
}

impl ModelPricing {
    const fn new(input: f64, output: f64) -> Self {
        Self {
            input_price_per_mtoken: input,
            input_price_per_mtoken_priority: 0.0,
            output_price_per_mtoken: output,
            output_price_per_mtoken_priority: 0.0,
            cache_read_price_per_mtoken: 0.0,
            cache_read_price_per_mtoken_priority: 0.0,
            long_input_price_per_mtoken: 0.0,
            long_input_price_per_mtoken_priority: 0.0,
            long_output_price_per_mtoken: 0.0,
            long_output_price_per_mtoken_priority: 0.0,
            long_cache_read_price_per_mtoken: 0.0,
            long_cache_read_price_per_mtoken_priority: 0.0,
        }
    }

    const fn with_cache(mut self, cache_read: f64) -> Self {
        self.cache_read_price_per_mtoken = cache_read;
        self
    }

    const fn with_priority(mut self, input: f64, output: f64, cache_read: f64) -> Self {
        self.input_price_per_mtoken_priority = input;
        self.output_price_per_mtoken_priority = output;
        self.cache_read_price_per_mtoken_priority = cache_read;
        self
    }

    const fn with_long_context(
        mut self,
        input: f64,
        output: f64,
        cache_read: f64,
        priority_input: f64,
        priority_output: f64,
        priority_cache_read: f64,
    ) -> Self {
        self.long_input_price_per_mtoken = input;
        self.long_output_price_per_mtoken = output;
        self.long_cache_read_price_per_mtoken = cache_read;
        self.long_input_price_per_mtoken_priority = priority_input;
        self.long_output_price_per_mtoken_priority = priority_output;
        self.long_cache_read_price_per_mtoken_priority = priority_cache_read;
        self
    }
}

#[derive(Debug, Clone, Copy)]
struct ModelPricingRule {
    model: &'static str,
    pricing: ModelPricing,
}

const DEFAULT_MODEL_PRICING: ModelPricing = ModelPricing::new(1.0, 2.0);

const MODEL_PRICING_RULES: &[ModelPricingRule] = &[
    ModelPricingRule {
        model: "gpt-5.5",
        pricing: ModelPricing::new(5.0, 30.0)
            .with_cache(0.5)
            .with_priority(12.5, 75.0, 1.25)
            .with_long_context(10.0, 45.0, 1.0, 25.0, 112.5, 2.5),
    },
    ModelPricingRule {
        model: "gpt-5.5-pro",
        pricing: ModelPricing::new(30.0, 180.0)
            .with_priority(75.0, 450.0, 0.0)
            .with_long_context(60.0, 270.0, 0.0, 150.0, 675.0, 0.0),
    },
    ModelPricingRule {
        model: "gpt-5.4-mini",
        pricing: ModelPricing::new(0.75, 4.5).with_cache(0.075),
    },
    ModelPricingRule {
        model: "gpt-5.4-nano",
        pricing: ModelPricing::new(0.2, 1.25).with_cache(0.02),
    },
    ModelPricingRule {
        model: "gpt-5.4",
        pricing: ModelPricing::new(2.5, 15.0)
            .with_cache(0.25)
            .with_priority(5.0, 30.0, 0.5)
            .with_long_context(5.0, 22.5, 0.5, 10.0, 45.0, 1.0),
    },
    ModelPricingRule {
        model: "gpt-5.4-pro",
        pricing: ModelPricing::new(30.0, 180.0)
            .with_priority(75.0, 450.0, 0.0)
            .with_long_context(60.0, 270.0, 0.0, 150.0, 675.0, 0.0),
    },
    ModelPricingRule {
        model: "gpt-5.3-codex-spark",
        pricing: ModelPricing::new(1.25, 10.0)
            .with_cache(0.125)
            .with_priority(2.5, 20.0, 0.25),
    },
    ModelPricingRule {
        model: "gpt-5.3-codex",
        pricing: ModelPricing::new(1.75, 14.0)
            .with_cache(0.175)
            .with_priority(3.5, 28.0, 0.35),
    },
    ModelPricingRule {
        model: "gpt-5.2",
        pricing: ModelPricing::new(1.75, 14.0)
            .with_cache(0.175)
            .with_priority(3.5, 28.0, 0.35),
    },
    ModelPricingRule {
        model: "gpt-4o-mini",
        pricing: ModelPricing::new(0.15, 0.6),
    },
    ModelPricingRule {
        model: "gpt-4o",
        pricing: ModelPricing::new(2.5, 10.0),
    },
    ModelPricingRule {
        model: "gpt-4-turbo",
        pricing: ModelPricing::new(10.0, 30.0),
    },
    ModelPricingRule {
        model: "gpt-4",
        pricing: ModelPricing::new(30.0, 60.0),
    },
    ModelPricingRule {
        model: "gpt-3.5-turbo",
        pricing: ModelPricing::new(0.5, 1.5),
    },
];

/// 单次请求计费明细。
#[derive(Debug, Clone, PartialEq)]
pub(in crate::admin) struct CostBreakdown {
    /// 输入成本。
    pub input_cost: f64,
    /// 输出成本。
    pub output_cost: f64,
    /// 缓存读取成本。
    pub cache_read_cost: f64,
    /// 总成本。
    pub total_cost: f64,
    /// 输入单价，美元 / 1M token。
    pub input_price_per_mtoken: f64,
    /// 输出单价，美元 / 1M token。
    pub output_price_per_mtoken: f64,
    /// 缓存读取单价，美元 / 1M token。
    pub cache_read_price_per_mtoken: f64,
    /// 服务档位。
    pub service_tier: Option<String>,
    /// 服务档位倍率。
    pub tier_multiplier: f64,
}

/// 按 codex2api 的 billing 引擎口径计算美元计费明细。
pub(in crate::admin) fn calculate_cost_breakdown(
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    model: &str,
    service_tier: Option<&str>,
) -> CostBreakdown {
    let pricing = model_pricing(model);
    let is_long = input_tokens > LONG_CONTEXT_THRESHOLD;

    let mut input_price = pricing.input_price_per_mtoken;
    let mut output_price = pricing.output_price_per_mtoken;
    let mut cache_read_price = pricing.cache_read_price_per_mtoken;

    if is_long && pricing.long_input_price_per_mtoken > 0.0 {
        input_price = pricing.long_input_price_per_mtoken;
        output_price = pricing.long_output_price_per_mtoken;
        if pricing.long_cache_read_price_per_mtoken > 0.0 {
            cache_read_price = pricing.long_cache_read_price_per_mtoken;
        }
    }

    let mut tier_multiplier = service_tier_cost_multiplier(service_tier);
    if use_priority_pricing(service_tier, pricing) {
        tier_multiplier = 1.0;
        if is_long && pricing.long_input_price_per_mtoken_priority > 0.0 {
            input_price = pricing.long_input_price_per_mtoken_priority;
        } else if pricing.input_price_per_mtoken_priority > 0.0 {
            input_price = pricing.input_price_per_mtoken_priority;
        }
        if is_long && pricing.long_output_price_per_mtoken_priority > 0.0 {
            output_price = pricing.long_output_price_per_mtoken_priority;
        } else if pricing.output_price_per_mtoken_priority > 0.0 {
            output_price = pricing.output_price_per_mtoken_priority;
        }
        if is_long && pricing.long_cache_read_price_per_mtoken_priority > 0.0 {
            cache_read_price = pricing.long_cache_read_price_per_mtoken_priority;
        } else if pricing.cache_read_price_per_mtoken_priority > 0.0 {
            cache_read_price = pricing.cache_read_price_per_mtoken_priority;
        }
    }

    let cached_tokens = cached_tokens.min(input_tokens);
    let uncached_input_tokens = if cache_read_price > 0.0 {
        input_tokens - cached_tokens
    } else {
        input_tokens
    };
    let input_cost = uncached_input_tokens as f64 / 1_000_000.0 * input_price;
    let cache_read_cost = cached_tokens as f64 / 1_000_000.0 * cache_read_price;
    let output_cost = output_tokens as f64 / 1_000_000.0 * output_price;
    let total_cost = (input_cost + cache_read_cost + output_cost) * tier_multiplier;

    CostBreakdown {
        input_cost,
        output_cost,
        cache_read_cost,
        total_cost,
        input_price_per_mtoken: input_price,
        output_price_per_mtoken: output_price,
        cache_read_price_per_mtoken: cache_read_price,
        service_tier: normalize_service_tier(service_tier),
        tier_multiplier,
    }
}

/// 按 codex2api 的 billing 引擎口径计算美元计费。
pub(in crate::admin) fn calculate_cost(
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    model: &str,
    service_tier: Option<&str>,
) -> f64 {
    calculate_cost_breakdown(
        input_tokens,
        output_tokens,
        cached_tokens,
        model,
        service_tier,
    )
    .total_cost
}

fn model_pricing(model: &str) -> ModelPricing {
    let mut normalized = normalize_billing_model_name(model);
    if let Some(pricing) = claude_family_pricing(&normalized) {
        return pricing;
    }
    if let Some(pricing) = gemini_family_pricing(&normalized) {
        return pricing;
    }
    if let Some(codex_model) = normalize_codex_billing_model(&normalized) {
        normalized = codex_model.to_string();
    }
    model_rule_pricing(&normalized).unwrap_or(DEFAULT_MODEL_PRICING)
}

fn normalize_billing_model_name(model: &str) -> String {
    let mut model = model.trim().to_ascii_lowercase();
    model = model.trim_start_matches('/').to_string();
    model = model.strip_prefix("models/").unwrap_or(&model).to_string();
    model = model
        .strip_prefix("publishers/google/models/")
        .unwrap_or(&model)
        .to_string();
    if let Some(index) = model.rfind("/publishers/google/models/") {
        model = model[index + "/publishers/google/models/".len()..].to_string();
    } else if let Some(index) = model.rfind("/models/") {
        model = model[index + "/models/".len()..].to_string();
    } else if let Some(index) = model.rfind('/') {
        model = model[index + 1..].to_string();
    }
    model.trim_start_matches('/').to_string()
}

fn normalize_codex_billing_model(model: &str) -> Option<&'static str> {
    let compact = model.replace([' ', '_'], "-").to_ascii_lowercase();
    if compact.contains("gpt-5.5-pro")
        || compact.contains("gpt5-5-pro")
        || compact.contains("gpt5.5-pro")
    {
        Some("gpt-5.5-pro")
    } else if compact.contains("gpt-5.5")
        || compact.contains("gpt5-5")
        || compact.contains("gpt5.5")
    {
        Some("gpt-5.5")
    } else if compact.contains("gpt-5.4-mini")
        || compact.contains("gpt5-4-mini")
        || compact.contains("gpt5.4-mini")
    {
        Some("gpt-5.4-mini")
    } else if compact.contains("gpt-5.4-nano")
        || compact.contains("gpt5-4-nano")
        || compact.contains("gpt5.4-nano")
    {
        Some("gpt-5.4-nano")
    } else if compact.contains("gpt-5.4-pro")
        || compact.contains("gpt5-4-pro")
        || compact.contains("gpt5.4-pro")
    {
        Some("gpt-5.4-pro")
    } else if compact.contains("gpt-5.4")
        || compact.contains("gpt5-4")
        || compact.contains("gpt5.4")
    {
        Some("gpt-5.4")
    } else if compact.contains("gpt-5.2")
        || compact.contains("gpt5-2")
        || compact.contains("gpt5.2")
    {
        Some("gpt-5.2")
    } else if compact.contains("gpt-5.3-codex-spark")
        || compact.contains("gpt5-3-codex-spark")
        || compact.contains("gpt5.3-codex-spark")
    {
        Some("gpt-5.3-codex-spark")
    } else if compact.contains("gpt-5.3-codex")
        || compact.contains("gpt5-3-codex")
        || compact.contains("gpt5.3-codex")
        || compact.contains("gpt-5.3")
        || compact.contains("gpt5-3")
        || compact.contains("gpt5.3")
    {
        Some("gpt-5.3-codex")
    } else if compact.contains("codex-auto-review") {
        Some("gpt-5.4")
    } else if compact.contains("codex") {
        Some("gpt-5.3-codex")
    } else if compact.contains("gpt-5") || compact.contains("gpt5") {
        Some("gpt-5.4")
    } else {
        None
    }
}

fn model_rule_pricing(model: &str) -> Option<ModelPricing> {
    MODEL_PRICING_RULES
        .iter()
        .filter(|rule| model_matches_rule(model, rule.model))
        .max_by_key(|rule| rule.model.len())
        .map(|rule| rule.pricing)
}

fn model_matches_rule(model: &str, rule: &str) -> bool {
    if model == rule {
        return true;
    }
    let Some(rest) = model.strip_prefix(rule) else {
        return false;
    };
    rest.is_empty() || matches!(rest.as_bytes().first(), Some(b'-' | b'.' | b':'))
}

fn claude_family_pricing(model: &str) -> Option<ModelPricing> {
    if model.contains("opus") {
        if model.contains("4.7")
            || model.contains("4-7")
            || model.contains("4.6")
            || model.contains("4-6")
            || model.contains("4.5")
            || model.contains("4-5")
        {
            Some(ModelPricing::new(5.0, 25.0))
        } else {
            Some(ModelPricing::new(15.0, 75.0))
        }
    } else if model.contains("sonnet") {
        Some(ModelPricing::new(3.0, 15.0))
    } else if model.contains("haiku") {
        if model.contains("3-5") || model.contains("3.5") {
            Some(ModelPricing::new(1.0, 5.0))
        } else {
            Some(ModelPricing::new(0.25, 1.25))
        }
    } else if model.contains("claude") {
        Some(ModelPricing::new(3.0, 15.0))
    } else {
        None
    }
}

fn gemini_family_pricing(model: &str) -> Option<ModelPricing> {
    (model.contains("gemini-3.1-pro") || model.contains("gemini-3-1-pro"))
        .then_some(ModelPricing::new(2.0, 12.0))
}

fn use_priority_pricing(service_tier: Option<&str>, pricing: ModelPricing) -> bool {
    matches!(
        normalize_service_tier(service_tier).as_deref(),
        Some("priority" | "fast")
    ) && (pricing.input_price_per_mtoken_priority > 0.0
        || pricing.output_price_per_mtoken_priority > 0.0
        || pricing.cache_read_price_per_mtoken_priority > 0.0)
}

fn service_tier_cost_multiplier(service_tier: Option<&str>) -> f64 {
    match normalize_service_tier(service_tier).as_deref() {
        Some("priority" | "fast") => 2.0,
        Some("flex") => 0.5,
        _ => 1.0,
    }
}

fn normalize_service_tier(service_tier: Option<&str>) -> Option<String> {
    service_tier
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
}
