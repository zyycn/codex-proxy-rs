use gateway_core::accounting::{CalculatedCostBreakdown, CurrencyCode, Decimal, Money};
use gateway_protocol::openai::events::retry_after_seconds_from_body;
use reqwest::StatusCode;
use serde_json::Value;

use super::{
    CodexBackendClient, CodexClientError, CodexClientResult, CodexRequestContext,
    CodexUpstreamDiagnostics,
    client::{read_capped_response_body, retry_after_seconds, truncate_for_error},
    endpoints::usage_endpoint_urls,
    response_meta,
};

const LONG_CONTEXT_THRESHOLD: u64 = 272_000;

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
    priority: TokenRates,
    long: TokenRates,
    long_priority: TokenRates,
    cache_write_percent: u32,
}

impl ModelPricing {
    const fn new(input: u128, output: u128, cache_read: u128) -> Self {
        Self {
            standard: TokenRates::new(input, output, cache_read),
            priority: TokenRates::ZERO,
            long: TokenRates::ZERO,
            long_priority: TokenRates::ZERO,
            cache_write_percent: 0,
        }
    }

    const fn with_priority(mut self, input: u128, output: u128, cache_read: u128) -> Self {
        self.priority = TokenRates::new(input, output, cache_read);
        self
    }

    const fn with_long_context(
        mut self,
        input: u128,
        output: u128,
        cache_read: u128,
        priority_input: u128,
        priority_output: u128,
        priority_cache_read: u128,
    ) -> Self {
        self.long = TokenRates::new(input, output, cache_read);
        self.long_priority = TokenRates::new(priority_input, priority_output, priority_cache_read);
        self
    }

    const fn with_cache_write(mut self, percent: u32) -> Self {
        self.cache_write_percent = percent;
        self
    }
}

#[derive(Clone, Copy)]
struct PricingRule {
    model: &'static str,
    pricing: ModelPricing,
}

const PRICING_RULES: &[PricingRule] = &[
    PricingRule {
        model: "gpt-5.6-sol",
        pricing: ModelPricing::new(50_000, 300_000, 5_000)
            .with_cache_write(125)
            .with_priority(100_000, 600_000, 10_000),
    },
    PricingRule {
        model: "gpt-5.6-terra",
        pricing: ModelPricing::new(25_000, 150_000, 2_500)
            .with_cache_write(125)
            .with_priority(50_000, 300_000, 5_000),
    },
    PricingRule {
        model: "gpt-5.6-luna",
        pricing: ModelPricing::new(10_000, 60_000, 1_000)
            .with_cache_write(125)
            .with_priority(20_000, 120_000, 2_000),
    },
    PricingRule {
        model: "gpt-5.6",
        pricing: ModelPricing::new(50_000, 300_000, 5_000)
            .with_cache_write(125)
            .with_priority(100_000, 600_000, 10_000),
    },
    PricingRule {
        model: "gpt-5.5-pro",
        pricing: ModelPricing::new(300_000, 1_800_000, 0)
            .with_priority(750_000, 4_500_000, 0)
            .with_long_context(600_000, 2_700_000, 0, 1_500_000, 6_750_000, 0),
    },
    PricingRule {
        model: "gpt-5.5",
        pricing: ModelPricing::new(50_000, 300_000, 5_000)
            .with_priority(125_000, 750_000, 12_500)
            .with_long_context(100_000, 450_000, 10_000, 250_000, 1_125_000, 25_000),
    },
    PricingRule {
        model: "gpt-5.4-mini",
        pricing: ModelPricing::new(7_500, 45_000, 750),
    },
    PricingRule {
        model: "gpt-5.4-nano",
        pricing: ModelPricing::new(2_000, 12_500, 200),
    },
    PricingRule {
        model: "gpt-5.4-pro",
        pricing: ModelPricing::new(300_000, 1_800_000, 0)
            .with_priority(750_000, 4_500_000, 0)
            .with_long_context(600_000, 2_700_000, 0, 1_500_000, 6_750_000, 0),
    },
    PricingRule {
        model: "gpt-5.4",
        pricing: ModelPricing::new(25_000, 150_000, 2_500)
            .with_priority(50_000, 300_000, 5_000)
            .with_long_context(50_000, 225_000, 5_000, 100_000, 450_000, 10_000),
    },
    PricingRule {
        model: "gpt-5.3-codex-spark",
        pricing: ModelPricing::new(12_500, 100_000, 1_250).with_priority(25_000, 200_000, 2_500),
    },
    PricingRule {
        model: "gpt-5.3-codex",
        pricing: ModelPricing::new(17_500, 140_000, 1_750).with_priority(35_000, 280_000, 3_500),
    },
    PricingRule {
        model: "gpt-5.2",
        pricing: ModelPricing::new(17_500, 140_000, 1_750).with_priority(35_000, 280_000, 3_500),
    },
    PricingRule {
        model: "gpt-4o-mini",
        pricing: ModelPricing::new(1_500, 6_000, 0),
    },
    PricingRule {
        model: "gpt-4o",
        pricing: ModelPricing::new(25_000, 100_000, 0),
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
        model: "gpt-3.5-turbo",
        pricing: ModelPricing::new(5_000, 15_000, 0),
    },
];

