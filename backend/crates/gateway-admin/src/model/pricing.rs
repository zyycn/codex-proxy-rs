//! 全局模型定价的管理合同。

use gateway_core::metering::{ModelPriceOverride, PricingOverrides};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PricingCatalog {
    pub defaults: PricingOverrides,
    pub overrides: PricingOverrides,
    pub synced: PricingOverrides,
    pub synced_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoredPricing {
    pub overrides: PricingOverrides,
    pub synced: PricingOverrides,
    pub synced_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PricingSyncPreview {
    pub prices: PricingOverrides,
    pub skipped: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PricingChange {
    Replace(ModelPriceOverride),
    Multiplier(u32),
    Reset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatePricing {
    pub provider: String,
    pub models: Vec<String>,
    pub change: PricingChange,
}

pub type ProviderPricingCatalog = BTreeMap<String, ModelPriceOverride>;
