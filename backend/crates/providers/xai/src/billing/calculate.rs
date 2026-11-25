//! xAI Token、缓存与 Priority 档位的费用计算

use super::catalog::{TokenRates, model_pricing};
use gateway_core::metering::{
    CalculatedCostAmounts, CalculatedCostBreakdown, CalculatedCostRates, CurrencyCode, Decimal,
    Money,
};

const LONG_CONTEXT_THRESHOLD: u64 = 200_000;

/// 按响应确认的实际档位计算 xAI Token 费用；未知档位不估算
#[must_use]
pub(crate) fn grok_billing_breakdown_with_tier(
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    cached_input_tokens: u64,
    service_tier: Option<&str>,
) -> Option<CalculatedCostBreakdown> {
    grok_billing_breakdown_with_override(
        model,
        input_tokens,
        output_tokens,
        cached_input_tokens,
        0,
        service_tier,
        None,
    )
}

#[must_use]
pub(crate) fn grok_billing_breakdown_with_override(
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    cached_input_tokens: u64,
    cache_write_input_tokens: u64,
    service_tier: Option<&str>,
    custom: Option<&gateway_core::metering::ModelPriceOverride>,
) -> Option<CalculatedCostBreakdown> {
    let (tier, multiplier) = match service_tier.map(str::trim) {
        None | Some("default" | "standard") => ("default", 1_u128),
        Some("priority") => ("priority", 2),
        Some(_) => return None,
    };
    let long = input_tokens >= LONG_CONTEXT_THRESHOLD;
    let standard_band = if long { "long_standard" } else { "standard" };
    let fast_band = if long { "long_fast" } else { "fast" };
    let convert = |rates: &gateway_core::metering::TokenPriceOverride| TokenRates {
        input_ticks: rates.input.ticks_per_token(),
        cached_input_ticks: rates.cache_read.ticks_per_token(),
        output_ticks: rates.output.ticks_per_token(),
    };
    let standard = custom
        .and_then(|p| p.bands.get(standard_band))
        .map(convert)
        .or_else(|| model_pricing(model).map(|p| if long { p.long } else { p.short }))?;
    let rates = if multiplier == 2 {
        custom
            .and_then(|p| p.bands.get(fast_band))
            .map(convert)
            .or_else(|| {
                // 未覆盖档位继承内置价，不把人工标准价再次解释成 Priority 价
                let builtin = model_pricing(model).map(|p| if long { p.long } else { p.short })?;
                Some(TokenRates {
                    input_ticks: builtin.input_ticks.checked_mul(2)?,
                    cached_input_ticks: builtin.cached_input_ticks.checked_mul(2)?,
                    output_ticks: builtin.output_ticks.checked_mul(2)?,
                })
            })?
    } else {
        standard
    };
    if cached_input_tokens.checked_add(cache_write_input_tokens)? > input_tokens {
        return None;
    }
    let standard_write = custom
        .and_then(|p| p.bands.get(standard_band))
        .map(|p| p.cache_write.ticks_per_token());
    let selected_band = if multiplier == 2 {
        fast_band
    } else {
        standard_band
    };
    let selected_write = custom
        .and_then(|p| p.bands.get(selected_band))
        .map(|p| p.cache_write.ticks_per_token());
    let uncached_tokens = input_tokens.checked_sub(cached_input_tokens)?.checked_sub(
        if selected_write.is_some() {
            cache_write_input_tokens
        } else {
            0
        },
    )?;
    let standard_input_tokens = input_tokens.checked_sub(cached_input_tokens)?.checked_sub(
        if standard_write.is_some() {
            cache_write_input_tokens
        } else {
            0
        },
    )?;
    let cache_write_amount_ticks =
        u128::from(cache_write_input_tokens).checked_mul(selected_write.unwrap_or_default())?;
    let input_amount_ticks = u128::from(uncached_tokens).checked_mul(rates.input_ticks)?;
    let cache_read_amount_ticks =
        u128::from(cached_input_tokens).checked_mul(rates.cached_input_ticks)?;
    let output_amount_ticks = u128::from(output_tokens).checked_mul(rates.output_ticks)?;
    let selected_amount_ticks = input_amount_ticks
        .checked_add(cache_read_amount_ticks)?
        .checked_add(cache_write_amount_ticks)?
        .checked_add(output_amount_ticks)?;
    let standard_amount_ticks = u128::from(standard_input_tokens)
        .checked_mul(standard.input_ticks)?
        .checked_add(u128::from(cached_input_tokens).checked_mul(standard.cached_input_ticks)?)?
        .checked_add(
            u128::from(cache_write_input_tokens).checked_mul(standard_write.unwrap_or_default())?,
        )?
        .checked_add(u128::from(output_tokens).checked_mul(standard.output_ticks)?)?;
    let multiplier_percent = if standard_amount_ticks == 0 {
        100
    } else {
        u32::try_from(
            selected_amount_ticks
                .checked_mul(100)?
                .checked_add(standard_amount_ticks / 2)?
                .checked_div(standard_amount_ticks)?,
        )
        .ok()?
    };
    CalculatedCostBreakdown::new(
        CalculatedCostAmounts::new(
            usd_money(input_amount_ticks)?,
            usd_money(output_amount_ticks)?,
            usd_money(cache_read_amount_ticks)?,
            usd_money(cache_write_amount_ticks)?,
            usd_money(standard_amount_ticks)?,
            usd_money(selected_amount_ticks)?,
        ),
        CalculatedCostRates::new(
            usd_price_per_million(rates.input_ticks)?,
            usd_price_per_million(rates.output_ticks)?,
            usd_price_per_million(rates.cached_input_ticks)?,
            usd_price_per_million(selected_write.unwrap_or_default())?,
        ),
        Some(tier.to_owned()),
        multiplier_percent,
    )
    .with_long_context_billing(long)
    .with_custom_multiplier(custom.map_or(10_000, |p| p.multiplier_bps))
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