/// 按 OpenAI Provider 当前受控价格规则计算费用明细。
#[must_use]
pub fn openai_billing_breakdown(
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    cache_write_tokens: u64,
    service_tier: Option<&str>,
) -> Option<CalculatedCostBreakdown> {
    if cached_tokens > input_tokens {
        return None;
    }
    let pricing = model_pricing(model)?;
    let long_context = input_tokens > LONG_CONTEXT_THRESHOLD;
    let normalized_tier = normalize_service_tier(service_tier);
    let priority = matches!(normalized_tier.as_deref(), Some("priority" | "fast"));
    let mut rates = if long_context && pricing.long.is_configured() {
        pricing.long
    } else {
        pricing.standard
    };
    let multiplier_percent = if priority {
        let priority_rates = if long_context && pricing.long_priority.is_configured() {
            pricing.long_priority
        } else {
            pricing.priority
        };
        if priority_rates.is_configured() {
            rates = priority_rates;
            100
        } else {
            200
        }
    } else if normalized_tier.as_deref() == Some("flex") {
        50
    } else {
        100
    };

    let billed_cache_read = if rates.cache_read_ticks > 0 {
        cached_tokens.min(input_tokens)
    } else {
        0
    };
    let cache_write_rate = if pricing.cache_write_percent > 0 {
        apply_percent(rates.input_ticks, pricing.cache_write_percent)?
    } else {
        0
    };
    let billed_cache_write = if cache_write_rate > 0 {
        cache_write_tokens.min(input_tokens.saturating_sub(billed_cache_read))
    } else {
        0
    };
    let uncached_input = input_tokens
        .saturating_sub(billed_cache_read)
        .saturating_sub(billed_cache_write);
    let input_amount = u128::from(uncached_input).checked_mul(rates.input_ticks)?;
    let output_amount = u128::from(output_tokens).checked_mul(rates.output_ticks)?;
    let cache_read_amount = u128::from(billed_cache_read).checked_mul(rates.cache_read_ticks)?;
    let cache_write_amount = u128::from(billed_cache_write).checked_mul(cache_write_rate)?;
    let standard_amount = input_amount
        .checked_add(output_amount)?
        .checked_add(cache_read_amount)?
        .checked_add(cache_write_amount)?;
    let total_amount = apply_percent(standard_amount, multiplier_percent)?;

    Some(CalculatedCostBreakdown::new(
        usd_money(input_amount)?,
        usd_money(output_amount)?,
        usd_money(cache_read_amount)?,
        usd_money(cache_write_amount)?,
        usd_money(standard_amount)?,
        usd_money(total_amount)?,
        usd_price_per_million(rates.input_ticks)?,
        usd_price_per_million(rates.output_ticks)?,
        usd_price_per_million(rates.cache_read_ticks)?,
        usd_price_per_million(cache_write_rate)?,
        Some(normalized_tier.unwrap_or_else(|| "default".to_owned())),
        multiplier_percent,
    ))
}

fn model_pricing(model: &str) -> Option<ModelPricing> {
    let normalized = normalize_model_name(model);
    PRICING_RULES
        .iter()
        .filter(|rule| model_matches_rule(&normalized, rule.model))
        .max_by_key(|rule| rule.model.len())
        .map(|rule| rule.pricing)
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

fn normalize_service_tier(service_tier: Option<&str>) -> Option<String> {
    service_tier
        .map(str::trim)
        .filter(|value| !value.is_empty())
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
        let mut last_invalid_body = None;

        for endpoint in usage_endpoint_urls(&self.base_url) {
            let headers = self.usage_request_headers(context)?;
            let response = self.client.get(endpoint).headers(headers).send().await?;
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
                    diagnostics: Box::new(diagnostics),
                    set_cookie_headers: Vec::new(),
                    rate_limit_headers: Vec::new(),
                    transport: super::client::CodexBackendTransport::HttpSse,
                });
            }
            let body = body.into_string();

            if status == StatusCode::NOT_FOUND {
                last_invalid_body = Some(body);
                continue;
            }
            if !status.is_success() {
                return Err(CodexClientError::Upstream {
                    status,
                    retry_after_seconds: retry_after_seconds
                        .or_else(|| retry_after_seconds_from_body(&body)),
                    body,
                    diagnostics: Box::new(diagnostics),
                    set_cookie_headers: Vec::new(),
                    rate_limit_headers: Vec::new(),
                    transport: super::client::CodexBackendTransport::HttpSse,
                });
            }

            match serde_json::from_str::<Value>(&body) {
                Ok(parsed) if is_usage_response(&parsed) => return Ok(parsed),
                _ => last_invalid_body = Some(body),
            }
        }

        Err(CodexClientError::Upstream {
            status: StatusCode::BAD_GATEWAY,
            retry_after_seconds: None,
            body: last_invalid_body.map_or_else(
                || "usage endpoint is unavailable".to_string(),
                |body| format!("invalid usage response: {}", truncate_for_error(&body)),
            ),
            diagnostics: Box::new(CodexUpstreamDiagnostics::with_status(
                StatusCode::BAD_GATEWAY.as_u16(),
            )),
            set_cookie_headers: Vec::new(),
            rate_limit_headers: Vec::new(),
            transport: super::client::CodexBackendTransport::HttpSse,
        })
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
