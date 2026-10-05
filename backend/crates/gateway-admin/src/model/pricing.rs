//! 全局模型定价的管理合同

use gateway_core::metering::{ModelPriceOverride, PricingOverrides};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[derive(serde::Serialize)]
pub struct SyncPricing {
    pub preview: PricingSyncPreview,
    pub models: BTreeMap<String, BTreeSet<String>>,
}

pub type PricingSyncChanges = BTreeMap<String, BTreeMap<String, Option<ModelPriceOverride>>>;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PricingChange {
    Replace(ModelPriceOverride),
    Multiplier(u32),
    Reset,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UpdatePricing {
    pub provider: String,
    pub models: Vec<String>,
    pub change: PricingChange,
}

pub type ProviderPricingCatalog = BTreeMap<String, ModelPriceOverride>;
