use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;

use codex_proxy_rs::upstream::accounts::model::{Account, AccountStatus};
use codex_proxy_rs::upstream::models::{
    BackendModelEntry, BackendReasoningEffort, BackendTruncationPolicy, ModelConfig,
    ModelPlanSnapshot, ParsedModelName,
};
use codex_proxy_rs::upstream::models::{
    ModelCatalog, ModelRefreshResult, ModelService, ModelServiceError, ModelSnapshotStore,
    ModelSnapshotStoreResult,
};
use codex_proxy_rs::upstream::transport::{
    CodexModelCatalogClient, CodexModelCatalogClientError, CodexModelCatalogRequest,
};

#[test]
fn model_catalog_should_parse_alias_reasoning_and_service_tier_suffixes() {
    let mut model_aliases = BTreeMap::new();
    model_aliases.insert("codex-fast".to_string(), "gpt-5.5".to_string());
    let catalog = ModelCatalog::from_config(&ModelConfig { model_aliases });

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
fn model_catalog_should_keep_unknown_model_names_without_fallback() {
    let catalog = ModelCatalog::from_config(&ModelConfig {
        model_aliases: BTreeMap::new(),
    });

    let parsed = catalog.parse_model_name("unknown-medium");

    assert_eq!(parsed.model_id, "unknown");
    assert_eq!(parsed.reasoning_effort, Some("medium".to_string()));
}

#[test]
fn model_catalog_should_parse_none_and_minimal_reasoning_suffixes() {
    let catalog = ModelCatalog::from_config(&ModelConfig {
        model_aliases: BTreeMap::new(),
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
            model_aliases: BTreeMap::new(),
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
async fn model_service_should_return_empty_catalog_when_snapshot_store_is_missing() {
    let service = ModelService::new(test_model_config(), None, None, None);

    let catalog = service.catalog().await;

    assert!(catalog.models().is_empty());
}

#[tokio::test]
async fn model_service_should_refresh_distinct_active_plans_and_build_allowlist() {
    let snapshot_store = Arc::new(InMemorySnapshotStore::default());
    let upstream = Arc::new(FakeModelCatalogClient::with_models(BTreeMap::from([
        ("plus".to_string(), backend_models("gpt-6")),
        ("team".to_string(), backend_models("gpt-7")),
    ])));
    let service = ModelService::new(
        test_model_config(),
        Some(snapshot_store.clone()),
        Some(upstream.clone()),
        None,
    );

    let result = service
        .refresh_backend_models(
            &[
                active_account("acct-plus-1", "plus"),
                active_account("acct-plus-2", "plus"),
                active_account("acct-team", "team"),
                inactive_account("acct-disabled", "plus"),
            ],
            "req-model-refresh",
        )
        .await
        .expect("refresh should succeed");

    assert_eq!(
        result,
        ModelRefreshResult {
            refreshed_plans: 2,
            model_count: 2,
            failed_plans: 0,
        }
    );
    assert_eq!(upstream.recorded_plan_types().await, vec!["plus", "team"]);

    let snapshots = snapshot_store.list_plan_snapshots().await.unwrap();
    assert_eq!(snapshots.len(), 2);
    assert_eq!(snapshots[0].plan_type, "plus");
    assert_eq!(snapshots[1].plan_type, "team");

    let allowlist = service
        .model_plan_allowlist()
        .await
        .expect("allowlist should be available");
    assert_eq!(allowlist.get("gpt-6").unwrap(), &vec!["plus".to_string()]);
    assert_eq!(allowlist.get("gpt-7").unwrap(), &vec!["team".to_string()]);
}

#[tokio::test]
async fn model_service_should_return_no_accounts_when_no_active_plans_exist() {
    let snapshot_store = Arc::new(InMemorySnapshotStore::default());
    let upstream = Arc::new(FakeModelCatalogClient::default());
    let service = ModelService::new(
        test_model_config(),
        Some(snapshot_store),
        Some(upstream),
        None,
    );

    let error = service
        .refresh_backend_models(
            &[inactive_account("acct-disabled", "plus")],
            "req-model-refresh",
        )
        .await
        .expect_err("refresh should fail");

    assert!(matches!(error, ModelServiceError::NoAccounts));
}

fn test_model_config() -> ModelConfig {
    ModelConfig {
        model_aliases: BTreeMap::new(),
    }
}

fn backend_models(model_id: &str) -> Vec<BackendModelEntry> {
    vec![BackendModelEntry {
        slug: Some(model_id.to_string()),
        display_name: Some(model_id.to_uppercase()),
        default_reasoning_level: Some("minimal".to_string()),
        supported_reasoning_levels: vec![BackendReasoningEffort {
            effort: Some("minimal".to_string()),
            description: Some("Minimal".to_string()),
            ..BackendReasoningEffort::default()
        }],
        ..BackendModelEntry::default()
    }]
}

fn active_account(id: &str, plan_type: &str) -> Account {
    let mut account = crate::support::accounts::test_account(id, AccountStatus::Active);
    account.account_id = Some(format!("upstream-{id}"));
    account.plan_type = Some(plan_type.to_string());
    account
}

fn inactive_account(id: &str, plan_type: &str) -> Account {
    let mut account = active_account(id, plan_type);
    account.status = AccountStatus::Disabled;
    account
}

#[derive(Default)]
struct InMemorySnapshotStore {
    snapshots: tokio::sync::Mutex<BTreeMap<String, ModelPlanSnapshot>>,
}

#[async_trait]
impl ModelSnapshotStore for InMemorySnapshotStore {
    async fn replace_plan_snapshot(
        &self,
        snapshot: &ModelPlanSnapshot,
    ) -> ModelSnapshotStoreResult<()> {
        self.snapshots
            .lock()
            .await
            .insert(snapshot.plan_type.clone(), snapshot.clone());
        Ok(())
    }

    async fn list_plan_snapshots(&self) -> ModelSnapshotStoreResult<Vec<ModelPlanSnapshot>> {
        Ok(self.snapshots.lock().await.values().cloned().collect())
    }
}

#[derive(Default)]
struct FakeModelCatalogClient {
    models_by_plan: BTreeMap<String, Vec<BackendModelEntry>>,
    requested_plan_types: tokio::sync::Mutex<Vec<String>>,
}

impl FakeModelCatalogClient {
    fn with_models(models_by_plan: BTreeMap<String, Vec<BackendModelEntry>>) -> Self {
        Self {
            models_by_plan,
            requested_plan_types: tokio::sync::Mutex::new(Vec::new()),
        }
    }

    async fn recorded_plan_types(&self) -> Vec<String> {
        self.requested_plan_types.lock().await.clone()
    }
}

#[async_trait]
impl CodexModelCatalogClient for FakeModelCatalogClient {
    async fn fetch_models(
        &self,
        request: &CodexModelCatalogRequest<'_>,
    ) -> Result<Vec<BackendModelEntry>, CodexModelCatalogClientError> {
        self.requested_plan_types
            .lock()
            .await
            .push(request.plan_type.to_string());
        Ok(self
            .models_by_plan
            .get(request.plan_type)
            .cloned()
            .unwrap_or_default())
    }
}
