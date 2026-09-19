//! 容量熔断冻结恢复编排回归：到期探测、失败顺延与自适应并发下调。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Duration as TimeDelta, Utc};
use gateway_admin::freeze_recovery::{FreezeRecoveryDeps, FreezeRecoveryTask};
use gateway_admin::model::MutationContext;
use gateway_admin::model::accounts::{AccountFreeze, AccountRuntimeSnapshot};
use gateway_admin::model::settings::{
    AdminApiKey, AdminApiKeyMutation, ReplaceRuntimeSettings, RuntimeSettings,
};
use gateway_admin::ports::store::{
    AccountRuntimeStore, AccountStore, AdminStoreError, AdminStoreResult, SettingsStore,
};
use gateway_core::engine::probe::{
    AccountProbe, AccountProbeError, AccountProbeRequest, AccountProbeResult,
};
use gateway_core::lifecycle::CancellationToken;
use gateway_core::task::{ScheduledTask as _, WorkerCycleContext, WorkerId, WorkerKind};

use super::AdminHarness;
use super::accounts::{FakeAccountStore, FakeProviderAdmin, account_record, events, revision};

fn runtime_settings(enabled: bool, probe_enabled: bool, adaptive: bool) -> RuntimeSettings {
    RuntimeSettings {
        openai_client_profile: None,
        xai_client_profile: None,
        request_location_enabled: false,
        request_location: Default::default(),
        config_revision: revision(1),
        model_mappings: Default::default(),
        refresh_margin_seconds: 300,
        refresh_concurrency: 2,
        max_concurrent_per_account: 5,
        request_interval_ms: 0,
        max_waiting_per_key: 0,
        max_waiting_per_account: 0,
        concurrency_wait_timeout_seconds: 30,
        responses_max_decompressed_body_bytes: 64 * 1024 * 1024,
        rotation_strategy: gateway_admin::model::settings::RotationStrategy::Smart,
        min_codex_desktop_version: None,
        min_codex_cli_version: None,
        usage_retention_days: 31,
        ops_event_retention_days: 30,
        audit_retention_days: 90,
        account_auto_freeze_enabled: enabled,
        account_auto_freeze_threshold: 12,
        account_auto_freeze_window_seconds: 600,
        account_auto_freeze_duration_seconds: 7_200,
        account_auto_freeze_probe_enabled: probe_enabled,
        account_auto_freeze_probe_model: Some("gpt-5.5".to_owned()),
        account_auto_freeze_adaptive_concurrency: adaptive,
        updated_at: Utc::now(),
    }
}

struct FreezeSettingsStore {
    settings: RuntimeSettings,
}

#[async_trait]
impl SettingsStore for FreezeSettingsStore {
    async fn load_pricing(&self) -> AdminStoreResult<gateway_admin::model::pricing::StoredPricing> {
        Ok(Default::default())
    }
    async fn sync_pricing(
        &self,
        _: gateway_admin::model::pricing::PricingSyncChanges,
        _: &MutationContext,
    ) -> AdminStoreResult<gateway_admin::model::Revision> {
        panic!("unexpected pricing sync")
    }
    async fn update_pricing(
        &self,
        _: gateway_admin::model::pricing::UpdatePricing,
        _: &MutationContext,
    ) -> AdminStoreResult<gateway_admin::model::Revision> {
        panic!("unexpected pricing update")
    }
    async fn load_runtime_settings(&self) -> AdminStoreResult<RuntimeSettings> {
        Ok(self.settings.clone())
    }

    async fn admin_api_key_exists(&self) -> AdminStoreResult<bool> {
        Ok(false)
    }

    async fn replace_runtime_settings(
        &self,
        _: ReplaceRuntimeSettings,
        _: &MutationContext,
    ) -> AdminStoreResult<RuntimeSettings> {
        Err(store_unavailable())
    }

