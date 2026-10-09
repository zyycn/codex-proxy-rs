//! 验证设置服务调用、Provider 参数校验与价格管理行为

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;

use gateway_admin::{
    model::{
        AdminErrorKind, MutationContext, Revision,
        pricing::{PricingChange, PricingSyncPreview, StoredPricing, SyncPricing, UpdatePricing},
        settings::{
            AdminApiKey, AdminApiKeyMutation, ReplaceRuntimeSettings, RotationStrategy,
            RuntimeSettings,
        },
    },
    ports::store::{AdminStoreError, AdminStoreErrorKind, AdminStoreResult, SettingsStore},
};

struct UnusedSettingsStore;

#[derive(Default)]
struct PricingSettingsStore {
    updated: Mutex<Option<UpdatePricing>>,
}

#[tokio::test]
async fn explicit_service_calls_freeze_empty_plan_and_cancel_before_entering_terminal() {
    use gateway_admin::public_service::{Origin, Plan, scope};
    use std::sync::atomic::{AtomicUsize, Ordering};

    let resolutions = Arc::new(AtomicUsize::new(0));
    let source_calls = resolutions.clone();
    let store = Arc::new(PricingSettingsStore::default());
    let services = super::AdminHarness::new()
        .settings(store.clone())
        .service_middleware(Arc::new(move || {
            source_calls.fetch_add(1, Ordering::SeqCst);
            None
        }))
        .build()
        .await;
    let context = MutationContext {
        actor: gateway_admin::model::MutationActor::System,
        request_id: "empty-plan".into(),
    };
    let command = UpdatePricing {
        provider: "xai".into(),
        models: vec!["example-model".into()],
        change: PricingChange::Reset,
    };
    for expected in 1..=2 {
        services
            .public_services()
            .call(
                "settings.update_pricing",
                serde_json::to_value((&context, command.clone())).unwrap(),
            )
            .await
            .unwrap();
        // 内部 pricing 不再单独进入插件链；显式调用只解析一次发布集合
        assert_eq!(resolutions.load(Ordering::SeqCst), expected);
    }
    store.updated.lock().unwrap().take();
    let cancellation = gateway_core::lifecycle::CancellationToken::new();
    cancellation.cancel();
    let error = scope(
        Origin {
            request_id: "cancelled-native".into(),
            call_id: "entry".into(),
            cancellation,
            extensions: Default::default(),
            plan: Plan::Frozen(None),
        },
        services.public_services().call(
            "settings.update_pricing",
            serde_json::to_value((context, command)).unwrap(),
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, "unavailable");
    assert!(store.updated.lock().unwrap().is_none());
    assert_eq!(resolutions.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn client_profile_preview_rejects_unknown_provider_before_loading_settings() {
    let services = super::AdminHarness::new()
        .settings(Arc::new(UnusedSettingsStore))
        .build()
        .await;
    let error = services
        .settings()
        .preview_client_profile("unknown", None)
        .await
        .expect_err("unknown provider cannot use global settings");
    assert_eq!(error.kind(), AdminErrorKind::Invalid);
}

#[tokio::test]
async fn pricing_accepts_native_provider_and_rejects_unknown_sync_provider() {
    let store = Arc::new(PricingSettingsStore::default());
    let services = super::AdminHarness::new()
        .settings(store.clone())
        .provider(super::test_provider("xai"))
        .build()
        .await;
    let context = MutationContext {
        actor: gateway_admin::model::MutationActor::System,
        request_id: "request-native-pricing".to_owned(),
    };
    let pricing = serde_json::from_value(serde_json::json!({
        "multiplierBps": 10000,
        "bands": {"standard": {
            "input": "1", "output": "2", "cacheRead": "0", "cacheWrite": "0"
        }}
    }))
    .expect("native pricing");
    let update = UpdatePricing {
        provider: "xai".to_owned(),
        models: vec!["example-model".to_owned()],
        change: PricingChange::Replace(pricing),
    };

    services
        .settings()
        .update_pricing(&context, update.clone())
        .await
        .expect("native provider pricing");
    assert_eq!(
        store.updated.lock().expect("updated pricing").as_ref(),
        Some(&update)
    );

    let error = services
        .settings()
        .sync_pricing(
            &context,
            SyncPricing {
                preview: PricingSyncPreview {
                    prices: BTreeMap::new(),
                    skipped: Vec::new(),
                },
                models: BTreeMap::from([(
                    "unregistered-provider".to_owned(),
                    BTreeSet::from(["example-model".to_owned()]),
                )]),
            },
        )
        .await
        .expect_err("unknown provider cannot enter pricing sync");
    assert_eq!(error.kind(), AdminErrorKind::Invalid);
}

#[async_trait]
impl SettingsStore for UnusedSettingsStore {
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
        Err(unused())
    }

    async fn admin_api_key_exists(&self) -> AdminStoreResult<bool> {
        Err(unused())
    }

    async fn replace_runtime_settings(
        &self,
        _: ReplaceRuntimeSettings,
        _: &MutationContext,
    ) -> AdminStoreResult<RuntimeSettings> {
        Err(unused())
    }

    async fn replace_admin_api_key(
        &self,
        _: AdminApiKey,
        _: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation> {
        Err(unused())
    }

    async fn delete_admin_api_key(
        &self,
        _: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation> {
        Err(unused())
    }
}

#[async_trait]
impl SettingsStore for PricingSettingsStore {
    async fn load_pricing(&self) -> AdminStoreResult<StoredPricing> {
        Ok(StoredPricing::default())
    }

    async fn sync_pricing(
        &self,
        _: gateway_admin::model::pricing::PricingSyncChanges,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        panic!("unexpected pricing sync")
    }

    async fn update_pricing(
        &self,
        command: UpdatePricing,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        *self.updated.lock().expect("updated pricing") = Some(command);
        Ok(Revision::new(2).expect("revision"))
    }

    async fn load_runtime_settings(&self) -> AdminStoreResult<RuntimeSettings> {
        Err(unused())
    }

    async fn admin_api_key_exists(&self) -> AdminStoreResult<bool> {
        Err(unused())
    }

    async fn replace_runtime_settings(
        &self,
        _: ReplaceRuntimeSettings,
        _: &MutationContext,
    ) -> AdminStoreResult<RuntimeSettings> {
        Err(unused())
    }

    async fn replace_admin_api_key(
        &self,
        _: AdminApiKey,
        _: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation> {
        Err(unused())
    }

    async fn delete_admin_api_key(
        &self,
        _: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation> {
        Err(unused())
    }
}

#[tokio::test]
async fn settings_should_reject_zero_refresh_margin_before_store_call() {
    let services = super::AdminHarness::new()
        .settings(Arc::new(UnusedSettingsStore))
        .build()
        .await;
    let error = services
        .settings()
        .replace(
            &MutationContext {
                actor: gateway_admin::model::MutationActor::System,
                request_id: "request-settings".to_owned(),
            },
            ReplaceRuntimeSettings {
                expected_revision: gateway_admin::model::Revision::new(1).unwrap(),
                request_profile_updates: Default::default(),
                model_mappings: Default::default(),
                rotation_strategy: RotationStrategy::Smart,
                values: gateway_admin::model::settings::RuntimeSettingsValues {
                    codex_privacy_policy: Default::default(),
                    request_location_enabled: false,
                    request_location: Default::default(),
                    refresh_margin_seconds: 0,
                    refresh_concurrency: 1,
                    max_concurrent_per_account: 1,
                    request_interval_ms: 0,
                    max_waiting_per_key: 0,
                    max_waiting_per_account: 0,
                    concurrency_wait_timeout_seconds: 30,
                    openai_guardian_reserved_concurrency: 0,
                    openai_account_affinity: gateway_core::account::AccountAffinity::Relaxed,
                    max_account_rotations: 3,
                    openai_session_affinity_ttl_hours: 24,
                    responses_max_decompressed_body_bytes: 64 * 1024 * 1024,
                    smart_scheduling: gateway_core::account::SmartSchedulingConfig::default(),
                    min_codex_desktop_version: None,
                    min_codex_cli_version: None,
                    usage_retention_days: 31,
                    ops_event_retention_days: 30,
                    audit_retention_days: 30,
                    account_auto_freeze_enabled: true,
                    account_auto_freeze_threshold: 12,
                    account_auto_freeze_window_seconds: 600,
                    account_auto_freeze_duration_seconds: 7_200,
                    account_auto_freeze_probe_enabled: true,
                    account_auto_freeze_probe_model: None,
                    account_auto_freeze_adaptive_concurrency: true,
                    account_warmup_enabled: false,
                    account_warmup_schedule_time: "08:00".to_owned(),
                    account_warmup_model: None,
                },
            },
        )
        .await
        .expect_err("invalid settings");

    assert_eq!(error.kind(), AdminErrorKind::Invalid);
}

fn unused() -> AdminStoreError {
    AdminStoreError::new(
        AdminStoreErrorKind::Unavailable,
        "settings",
        "unused in this test",
    )
}
