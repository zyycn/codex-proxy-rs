//! 从上游分组声明提取实际存在的推理档位，不按型号前缀猜测成员

use crate::transport::Catalog;
use gateway_core::routing::UpstreamModelId;

pub(super) fn variants(catalog: &Catalog) -> Vec<(UpstreamModelId, String, UpstreamModelId)> {
    let mut variants = Vec::new();
    let Some(groups) = catalog.grouped_models.as_object() else {
        return variants;
    };
    for (group, description) in groups {
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
            if suffix.is_empty()
                || !suffix
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
                variants.push((group_id.clone(), level, target));
            }
        }
    }
    variants
}
