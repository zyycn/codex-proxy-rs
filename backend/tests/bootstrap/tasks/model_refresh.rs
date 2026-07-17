use super::*;

use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicUsize, Ordering},
};

use codex_proxy_rs::{
    fleet::pool::{AccountAcquireRequest, AccountPoolOptions, AccountPoolService},
    infra::identity::AccountPseudonymizer,
    models::{
        gateway::{ModelCatalogRequest, ModelCatalogSource, ModelCatalogSourceError},
        store::{ModelSnapshotStore, ModelSnapshotStoreResult},
        types::{BackendModelEntry, ModelPlanSnapshot},
    },
};

#[tokio::test]
async fn model_refresh_task_should_start_and_shutdown() {
    let model_service = Arc::new(ModelService::new(empty_model_config(), None, None));
    let account_store = Arc::new(FakeAccountStore);
    let account_pool = Arc::new(AccountPoolService::new(
        account_store.clone(),
        Arc::new(FakeAccountUsageStore),
        AccountPoolOptions::default(),
        0,
    ));

    let handle = codex_proxy_rs::bootstrap::tasks::model_refresh::ModelRefreshTask::new(
        model_service,
        account_pool,
        Arc::new(AccountPseudonymizer::new([7; 32])),
    )
    .start();

    handle.shutdown().await;
}

#[tokio::test]
async fn model_refresh_task_should_sync_model_plan_allowlist_to_account_pool() {
    let store = Arc::new(InMemorySnapshotStore::default());
    let upstream = Arc::new(FakeModelCatalogClient {
        models_by_plan: BTreeMap::from([
            ("plus".to_string(), backend_models("gpt-plus")),
            ("team".to_string(), backend_models("gpt-team")),
        ]),
    });
    let model_service = Arc::new(ModelService::new(
        empty_model_config(),
        Some(store),
        Some(upstream),
    ));
    let request_usage_records = Arc::new(AtomicUsize::new(0));
    let account_store = Arc::new(PlanAccountStore {
        accounts: vec![
            plan_account("acct-plus", "plus"),
            plan_account("acct-team", "team"),
        ],
        request_usage_records: request_usage_records.clone(),
    });
    let usage_store = Arc::new(PlanAccountUsageStore {
        request_usage_records,
    });
    let account_pool = Arc::new(AccountPoolService::new(
        account_store.clone(),
        usage_store,
        AccountPoolOptions::default(),
        0,
    ));
    account_pool
        .restore_from_store()
        .await
        .expect("accounts should restore");

    codex_proxy_rs::bootstrap::tasks::model_refresh::ModelRefreshTask::new(
        model_service,
        account_pool.clone(),
        Arc::new(AccountPseudonymizer::new([7; 32])),
    )
    .refresh_once()
    .await
    .expect("model refresh should sync allowlist");

    let acquired = account_pool
        .acquire_with(&AccountAcquireRequest::new("gpt-team", Utc::now()))
        .await
        .expect("team model should acquire an account");
    assert_eq!(acquired.account.id, "acct-team");
    assert_eq!(
        account_store.request_usage_records.load(Ordering::SeqCst),
        0
    );
}

fn empty_model_config() -> ModelConfig {
    ModelConfig {
        model_aliases: BTreeMap::new(),
    }
}

fn backend_models(model_id: &str) -> Vec<BackendModelEntry> {
    vec![BackendModelEntry {
        slug: Some(model_id.to_string()),
        display_name: Some(model_id.to_string()),
        ..BackendModelEntry::default()
    }]
}

fn plan_account(id: &str, plan_type: &str) -> Account {
    let mut account = crate::support::accounts::test_account(id, AccountStatus::Active);
    account.plan_type = Some(plan_type.to_string());
    account
}

#[derive(Default)]
struct InMemorySnapshotStore {
    snapshots: tokio::sync::Mutex<BTreeMap<String, ModelPlanSnapshot>>,
}

#[async_trait]
impl ModelSnapshotStore for InMemorySnapshotStore {
    async fn replace_plan_snapshots(
        &self,
        snapshots: &[ModelPlanSnapshot],
    ) -> ModelSnapshotStoreResult<()> {
        let mut stored = self.snapshots.lock().await;
        stored.clear();
        stored.extend(
            snapshots
                .iter()
                .map(|snapshot| (snapshot.plan_type.clone(), snapshot.clone())),
        );
        Ok(())
    }

    async fn list_plan_snapshots(&self) -> ModelSnapshotStoreResult<Vec<ModelPlanSnapshot>> {
        Ok(self.snapshots.lock().await.values().cloned().collect())
    }
}

struct FakeModelCatalogClient {
    models_by_plan: BTreeMap<String, Vec<BackendModelEntry>>,
}

#[async_trait]
impl ModelCatalogSource for FakeModelCatalogClient {
    async fn fetch_models(
        &self,
        request: &ModelCatalogRequest<'_>,
    ) -> Result<Vec<BackendModelEntry>, ModelCatalogSourceError> {
        Ok(self
            .models_by_plan
            .get(request.plan_type)
            .cloned()
            .unwrap_or_default())
    }
}

struct PlanAccountStore {
    accounts: Vec<Account>,
    request_usage_records: Arc<AtomicUsize>,
}

#[async_trait]
impl AccountStore for PlanAccountStore {
    async fn list_pool_accounts(&self) -> AccountStoreResult<Vec<Account>> {
        Ok(self.accounts.clone())
    }

    async fn mark_quota_limited_until(
        &self,
        _account_id: &str,
        _cooldown_until: chrono::DateTime<Utc>,
    ) -> AccountStoreResult<bool> {
        Ok(true)
    }

    async fn set_cloudflare_cooldown_until(
        &self,
        _account_id: &str,
        _cooldown_until: chrono::DateTime<Utc>,
    ) -> AccountStoreResult<bool> {
        Ok(true)
    }

    async fn set_status(
        &self,
        _account_id: &str,
        _status: AccountStatus,
    ) -> AccountStoreResult<bool> {
        Ok(true)
    }

    async fn get_quota_json(&self, _account_id: &str) -> AccountStoreResult<Option<String>> {
        Ok(None)
    }

    async fn apply_quota_snapshot(
        &self,
        _account_id: &str,
        _quota_json: &str,
        _limit_reached: bool,
        _cooldown_until: Option<chrono::DateTime<Utc>>,
    ) -> AccountStoreResult<bool> {
        Ok(false)
    }

    async fn sync_runtime_account_state(&self, _account: &Account) -> AccountStoreResult<bool> {
        Ok(false)
    }
}

struct PlanAccountUsageStore {
    request_usage_records: Arc<AtomicUsize>,
}

#[async_trait]
impl AccountUsageStore for PlanAccountUsageStore {
    async fn snapshots(
        &self,
        _account_ids: &[String],
    ) -> Result<HashMap<String, AccountUsageSnapshot>, AccountUsageStoreError> {
        Ok(HashMap::new())
    }

    async fn record_usage_delta(
        &self,
        _account_id: &str,
        usage: AccountUsageDelta,
    ) -> Result<(), AccountUsageStoreError> {
        if usage.requests > 0 {
            self.request_usage_records.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }

    async fn sync_runtime_window(
        &self,
        _account_id: &str,
        _window: AccountUsageWindow,
    ) -> Result<(), AccountUsageStoreError> {
        Ok(())
    }

    async fn sync_rate_limit_window(
        &self,
        _account_id: &str,
        _reset_at: chrono::DateTime<Utc>,
        _limit_window_seconds: Option<u64>,
    ) -> Result<(), AccountUsageStoreError> {
        Ok(())
    }
}
