use std::collections::BTreeMap;

use codex_proxy_rs::{
    config::ModelConfig,
    models::catalog::{ModelCatalog, ParsedModelName},
};

#[test]
fn model_catalog_should_parse_alias_reasoning_and_service_tier_suffixes() {
    let mut aliases = BTreeMap::new();
    aliases.insert("codex-fast".to_string(), "gpt-5.5".to_string());
    let catalog = ModelCatalog::from_config(&ModelConfig {
        default_model: "gpt-5.5".to_string(),
        default_reasoning_effort: None,
        service_tier: None,
        aliases,
    });

    let parsed = catalog.parse_model_name("codex-fast-high-flex");

    assert_eq!(
        parsed,
        ParsedModelName {
            model_id: "gpt-5.5".to_string(),
            reasoning_effort: Some("high".to_string()),
            service_tier: Some("flex".to_string())
        }
    );
}

#[test]
fn model_catalog_should_fall_back_to_default_for_unknown_model_names() {
    let catalog = ModelCatalog::from_config(&ModelConfig {
        default_model: "gpt-5.5".to_string(),
        default_reasoning_effort: None,
        service_tier: None,
        aliases: BTreeMap::new(),
    });

    let parsed = catalog.parse_model_name("unknown-medium");

    assert_eq!(parsed.model_id, "gpt-5.5");
    assert_eq!(parsed.reasoning_effort, Some("medium".to_string()));
}
