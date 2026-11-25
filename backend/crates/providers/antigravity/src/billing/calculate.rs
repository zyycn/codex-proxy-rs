//! 使用公开 API 参考价与请求冻结的配置估算费用；不将订阅额度解释为现金账单

use super::catalog::defaults;

use gateway_core::metering::{
    CalculatedCostAmounts, CalculatedCostBreakdown, CalculatedCostRates, CurrencyCode, Decimal,
    ImageCostBreakdown, ModelPriceOverride, Money, TokenPriceOverride, Usage,
};

pub(crate) fn calculate(
    model: &str,
    usage: &Usage,
    pricing: Option<&ModelPriceOverride>,
) -> Option<CalculatedCostBreakdown> {
    let builtin = defaults().get(model);
    let band = |name| {
        pricing
            .and_then(|p| p.bands.get(name))
            .or_else(|| builtin.and_then(|p| p.bands.get(name)))
    };
    let input = usage.input_tokens?;
    let output = usage.output_tokens?;
    let long = input > 200_000 && band("long_standard").is_some();
    let rates = band(if long { "long_standard" } else { "standard" })?;
    let image_rates = band("image");
    // 图片模型必须报告输出模态数量，避免把图片误按文本单价计费
    let image_output = if image_rates.is_some() {
        usage.image_output_tokens?
    } else {
        usage.image_output_tokens.unwrap_or_default()
    };
    let text_output = output.checked_sub(image_output)?;
    let output_ticks = u128::from(text_output)
        .checked_mul(rates.output.ticks_per_token())?
        .checked_add(u128::from(image_output).checked_mul(if image_output == 0 {
            0
        } else {
            image_rates?.output.ticks_per_token()
        })?)?;
    let (input, cached, image) = split_image_input(usage, rates, image_rates)?;
    let written = usage.cache_write_input_tokens.unwrap_or_default();
    let uncached = input.checked_sub(cached)?.checked_sub(written)?;
    // 原生用量已经把 thoughts 合入 output；缓存也是 input 的子集，不能重复收费
    let input_amount = usd(u128::from(uncached).checked_mul(rates.input.ticks_per_token())?)?;
    let output_amount = usd(output_ticks)?;
    let cache_read_amount =
        usd(u128::from(cached).checked_mul(rates.cache_read.ticks_per_token())?)?;
    let cache_write_amount =
        usd(u128::from(written).checked_mul(rates.cache_write.ticks_per_token())?)?;
    let mut total = input_amount
        .checked_add(output_amount)?
        .checked_add(cache_read_amount)?
        .checked_add(cache_write_amount)?;
    if let Some(image) = &image {
        total = total
            .checked_add(image.input_amount)?
            .checked_add(image.cache_read_amount)?;
    }
    let mut breakdown = CalculatedCostBreakdown::new(
        CalculatedCostAmounts::new(
            input_amount,
            output_amount,
            cache_read_amount,
            cache_write_amount,
            total,
            total,
        ),
        CalculatedCostRates::new(
            usd(rates.input.ticks_per_token().checked_mul(1_000_000)?)?,
            // 混合输出展示加权单价，费用直接按各模态整数 Token 计算
            usd(if output == 0 {
                rates.output.ticks_per_token().checked_mul(1_000_000)?
            } else {
                output_ticks
                    .checked_mul(1_000_000)?
                    .checked_div(u128::from(output))?
            })?,
            usd(rates.cache_read.ticks_per_token().checked_mul(1_000_000)?)?,
            usd(rates.cache_write.ticks_per_token().checked_mul(1_000_000)?)?,
        ),
        None,
        100,
    )
    .with_long_context_billing(long);
    if let Some(image) = image {
        breakdown = breakdown.with_image(image);
    }
    breakdown.with_custom_multiplier(pricing.map_or(10_000, |p| p.multiplier_bps))
}

/// 输入模态同价时直接计总量；异价且缓存只给总量时，只接受能唯一拆分的情况
fn split_image_input(
    usage: &Usage,
    text: &TokenPriceOverride,
    image: Option<&TokenPriceOverride>,
) -> Option<(u64, u64, Option<ImageCostBreakdown>)> {
    let input = usage.input_tokens?;
    let cached = usage.cached_input_tokens.unwrap_or_default();
    let Some(image) = image.filter(|image| {
        image.input != text.input
            || image.cache_read != text.cache_read
            || image.cache_write != text.cache_write
    }) else {
        return Some((input, cached, None));
    };
    let image_input = usage.image_input_tokens?;
    let text_input = input.checked_sub(image_input)?;
    if image_input == 0 {
        return Some((input, cached, None));
    }
    if usage.cache_write_input_tokens.unwrap_or_default() != 0 {
        return None;
    }
    let image_cached = match (text_input, cached) {
        (_, 0) => 0,
        (0, cached) => cached,
        (_, cached) if cached == input => image_input,
        _ => return None,
    };
    Some((
        text_input,
        cached.checked_sub(image_cached)?,
        Some(ImageCostBreakdown {
            input_tokens: image_input,
            cached_input_tokens: image_cached,
            input_amount: usd(u128::from(image_input.checked_sub(image_cached)?)
                .checked_mul(image.input.ticks_per_token())?)?,
            cache_read_amount: usd(
                u128::from(image_cached).checked_mul(image.cache_read.ticks_per_token())?
            )?,
            input_price_per_million: usd(image.input.ticks_per_token().checked_mul(1_000_000)?)?,
            cache_read_price_per_million: usd(image
                .cache_read
                .ticks_per_token()
                .checked_mul(1_000_000)?)?,
        }),
    ))
}

fn usd(ticks: u128) -> Option<Money> {
    Some(Money::new(
        Decimal::from_scaled(ticks).ok()?,
        CurrencyCode::new("USD").ok()?,
    ))
}

/// 在请求开始时冻结内置、同步和人工价，随后不再读取可变设置
pub(crate) fn effective_pricing(
    model: &str,
    custom: Option<&ModelPriceOverride>,
) -> Option<ModelPriceOverride> {
    let mut result = defaults().get(model).cloned().or_else(|| custom.cloned())?;
    if let Some(custom) = custom {
        result.bands.extend(custom.bands.clone());
        result.multiplier_bps = custom.multiplier_bps;
    }
    Some(result)
}

/// 现有用量合同没有音频子计数；含音视频的请求不能按文本参考单价估算
pub(crate) fn supports_pricing(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(fields) => supports_pricing_fields(fields),
        serde_json::Value::Array(values) => values.iter().all(supports_pricing),
        _ => true,
    }
}

pub(crate) fn supports_pricing_fields(fields: &serde_json::Map<String, serde_json::Value>) -> bool {
    fields.iter().all(|(key, value)| {
        if matches!(key.as_str(), "mimeType" | "mime_type")
            && value
                .as_str()
                .is_some_and(|mime| mime.starts_with("audio/") || mime.starts_with("video/"))
        {
            return false;
        }
        if key == "modality" && value.as_str() == Some("AUDIO") {
            return false;
        }
        supports_pricing(value)
    })
}