    async fn replace_admin_api_key(
        &self,
        _: AdminApiKey,
        _: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation> {
        Err(store_unavailable())
    }

    async fn delete_admin_api_key(
        &self,
        _: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation> {
        Err(store_unavailable())
    }
}

fn store_unavailable() -> AdminStoreError {
    AdminStoreError::new(
        gateway_admin::ports::store::AdminStoreErrorKind::Unavailable,
        "freeze recovery test",
        "unused",
    )
}

/// 记录解冻/顺延调用的运行态 fake；冻结与峰值证据由测试预置。
struct FreezeRuntimeStore {
    freezes: BTreeMap<String, AccountFreeze>,
    peaks: BTreeMap<String, u32>,
    cleared: Mutex<Vec<String>>,
    extended: Mutex<Vec<(String, DateTime<Utc>)>>,
}

impl FreezeRuntimeStore {
    fn new(freezes: BTreeMap<String, AccountFreeze>, peaks: BTreeMap<String, u32>) -> Arc<Self> {
        Arc::new(Self {
            freezes,
            peaks,
            cleared: Mutex::new(Vec::new()),
            extended: Mutex::new(Vec::new()),
        })
    }

    fn cleared(&self) -> Vec<String> {
        self.cleared.lock().expect("cleared lock").clone()
    }

    fn extended(&self) -> Vec<(String, DateTime<Utc>)> {
        self.extended.lock().expect("extended lock").clone()
    }
}

#[async_trait]
impl AccountRuntimeStore for FreezeRuntimeStore {
    async fn active_rate_limits(&self) -> AdminStoreResult<AccountRuntimeSnapshot> {
        Ok(AccountRuntimeSnapshot::default())
    }

    async fn account_runtime(
        &self,
        _account_ids: &[String],
    ) -> AdminStoreResult<AccountRuntimeSnapshot> {
        Ok(AccountRuntimeSnapshot::default())
    }

    async fn active_freezes(&self) -> AdminStoreResult<BTreeMap<String, AccountFreeze>> {
        Ok(self.freezes.clone())
    }

    async fn capacity_peaks(
        &self,
        account_ids: &[String],
    ) -> AdminStoreResult<BTreeMap<String, u32>> {
        Ok(account_ids
            .iter()
            .filter_map(|id| self.peaks.get(id).map(|peak| (id.clone(), *peak)))
            .collect())
    }

    async fn finish_freeze(
        &self,
        account_id: &str,
        expected: &AccountFreeze,
        postpone_until: Option<DateTime<Utc>>,
    ) -> AdminStoreResult<bool> {
        assert_eq!(self.freezes.get(account_id), Some(expected));
        if let Some(until) = postpone_until {
            self.extended
                .lock()
                .expect("extended")
                .push((account_id.to_owned(), until));
        } else {
            self.cleared
                .lock()
                .expect("cleared")
                .push(account_id.to_owned());
        }
        Ok(true)
    }
}

struct SuccessfulProbe;

impl AccountProbe for SuccessfulProbe {
    fn probe(
        &self,
        _: AccountProbeRequest,
    ) -> futures::future::BoxFuture<'_, Result<AccountProbeResult, AccountProbeError>> {
        Box::pin(async {
            Ok(AccountProbeResult {
                text: vec!["OK".to_owned()],
            })
        })
    }
}

struct FailingProbe;

impl AccountProbe for FailingProbe {
    fn probe(
        &self,
        _: AccountProbeRequest,
    ) -> futures::future::BoxFuture<'_, Result<AccountProbeResult, AccountProbeError>> {
        Box::pin(async {
            Err(AccountProbeError::new(
                gateway_core::error::GatewayError::new(
                    gateway_core::error::GatewayErrorKind::RateLimited,
                    "still overloaded",
                ),
                gateway_core::engine::probe::AccountProbeErrorSource::Upstream,
                Some(gateway_core::upstream::UpstreamSendState::NotSent),
                None,
            ))
        })
    }
}

async fn recovery_task(
    settings: RuntimeSettings,
    runtime: Arc<FreezeRuntimeStore>,
    probe: Arc<dyn AccountProbe>,
) -> (FreezeRecoveryTask, Arc<FakeAccountStore>) {
    recovery_task_with_store(settings, runtime, probe).await
}

async fn recovery_task_with_store(
    settings: RuntimeSettings,
    runtime: Arc<FreezeRuntimeStore>,
    probe: Arc<dyn AccountProbe>,
) -> (FreezeRecoveryTask, Arc<FakeAccountStore>) {
    let store = FakeAccountStore::new("openai", events());
    let services = AdminHarness::new()
        .accounts(Arc::clone(&store) as Arc<dyn AccountStore>)
        .account_runtime(Arc::clone(&runtime) as Arc<dyn AccountRuntimeStore>)
        .settings(Arc::new(FreezeSettingsStore {
            settings: settings.clone(),
        }))
        .provider(FakeProviderAdmin::new("openai", events()))
        .probe(probe)
        .build()
        .await;
    let task = FreezeRecoveryTask::new(FreezeRecoveryDeps {
        accounts: services.accounts_handle(),
        store: store.clone(),
        runtime,
        settings: Arc::new(FreezeSettingsStore {
            settings: settings.clone(),
        }),
    });
    (task, store)
}

async fn run_cycle(task: &FreezeRecoveryTask) {
    let worker = WorkerId::try_new(WorkerKind::AccountFreezeRecovery, "test").expect("worker id");
    let context = WorkerCycleContext::new(worker, None, CancellationToken::new());
    task.run_cycle(context)
        .await
        .expect("freeze recovery cycle");
}

fn freeze_until(until: DateTime<Utc>) -> BTreeMap<String, AccountFreeze> {
    BTreeMap::from([(
        "acct_test".to_owned(),
        AccountFreeze {
            credential_revision: revision(1),
            until,
            generation: "freeze-generation".to_owned(),
            requires_probe: true,
        },
    )])
}

fn freeze_due() -> BTreeMap<String, AccountFreeze> {
    freeze_until(Utc::now() - TimeDelta::seconds(1))
}

#[tokio::test]
async fn probe_success_clears_freeze() {
    let runtime = FreezeRuntimeStore::new(freeze_due(), BTreeMap::new());
    let (task, _store) = recovery_task(
        runtime_settings(true, true, false),
        Arc::clone(&runtime),
        Arc::new(SuccessfulProbe),
    )
    .await;

    run_cycle(&task).await;

    assert_eq!(runtime.cleared(), vec!["acct_test".to_owned()]);
    assert!(runtime.extended().is_empty());
}

