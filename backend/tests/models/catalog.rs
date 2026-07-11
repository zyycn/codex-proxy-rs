use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use async_trait::async_trait;

use codex_proxy_rs::fleet::account::{Account, AccountStatus};
use codex_proxy_rs::models::catalog::ModelCatalog;
use codex_proxy_rs::models::service::{
    ModelRefreshPlanAccount, ModelRefreshResult, ModelService, ModelServiceError,
};
use codex_proxy_rs::models::store::{ModelSnapshotStore, ModelSnapshotStoreResult};
use codex_proxy_rs::models::types::{
    BackendModelEntry, BackendReasoningEffort, BackendTruncationPolicy, ModelConfig,
    ModelPlanSnapshot,
};
use codex_proxy_rs::upstream::openai::transport::{
    CodexModelCatalogClient, CodexModelCatalogClientError, CodexModelCatalogRequest,
};

#[test]
fn model_catalog_should_resolve_alias_chain_to_model_id() {
    let mut model_aliases = BTreeMap::new();
    model_aliases.insert("codex-fast".to_string(), "gpt-5.5".to_string());
    let catalog = ModelCatalog::from_config(&ModelConfig { model_aliases });

    // 透明代理：只解析 alias，不再拆分 `-high`/`-flex` 等后缀。
    assert!(catalog
        .public_model_ids()
        .contains(&"codex-fast".to_string()));
    assert_eq!(
        catalog.model_info_for_name("codex-fast").unwrap().id,
        "gpt-5.5"
    );
    assert_eq!(catalog.resolve_model_id("codex-fast"), "gpt-5.5");
}

#[test]
fn model_catalog_should_keep_unknown_model_names_verbatim() {
    let catalog = ModelCatalog::from_config(&ModelConfig {
        model_aliases: BTreeMap::new(),
    });

    // 无 alias 命中时原样返回，不再剥离后缀。
    assert_eq!(catalog.resolve_model_id("unknown-medium"), "unknown-medium");
    assert_eq!(catalog.resolve_model_id("gpt-5.5-high"), "gpt-5.5-high");
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
    assert_eq!(catalog.models_for_plan("plus")[0].id, "gpt-6");
    assert!(catalog.models_for_plan("team").is_empty());
}

#[tokio::test]
async fn model_service_should_return_builtin_catalog_when_store_is_missing() {
    let service = ModelService::new(test_model_config(), None, None);

    let catalog = service.catalog().await;

    assert!(catalog.is_recognized_model_name("gpt-5.5"));
    assert!(!catalog.models().is_empty());
    assert!(catalog.model_plan_allowlist().is_empty());
}

#[tokio::test]
async fn model_service_catalog_should_use_loaded_memory_snapshot() {
    let store = Arc::new(InMemorySnapshotStore::default());
    store
        .replace_plan_snapshot(&ModelPlanSnapshot::from_backend_entries(
            "plus",
            backend_models("gpt-6"),
        ))
        .await
        .unwrap();
    let service = ModelService::new(test_model_config(), Some(store.clone()), None);

    service
        .reload_from_store()
        .await
        .expect("initial model catalog load should succeed");
    let first = service.catalog().await;
    let second = service.catalog().await;

    assert!(first.is_recognized_model_name("gpt-6"));
    assert!(second.is_recognized_model_name("gpt-6"));
    assert_eq!(store.list_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn model_service_should_refresh_plan_accounts_and_build_routing() {
    let store = Arc::new(InMemorySnapshotStore::default());
    let upstream = Arc::new(FakeModelCatalogClient::with_models(BTreeMap::from([
        ("plus".to_string(), backend_models("gpt-6")),
        ("team".to_string(), backend_models("gpt-7")),
    ])));
    let service = ModelService::new(
        test_model_config(),
        Some(store.clone()),
        Some(upstream.clone()),
    );

    let result = service
        .refresh_backend_models(
            &[
                refresh_plan_account("plus", active_account("acct-plus-1", "plus")),
                refresh_plan_account("team", active_account("acct-team", "team")),
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

    let snapshots = store.list_plan_snapshots().await.unwrap();
    assert_eq!(snapshots.len(), 2);
    assert_eq!(snapshots[0].plan_type, "plus");
    assert_eq!(snapshots[1].plan_type, "team");

    let routing = service
        .model_plan_routing()
        .await
        .expect("routing should be available");
    assert_eq!(
        routing.allowlist.get("gpt-6").unwrap(),
        &vec!["plus".to_string()]
    );
    assert_eq!(
        routing.allowlist.get("gpt-7").unwrap(),
        &vec!["team".to_string()]
    );
    assert!(routing.fetched_plan_types.contains("plus"));
    assert!(routing.fetched_plan_types.contains("team"));
}

#[tokio::test]
async fn model_service_should_return_no_accounts_when_no_plan_accounts_exist() {
    let store = Arc::new(InMemorySnapshotStore::default());
    let upstream = Arc::new(FakeModelCatalogClient::default());
    let service = ModelService::new(test_model_config(), Some(store), Some(upstream));

    let error = service
        .refresh_backend_models(&[], "req-model-refresh")
        .await
        .expect_err("refresh should fail");

    assert!(matches!(error, ModelServiceError::NoAccounts));
}

#[tokio::test]
async fn model_service_should_notify_only_when_models_etag_changes() {
    let service = ModelService::new(test_model_config(), None, None);
    let mut changes = service.subscribe_models_etag_changes();

    assert!(!service.observe_models_etag(None));
    assert!(service.observe_models_etag(Some("etag-1")));
    changes.changed().await.expect("first etag notification");
    assert_eq!(*changes.borrow_and_update(), 1);

    assert!(!service.observe_models_etag(Some("etag-1")));
    assert!(service.observe_models_etag(Some("etag-2")));
    changes.changed().await.expect("second etag notification");
    assert_eq!(*changes.borrow_and_update(), 2);
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

fn refresh_plan_account(plan_type: &str, account: Account) -> ModelRefreshPlanAccount {
    ModelRefreshPlanAccount {
        plan_type: plan_type.to_string(),
        access_token: account.access_token,
        account_id: account.account_id,
        installation_id: "test-installation-id".to_string(),
    }
}

#[derive(Default)]
struct InMemorySnapshotStore {
    snapshots: tokio::sync::Mutex<BTreeMap<String, ModelPlanSnapshot>>,
    list_calls: AtomicUsize,
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
        self.list_calls.fetch_add(1, Ordering::SeqCst);
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
    ) -> Result<Vec<serde_json::Value>, CodexModelCatalogClientError> {
        self.requested_plan_types
            .lock()
            .await
            .push(request.plan_type.to_string());
        Ok(self
            .models_by_plan
            .get(request.plan_type)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|model| serde_json::to_value(model).unwrap())
            .collect())
    }
}
