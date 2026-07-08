use super::*;

use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicUsize, Ordering},
};

use codex_proxy_rs::upstream::{
    accounts::pool::{AccountAcquireRequest, AccountPoolOptions, RuntimeAccountPoolService},
    models::{
        backend_entry::BackendModelEntry,
        snapshot::ModelPlanSnapshot,
        snapshot_store::{ModelSnapshotStore, ModelSnapshotStoreResult},
    },
    transport::{CodexModelCatalogClient, CodexModelCatalogClientError, CodexModelCatalogRequest},
};

#[tokio::test]
async fn model_refresh_task_should_start_and_shutdown() {
    let model_service = Arc::new(ModelService::new(empty_model_config(), None, None));
    let account_store = Arc::new(FakeAccountStore);
    let account_pool = Arc::new(RuntimeAccountPoolService::new(
        account_store.clone(),
        AccountPoolOptions::default(),
        0,
    ));

    let handle = codex_proxy_rs::runtime::tasks::model_refresh::ModelRefreshTask::new(
        model_service,
        account_pool,
    )
    .start();

    handle.shutdown().await;
}

#[tokio::test]
async fn model_refresh_task_should_sync_model_plan_allowlist_to_account_pool() {
    let snapshot_store = Arc::new(InMemorySnapshotStore::default());
    let upstream = Arc::new(FakeModelCatalogClient {
        models_by_plan: BTreeMap::from([
            ("plus".to_string(), backend_models("gpt-plus")),
            ("team".to_string(), backend_models("gpt-team")),
        ]),
    });
    let model_service = Arc::new(ModelService::new(
        empty_model_config(),
        Some(snapshot_store),
        Some(upstream),
    ));
    let account_store = Arc::new(PlanAccountStore {
        accounts: vec![
            plan_account("acct-plus", "plus"),
            plan_account("acct-team", "team"),
        ],
        request_usage_records: Arc::new(AtomicUsize::new(0)),
        model_request_usage_records: Arc::new(AtomicUsize::new(0)),
    });
    let account_pool = Arc::new(RuntimeAccountPoolService::new(
        account_store.clone(),
        AccountPoolOptions::default(),
        0,
    ));
    account_pool
        .restore_from_repository()
        .await
        .expect("accounts should restore");

    codex_proxy_rs::runtime::tasks::model_refresh::ModelRefreshTask::new(
        model_service,
        account_pool.clone(),
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
    assert_eq!(
        account_store
            .model_request_usage_records
            .load(Ordering::SeqCst),
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

struct FakeModelCatalogClient {
    models_by_plan: BTreeMap<String, Vec<BackendModelEntry>>,
}

#[async_trait]
impl CodexModelCatalogClient for FakeModelCatalogClient {
    async fn fetch_models(
        &self,
        request: &CodexModelCatalogRequest<'_>,
    ) -> Result<Vec<BackendModelEntry>, CodexModelCatalogClientError> {
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
    model_request_usage_records: Arc<AtomicUsize>,
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

    async fn record_usage_delta(
        &self,
        _account_id: &str,
        usage: AccountUsageDelta,
    ) -> AccountStoreResult<()> {
        if usage.requests > 0 {
            self.request_usage_records.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }

    async fn record_model_usage_delta(
        &self,
        _account_id: &str,
        _model: &str,
        usage: AccountModelUsageDelta,
    ) -> AccountStoreResult<()> {
        if usage.requests > 0 {
            self.model_request_usage_records
                .fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
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

    async fn sync_runtime_account_state(
        &self,
        _account: &Account,
        _sync_usage_window: bool,
    ) -> AccountStoreResult<bool> {
        Ok(false)
    }

    async fn sync_rate_limit_window(
        &self,
        _account_id: &str,
        _reset_at: chrono::DateTime<Utc>,
        _limit_window_seconds: Option<u64>,
    ) -> AccountStoreResult<()> {
        Ok(())
    }
}
