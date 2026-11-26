//! 从上游分组声明提取展示名、档位顺序及实际存在的成员

use crate::transport::Catalog;
use gateway_core::{
    account::AccountCatalogObservation,
    routing::{
        ModelCapabilities, ModelPresentation, ProviderModelCapabilities, ReasoningModelVariant,
        UpstreamModelId,
    },
};
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct ModelGroup {
    display_name: Option<String>,
    efforts: Vec<String>,
    pub(super) variants: BTreeMap<String, ReasoningModelVariant>,
}

impl ModelGroup {
    pub(super) fn into_model(
        self,
        model: UpstreamModelId,
        capabilities: ModelCapabilities,
    ) -> ProviderModelCapabilities {
        let default = self
            .efforts
            .iter()
            .find(|level| *level == "medium")
            .or_else(|| self.efforts.first())
            .cloned();
        let presentation =
            ModelPresentation::new(self.display_name, None).with_reasoning(default, self.efforts);
        ProviderModelCapabilities::new(model, capabilities.with_reasoning_variants(self.variants))
            .with_presentation(presentation)
    }
}

pub(super) fn collect(
    groups: &mut BTreeMap<UpstreamModelId, ModelGroup>,
    catalog: &Catalog,
    accounts: &[AccountCatalogObservation],
) {
    let Some(declarations) = catalog.grouped_models.as_object() else {
        return;
    };
    for (group, description) in declarations {
        let Ok(group_id) = UpstreamModelId::new(group) else {
            continue;
        };
        let Some(levels) = description["supportedThinkingLevels"].as_array() else {
            continue;
        };
        for level in levels {
            let Some(suffix) = level["thinkingSuffix"].as_str() else {
                continue;
            };
            if !suffix
                .split('-')
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric()))
            {
                continue;
            }
            let level = suffix.to_ascii_lowercase();
            let target = format!("{group}-{level}");
            if catalog.models.contains_key(&target)
                && let Ok(target) = UpstreamModelId::new(target)
            {
                let entry = groups.entry(group_id.clone()).or_default();
                if entry.display_name.is_none() {
                    entry.display_name = description["displayName"]
                        .as_str()
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .map(str::to_owned);
                }
                entry
                    .variants
                    .entry(level.clone())
                    .or_insert_with(|| {
                        entry.efforts.push(level);
                        ReasoningModelVariant {
                            model: target,
                            accounts: Default::default(),
                        }
                    })
                    .accounts
                    .extend(accounts.iter().map(|account| account.account_id.clone()));
            }
        }
    }
}
