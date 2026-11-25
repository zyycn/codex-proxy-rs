//! 历史费用场景的 Provider 计价投影

use crate::model::AdminError;
use crate::model::observability::{
    CurrencyCost, DecimalAmount, ProviderBillingInput, UsageBillingProjection,
    UsageCalculatedBillingFact,
};
use crate::ports::provider::{ProviderAdminErrorKind, ProviderAdminRegistry};
use crate::ports::store::UsageBillingProjector;
use crate::use_case::map_provider_error;
use gateway_core::routing::ProviderKind;
use std::str::FromStr;

/// 在后台投影时恢复费用场景，查询不再逐请求读取计费快照
pub struct ProviderUsageBillingProjector {
    providers: ProviderAdminRegistry,
}

impl ProviderUsageBillingProjector {
    #[must_use]
    pub fn new(providers: ProviderAdminRegistry) -> Self {
        Self { providers }
    }
}

impl UsageBillingProjector for ProviderUsageBillingProjector {
    fn project(
        &self,
        fact: &UsageCalculatedBillingFact,
    ) -> Result<UsageBillingProjection, AdminError> {
        let Ok(provider_kind) = ProviderKind::new(fact.provider_kind.clone()) else {
            return Ok(UsageBillingProjection::default());
        };
        let input = ProviderBillingInput {
            upstream_model_id: fact.upstream_model_id.clone(),
            service_tier: fact.service_tier.clone(),
            input_tokens: fact.input_tokens,
            output_tokens: fact.output_tokens,
            cached_input_tokens: fact.cached_input_tokens,
            cache_write_input_tokens: fact.cache_write_input_tokens,
            total: fact.total.clone(),
        };
        let breakdown = match fact
            .breakdown
            .as_ref()
            .filter(|breakdown| breakdown.total_amount == input.total)
            .cloned()
            .map_or_else(
                || self.providers.calculated_billing(&provider_kind, &input),
                |breakdown| Ok(Some(breakdown)),
            ) {
            Ok(breakdown) => breakdown,
            Err(error) if error.kind() == ProviderAdminErrorKind::Unsupported => {
                return Ok(UsageBillingProjection::default());
            }
            Err(error) => return Err(map_provider_error(error, "usage billing")),
        };
        let Some(breakdown) = breakdown else {
            return Ok(UsageBillingProjection::default());
        };
        if !breakdown
            .standard_amount
            .currency
            .eq_ignore_ascii_case("USD")
        {
            return Ok(UsageBillingProjection::default());
        }
        Ok(UsageBillingProjection {
            standard_amount: Some(breakdown.standard_amount.amount.clone()),
            no_cache_amount: no_cache_cost(fact, &breakdown),
        })
    }
}

fn no_cache_cost(
    fact: &UsageCalculatedBillingFact,
    breakdown: &crate::model::observability::CalculatedBillingBreakdown,
) -> Option<DecimalAmount> {
    let image = breakdown.image.as_ref();
    let input_tokens = fact
        .input_tokens?
        .checked_sub(image.map_or(0, |i| i.input_tokens))?;
    let cached_input_tokens = fact
        .cached_input_tokens
        .unwrap_or_default()
        .checked_sub(image.map_or(0, |i| i.cached_input_tokens))?
        .min(input_tokens);
    let cache_write_input_tokens = fact
        .cache_write_input_tokens
        .unwrap_or_default()
        .min(input_tokens.saturating_sub(cached_input_tokens));
    let input_rate = decimal(&breakdown.input_price_per_million.amount)?;
    let replaced_input_amount = input_rate
        .scaled()
        .checked_mul(u128::from(
            cached_input_tokens.saturating_add(cache_write_input_tokens),
        ))?
        .checked_div(1_000_000)?;
    let mut total = decimal(&breakdown.total_amount.amount)?
        .scaled()
        .checked_sub(decimal(&breakdown.cache_read_amount.amount)?.scaled())?
        .checked_sub(decimal(&breakdown.cache_write_amount.amount)?.scaled())?
        .checked_add(replaced_input_amount)?;
    if let Some(image) = image {
        let image_no_cache = decimal(&image.input_price_per_million.amount)?
            .scaled()
            .checked_mul(u128::from(image.cached_input_tokens))?
            .checked_div(1_000_000)?;
        total = total
            .checked_sub(decimal(&image.cache_read_amount.amount)?.scaled())?
            .checked_add(image_no_cache)?;
    }
    DecimalAmount::from_str(
        &gateway_core::metering::Decimal::from_scaled(total)
            .ok()?
            .canonical(),
    )
    .ok()
}

pub(super) fn amount_difference(
    larger: Option<&DecimalAmount>,
    smaller: Option<&DecimalAmount>,
) -> Option<DecimalAmount> {
    let difference = decimal(larger?)?
        .scaled()
        .checked_sub(decimal(smaller?)?.scaled())?;
    DecimalAmount::from_str(
        &gateway_core::metering::Decimal::from_scaled(difference)
            .ok()?
            .canonical(),
    )
    .ok()
}

fn decimal(value: &DecimalAmount) -> Option<gateway_core::metering::Decimal> {
    value.as_str().parse().ok()
}

pub(super) fn usd_cost(costs: &[CurrencyCost]) -> Option<DecimalAmount> {
    costs
        .iter()
        .find(|cost| cost.currency.eq_ignore_ascii_case("USD"))
        .map(|cost| cost.amount.clone())
}
