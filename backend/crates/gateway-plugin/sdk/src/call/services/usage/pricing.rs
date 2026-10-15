//! 纯 Token 计价合同，费用是否采用与额外费用合并由调用方决定

use super::{UsageCost, UsageTokens};
use crate::call::services::settings::ModelPriceOverride;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UsagePriceSelection {
    Current,
    Override { price: Box<ModelPriceOverride> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsagePricingInput {
    pub id: String,
    pub provider: String,
    pub model: String,
    pub tokens: UsageTokens,
    pub service_tier: Option<String>,
    pub price: UsagePriceSelection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluateUsagePricing {
    pub items: Vec<UsagePricingInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsagePricingBreakdown {
    pub input_amount: String,
    pub output_amount: String,
    pub cache_read_amount: String,
    pub cache_write_amount: String,
    pub standard_amount: String,
    pub long_context_billing_applied: bool,
    pub service_tier: Option<String>,
    pub custom_multiplier_bps: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum UsagePricingResult {
    Estimated {
        id: String,
        cost: UsageCost,
        breakdown: UsagePricingBreakdown,
    },
    NotEstimable {
        id: String,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsagePricingEvaluation {
    pub items: Vec<UsagePricingResult>,
}