#[tokio::test]
async fn probe_failure_postpones_freeze() {
    let runtime = FreezeRuntimeStore::new(freeze_due(), BTreeMap::new());
    let (task, _store) = recovery_task(
        runtime_settings(true, true, false),
        Arc::clone(&runtime),
        Arc::new(FailingProbe),
    )
    .await;

    run_cycle(&task).await;

    assert!(runtime.cleared().is_empty());
    let extended = runtime.extended();
    assert_eq!(extended.len(), 1);
    let (account_id, until) = &extended[0];
    assert_eq!(account_id, "acct_test");
    let remaining = until.timestamp_millis() - Utc::now().timestamp_millis();
    assert!(
        remaining > 7_000_000 && remaining <= 7_200_000,
        "postponed freeze should be about 2 hours, got {remaining}ms"
    );
}

#[tokio::test]
async fn disabled_policy_releases_due_freeze_without_probing() {
    let runtime = FreezeRuntimeStore::new(freeze_due(), BTreeMap::new());
    let (task, _store) = recovery_task(
        runtime_settings(false, true, false),
        Arc::clone(&runtime),
        Arc::new(SuccessfulProbe),
    )
    .await;

    run_cycle(&task).await;

    assert_eq!(runtime.cleared(), vec!["acct_test".to_owned()]);
    assert!(runtime.extended().is_empty());
}

#[tokio::test]
async fn adaptive_concurrency_lowers_limit_to_observed_peak() {
    let runtime = FreezeRuntimeStore::new(
        freeze_until(Utc::now() + TimeDelta::hours(1)),
        BTreeMap::from([("acct_test".to_owned(), 4_u32)]),
    );
    let (task, store) = recovery_task(
        runtime_settings(true, false, true),
        Arc::clone(&runtime),
        Arc::new(SuccessfulProbe),
    )
    .await;

    run_cycle(&task).await;

    // 峰值 4 × 0.8 = 3（不低于下限 2），低于全局默认 5，应下调到 3。
    assert_eq!(
        *store.lowered_limits.lock().expect("lowered limits"),
        vec![("acct_test".to_owned(), 3)]
    );
    assert!(store.update_commands().is_empty());
    assert!(runtime.extended().is_empty());
    assert!(runtime.cleared().is_empty());
}

#[tokio::test]
async fn adaptive_concurrency_never_raises_limit() {
    let runtime = FreezeRuntimeStore::new(
        freeze_until(Utc::now() + TimeDelta::hours(1)),
        BTreeMap::from([("acct_test".to_owned(), 40_u32)]),
    );
    let (task, store) = recovery_task(
        runtime_settings(true, false, true),
        Arc::clone(&runtime),
        Arc::new(SuccessfulProbe),
    )
    .await;

    run_cycle(&task).await;

    // 峰值 40 的目标 32 高于全局默认 5；只降不升，不下发任何更新。
    assert!(store.update_commands().is_empty());
}

/// 到期时间还早（超过探测提前量）的冻结不发起探测。
#[tokio::test]
async fn probe_skips_freezes_far_from_expiry() {
    let runtime = FreezeRuntimeStore::new(
        freeze_until(Utc::now() + TimeDelta::hours(1)),
        BTreeMap::new(),
    );
    let (task, _store) = recovery_task(
        runtime_settings(true, true, false),
        Arc::clone(&runtime),
        Arc::new(SuccessfulProbe),
    )
    .await;

    run_cycle(&task).await;

    assert!(runtime.cleared().is_empty());
    assert!(runtime.extended().is_empty());
}

/// 停用账号不参与探测恢复；并发下调同样跳过。
#[tokio::test]
async fn disabled_accounts_are_skipped() {
    let runtime = FreezeRuntimeStore::new(
        freeze_due(),
        BTreeMap::from([("acct_test".to_owned(), 4_u32)]),
    );
    let mut record = account_record("openai");
    record.enabled = false;
    let store = FakeAccountStore::with_account(record, events());
    let services = AdminHarness::new()
        .accounts(Arc::clone(&store) as Arc<dyn AccountStore>)
        .account_runtime(Arc::clone(&runtime) as Arc<dyn AccountRuntimeStore>)
        .settings(Arc::new(FreezeSettingsStore {
            settings: runtime_settings(true, true, true),
        }))
        .provider(FakeProviderAdmin::new("openai", events()))
        .probe(Arc::new(SuccessfulProbe))
        .build()
        .await;
    let task = FreezeRecoveryTask::new(FreezeRecoveryDeps {
        accounts: services.accounts_handle(),
        store: store.clone(),
        runtime: Arc::clone(&runtime) as Arc<dyn AccountRuntimeStore>,
        settings: Arc::new(FreezeSettingsStore {
            settings: runtime_settings(true, true, true),
        }),
    });

    run_cycle(&task).await;

    assert!(runtime.cleared().is_empty());
    assert!(runtime.extended().is_empty());
    assert!(store.update_commands().is_empty());
}
