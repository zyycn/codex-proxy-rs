//! 从上游分组声明解析思考档位及其来源账号

use crate::transport::Catalog;
use gateway_core::{
    account::AccountCatalogObservation,
    routing::{ReasoningModelVariant, UpstreamModelId},
};
use std::collections::BTreeMap;

pub(super) fn collect(
    groups: &mut BTreeMap<UpstreamModelId, BTreeMap<String, ReasoningModelVariant>>,
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
                groups
                    .entry(group_id.clone())
                    .or_default()
                    .entry(level)
                    .or_insert_with(|| ReasoningModelVariant {
                        model: target,
                        accounts: Default::default(),
                    })
                    .accounts
                    .extend(accounts.iter().map(|account| account.account_id.clone()));
            }
        }
    }
}
