//! 手动同步的外部价目来源；网络和外部文档解析由 Host 实现

use crate::model::{AdminError, pricing::PricingSyncPreview};
use async_trait::async_trait;
use std::collections::{BTreeMap, BTreeSet};

/// Provider 明确登记的外部参考模型，不按名称前缀猜测不同产品的对应关系
pub struct PricingSourceModel {
    pub provider: String,
    pub model: String,
}

/// 同名外部目录可直接同步，跨平台模型只采用 Provider 登记的参考价
pub enum PricingCatalogSource {
    Direct,
    References(BTreeMap<String, PricingSourceModel>),
}

/// 同一次同步用相同的来源声明选择外部目录并映射网关模型
pub struct PricingSourceScope {
    pub catalogs: BTreeMap<String, PricingCatalogSource>,
}

impl PricingSourceScope {
    pub fn vendors(&self) -> BTreeSet<&str> {
        let mut vendors = BTreeSet::new();
        for (kind, source) in &self.catalogs {
            match source {
                PricingCatalogSource::Direct => {
                    vendors.insert(kind.as_str());
                }
                PricingCatalogSource::References(models) => {
                    vendors.extend(models.values().map(|model| model.provider.as_str()))
                }
            }
        }
        vendors
    }

    pub fn requires_catalog(&self, vendor: &str) -> bool {
        matches!(
            self.catalogs.get(vendor),
            Some(PricingCatalogSource::Direct)
        )
    }

    pub(crate) fn project(self, source: PricingSyncPreview) -> PricingSyncPreview {
        let mut result = PricingSyncPreview {
            prices: BTreeMap::new(),
            skipped: Vec::new(),
        };
        for (kind, catalog) in self.catalogs {
            let prices = match catalog {
                PricingCatalogSource::Direct => {
                    let prefix = format!("{kind}/");
                    result.skipped.extend(
                        source
                            .skipped
                            .iter()
                            .filter(|id| id.starts_with(&prefix))
                            .cloned(),
                    );
                    source.prices.get(&kind).cloned().unwrap_or_default()
                }
                PricingCatalogSource::References(models) => {
                    let mut prices = BTreeMap::new();
                    for (model, reference) in models {
                        match source
                            .prices
                            .get(&reference.provider)
                            .and_then(|models| models.get(&reference.model))
                        {
                            Some(price) => {
                                prices.insert(model, price.clone());
                            }
                            None => result.skipped.push(format!("{kind}/{model}")),
                        }
                    }
                    prices
                }
            };
            if !prices.is_empty() {
                result.prices.insert(kind, prices);
            }
        }
        result.skipped.sort();
        result.skipped.dedup();
        result
    }
}

#[async_trait]
pub trait PricingSource: Send + Sync {
    async fn fetch(&self, scope: &PricingSourceScope) -> Result<PricingSyncPreview, AdminError>;
}
