use async_trait::async_trait;

use gateway_admin::{
    model::{
        AdminErrorKind, MutationContext, Revision,
        settings::{
            AdminApiKey, AdminApiKeyMutation, ReplaceRuntimeSettings, RotationStrategy,
            RuntimeSettings,
        },
    },
    ports::store::{AdminStoreError, AdminStoreErrorKind, AdminStoreResult, SettingsStore},
};

struct UnusedSettingsStore;

#[async_trait]
impl SettingsStore for UnusedSettingsStore {
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
        _: Revision,
        _: AdminApiKey,
        _: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation> {
        Err(unused())
    }

    async fn delete_admin_api_key(
        &self,
        _: Revision,
        _: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation> {
        Err(unused())
    }
}

#[tokio::test]
async fn settings_should_reject_zero_refresh_margin_before_store_call() {
    let services = super::AdminHarness::new()
        .settings(std::sync::Arc::new(UnusedSettingsStore))
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
                expected_config_revision: Revision::new(1).expect("revision"),
                provider_model_mappings: Default::default(),
                refresh_margin_seconds: 0,
                refresh_concurrency: 1,
                max_concurrent_per_account: 1,
                request_interval_ms: 0,
                rotation_strategy: RotationStrategy::Smart,
                usage_retention_days: 31,
                ops_event_retention_days: 30,
                audit_retention_days: 30,
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
