//! Antigravity 桌面端与 CLI 的模型选择器识别及菜单投影

use axum::http::{HeaderMap, header::USER_AGENT};
use gateway_core::routing::{AgentModelListing, ExecutableModelMetadata, PublicModelId};

pub(super) fn uses_agent_menu(headers: &HeaderMap) -> bool {
    headers
        .get(USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|ua| ua.starts_with("antigravity/hub/") || ua.starts_with("antigravity/cli/"))
}

pub(super) fn model_menu(models: Vec<ExecutableModelMetadata<'_>>) -> Vec<(PublicModelId, String)> {
    let mut entries = Vec::new();
    for entry in models {
        let model = entry.model;
        let selected = entry
            .sources
            .iter()
            .filter_map(
                |source| match source.metadata.map(|metadata| &metadata.agent_listing) {
                    None | Some(AgentModelListing::Unspecified) => {
                        Some((usize::MAX, model.as_str()))
                    }
                    Some(AgentModelListing::Hidden) => None,
                    Some(AgentModelListing::Listed {
                        order,
                        display_name,
                    }) => {
                        let name = if model.as_str() == source.target.as_str() {
                            display_name.as_str()
                        } else {
                            model.as_str()
                        };
                        Some((*order, name))
                    }
                },
            )
            .min_by_key(|(order, _)| *order);
        if let Some((order, name)) = selected {
            let name = name.to_owned();
            entries.push((order, model, name));
        }
    }
    entries.sort_unstable();
    entries
        .into_iter()
        .map(|(_, model, name)| (model, name))
        .collect()
}
