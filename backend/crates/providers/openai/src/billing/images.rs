//! OpenAI Images 用量的模态校验与费用计算

use gateway_core::metering::{
    CalculatedCost, CalculatedCostAmounts, CalculatedCostBreakdown, CalculatedCostRates,
};
use serde_json::Value;

use super::calculate::{token_amounts, usd_money, usd_price_per_million};
use super::catalog::{IMAGE_MODELS, IMAGE_TEXT_RATES, IMAGE_TOKEN_RATES, TokenRates};

/// 独立 Images 端点按公开标准 API 单价估算；不是 ChatGPT 账号实际扣费
pub(crate) fn image_calculated_cost(
    request_body: &[u8],
    usage: &Value,
    prices: &gateway_core::metering::PricingOverrides,
) -> Option<CalculatedCost> {
    #[derive(serde::Deserialize)]
    struct ImageModel {
        model: String,
    }
    // 只读取模型，跳过编辑请求中可能很大的 base64 图片
    let request = serde_json::from_slice::<ImageModel>(request_body).ok()?;
    let custom = prices
        .get("openai")
        .and_then(|models| models.get(&request.model));
    let known = IMAGE_MODELS.contains(&request.model.as_str());
    let rates = |band, builtin| {
        custom
            .and_then(|p| p.bands.get(band))
            .map(|p| TokenRates {
                input_ticks: p.input.ticks_per_token(),
                output_ticks: p.output.ticks_per_token(),
                cache_read_ticks: p.cache_read.ticks_per_token(),
                cache_write_ticks: Some(p.cache_write.ticks_per_token()),
                explicit_cache: true,
            })
            .or_else(|| known.then_some(builtin))
    };
    let text_rates = rates("standard", IMAGE_TEXT_RATES)?;
    let image_rates = rates("image", IMAGE_TOKEN_RATES)?;
    let input = usage.get("input_tokens")?.as_u64()?;
    let output = usage.get("output_tokens")?.as_u64()?;
    let details = usage.get("input_tokens_details")?;
    let text_input = details.get("text_tokens")?.as_u64()?;
    let image_input = details.get("image_tokens")?.as_u64()?;
    if text_input.checked_add(image_input)? != input {
        return None;
    }
    if let Some(total) = usage.get("total_tokens")
        && total.as_u64()? != input.checked_add(output)?
    {
        return None;
    }
    // 此模型仅输出图片；上游如报告其他输出模态，不能套用图片单价
    if let Some(details) = usage.get("output_tokens_details")
        && (details.get("image_tokens")?.as_u64()? != output
            || details.get("text_tokens")?.as_u64()? != 0)
    {
        return None;
    }
    let cached = match details.get("cached_tokens") {
        None => 0,
        Some(value) => value.as_u64()?,
    };
    if cached > input {
        return None;
    }
    // 缓存只给总量时，混合输入无法判定应套用哪种缓存单价
    let (cached_text, cached_image) = match (text_input, image_input, cached) {
        (_, _, 0) => (0, 0),
        (_, 0, cached) => (cached, 0),
        (0, _, cached) => (0, cached),
        (_, _, cached) if cached == input => (text_input, image_input),
        _ => return None,
    };
    // 价格来源：https://developers.openai.com/api/docs/pricing，核验日期 2026-09-09
    // GPT Image 2 和两个 2.5 型号按相同的 token 单价计费，不使用按张估价
    // 每百万 token 的美元单价：文本 5 / 缓存 1.25；图片 8 / 缓存 2 / 输出 30
    let text = token_amounts(text_rates, 0, text_input, 0, cached_text, 0)?;
    let image = token_amounts(image_rates, 0, image_input, output, cached_image, 0)?;
    let total = usd_money(text.total_ticks.checked_add(image.total_ticks)?)?;
    let breakdown = CalculatedCostBreakdown::new(
        CalculatedCostAmounts::new(
            usd_money(text.input_ticks)?,
            usd_money(image.output_ticks)?,
            usd_money(text.cache_read_ticks)?,
            usd_money(0)?,
            total,
            total,
        ),
        CalculatedCostRates::new(
            usd_price_per_million(text_rates.input_ticks)?,
            usd_price_per_million(image_rates.output_ticks)?,
            usd_price_per_million(text_rates.cache_read_ticks)?,
            usd_money(0)?,
        ),
        Some("default".to_owned()),
        100,
    )
    .with_image(gateway_core::metering::ImageCostBreakdown {
        input_tokens: image_input,
        cached_input_tokens: cached_image,
        input_amount: usd_money(image.input_ticks)?,
        cache_read_amount: usd_money(image.cache_read_ticks)?,
        input_price_per_million: usd_price_per_million(image_rates.input_ticks)?,
        cache_read_price_per_million: usd_price_per_million(image_rates.cache_read_ticks)?,
    })
    .with_custom_multiplier(custom.map_or(10_000, |p| p.multiplier_bps))?;
    Some(breakdown.calculated_cost())
}
