use std::collections::BTreeMap;

use codex_proxy_rs::{
    codex::models::{
        catalog::{
            BackendModelEntry, BackendReasoningEffort, BackendTruncationPolicy, ModelCatalog,
            ModelPlanSnapshot, ParsedModelName,
        },
        repository::ModelSnapshotRepository,
    },
    config::ModelConfig,
    storage::db::connect_sqlite,
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

#[test]
fn model_catalog_should_parse_none_and_minimal_reasoning_suffixes() {
    let catalog = ModelCatalog::from_config(&ModelConfig {
        default_model: "gpt-5.5".to_string(),
        default_reasoning_effort: None,
        service_tier: None,
        aliases: BTreeMap::new(),
    });

    let none = catalog.parse_model_name("gpt-5.5-none");
    let minimal = catalog.parse_model_name("gpt-5.5-minimal-fast");

    assert_eq!(none.reasoning_effort, Some("none".to_string()));
    assert_eq!(minimal.reasoning_effort, Some("minimal".to_string()));
    assert_eq!(minimal.service_tier, Some("fast".to_string()));
}

#[test]
fn model_catalog_should_merge_backend_snapshots_and_build_plan_allowlist() {
    let snapshot = ModelPlanSnapshot::from_backend_entries(
        "plus",
        vec![BackendModelEntry {
            slug: Some("gpt-6".to_string()),
            display_name: Some("GPT-6".to_string()),
            description: Some("Backend model".to_string()),
            is_default: Some(true),
            default_reasoning_level: Some("minimal".to_string()),
            supported_reasoning_levels: vec![BackendReasoningEffort {
                effort: Some("minimal".to_string()),
                description: Some("Minimal".to_string()),
                ..BackendReasoningEffort::default()
            }],
            input_modalities: Some(vec!["text".to_string(), "image".to_string()]),
            output_modalities: Some(vec!["text".to_string()]),
            supports_personality: Some(true),
            context_window: Some(200_000),
            max_output_tokens: Some(16_384),
            truncation_policy: Some(BackendTruncationPolicy {
                limit: Some(131_072),
            }),
            ..BackendModelEntry::default()
        }],
    );
    let catalog = ModelCatalog::from_config_and_snapshots(
        &ModelConfig {
            default_model: "gpt-5.5".to_string(),
            default_reasoning_effort: None,
            service_tier: None,
            aliases: BTreeMap::new(),
        },
        &[snapshot],
    );

    let info = catalog.model_info("gpt-6").unwrap();
    assert_eq!(info.display_name, "GPT-6");
    assert_eq!(info.default_reasoning_effort, "minimal");
    assert_eq!(
        info.supported_reasoning_efforts[0].reasoning_effort,
        "minimal"
    );
    assert_eq!(info.context_window, Some(200_000));
    assert_eq!(info.truncation_policy_limit, Some(131_072));
    assert_eq!(
        catalog.model_plan_allowlist().get("gpt-6").unwrap(),
        &vec!["plus".to_string()]
    );
}

#[tokio::test]
async fn model_snapshot_repository_should_replace_and_load_plan_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("models.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .unwrap();
    let repo = ModelSnapshotRepository::new(pool);
    let snapshot = ModelPlanSnapshot::from_backend_entries(
        "team",
        vec![BackendModelEntry {
            id: Some("gpt-team".to_string()),
            name: Some("GPT Team".to_string()),
            ..BackendModelEntry::default()
        }],
    );

    repo.replace_plan_snapshot(&snapshot).await.unwrap();
    let loaded = repo.list_plan_snapshots().await.unwrap();

    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].plan_type, "team");
    assert_eq!(loaded[0].models[0].id, "gpt-team");
    assert_eq!(loaded[0].models[0].source, "backend");
}
