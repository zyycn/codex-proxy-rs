//! 复用 Provider 的单请求 Token 计价规则，不写入历史或账本

use super::{DefaultUsageDataService, validation};
use crate::{
    model::usage_data::*,
    ports::provider::{ProviderAdminErrorKind, ProviderUsagePricingInput},
};
use gateway_core::{
    metering::{PricingOverrides, Usage},
    routing::ProviderKind,
};

impl DefaultUsageDataService {
    pub(super) fn evaluate_one(
        &self,
        input: UsagePricingInput,
        prices: &PricingOverrides,
    ) -> UsageDataResult<UsagePricingResult> {
        validation::text(&input.provider, 128)?;
        validation::text(&input.model, 128)?;
        validation::tokens(&input.tokens)?;
        let unavailable = |reason: &str| UsagePricingResult::NotEstimable {
            id: input.id.clone(),
            reason: reason.into(),
        };
        let kind = ProviderKind::new(input.provider.clone())
            .map_err(|_| UsageDataError::invalid("Provider 无效"))?;
        let Ok(provider) = self.providers.require(&kind) else {
            return Ok(unavailable("Provider 没有可用的计价规则"));
        };
        let tokens = &input.tokens;
        let price = match &input.price {
            UsagePriceSelection::Current => prices
                .get(&input.provider)
                .and_then(|models| models.get(&input.model)),
            UsagePriceSelection::Override { price } => {
                price.validate().map_err(UsageDataError::invalid)?;
                Some(price.as_ref())
            }
        };
        let request = ProviderUsagePricingInput {
            upstream_model_id: input.model.clone(),
            service_tier: input.service_tier.clone(),
            usage: Usage {
                input_tokens: tokens.input_tokens.map(UsageCount::get),
                output_tokens: tokens.output_tokens.map(UsageCount::get),
                cached_input_tokens: tokens.cached_input_tokens.map(UsageCount::get),
                cache_write_input_tokens: tokens.cache_write_input_tokens.map(UsageCount::get),
                reasoning_output_tokens: tokens.reasoning_output_tokens.map(UsageCount::get),
                image_input_tokens: tokens.image_input_tokens.map(UsageCount::get),
                image_output_tokens: tokens.image_output_tokens.map(UsageCount::get),
                total_tokens: tokens.total_tokens.map(UsageCount::get),
            },
        };
        let calculated = match provider.evaluate_usage_pricing(&request, price) {
            Ok(Some(value)) => value,
            Ok(None) => return Ok(unavailable("用量不完整、包含图像或价格不可用")),
            Err(error)
                if matches!(
                    error.kind(),
                    ProviderAdminErrorKind::Unsupported | ProviderAdminErrorKind::Invalid
                ) =>
            {
                return Ok(unavailable("Provider 不支持该计价输入"));
            }
            Err(_) => return Err(UsageDataError::unavailable("Provider 计价失败")),
        };
        Ok(UsagePricingResult::Estimated {
            id: input.id,
            cost: UsageCost::Known {
                source: UsageCostSource::Calculated,
                amount: calculated.total_amount().amount().canonical(),
                currency: calculated.total_amount().currency().as_str().into(),
            },
            breakdown: UsagePricingBreakdown {
                input_amount: calculated.input_amount().amount().canonical(),
                output_amount: calculated.output_amount().amount().canonical(),
                cache_read_amount: calculated.cache_read_amount().amount().canonical(),
                cache_write_amount: calculated.cache_write_amount().amount().canonical(),
                standard_amount: calculated.standard_amount().amount().canonical(),
                long_context_billing_applied: calculated.long_context_billing_applied(),
                service_tier: calculated.service_tier().map(str::to_owned),
                custom_multiplier_bps: calculated.custom_multiplier_bps(),
            },
        })
    }
}
