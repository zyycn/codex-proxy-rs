//! Runtime settings 与管理员 API Key 用例。

use std::sync::Arc;

use async_trait::async_trait;
use gateway_core::routing::snapshot::SnapshotControl;
use rand_core::{OsRng, RngCore as _};

use crate::{
    model::{
        AdminError, MutationContext,
        settings::{
            AdminApiKey, AdminApiKeyMutation, RegeneratedAdminApiKey, ReplaceRuntimeSettings,
            RuntimeSettings,
        },
    },
    ports::store::SettingsStore,
};

use super::{map_store_error, publish_committed};

/// API 消费的 Runtime settings 管理服务。
#[async_trait]
pub trait SettingsService: Send + Sync {
    async fn load(&self) -> Result<RuntimeSettings, AdminError>;
    async fn replace(
        &self,
        context: &MutationContext,
        command: ReplaceRuntimeSettings,
    ) -> Result<RuntimeSettings, AdminError>;
    async fn admin_api_key_exists(&self) -> Result<bool, AdminError>;
    async fn regenerate_admin_api_key(
        &self,
        context: &MutationContext,
    ) -> Result<RegeneratedAdminApiKey, AdminError>;
    async fn delete_admin_api_key(
        &self,
        context: &MutationContext,
    ) -> Result<AdminApiKeyMutation, AdminError>;
}

pub(crate) struct DefaultSettingsService {
    store: Arc<dyn SettingsStore>,
    snapshot: Arc<dyn SnapshotControl>,
}

impl DefaultSettingsService {
    #[must_use]
    pub(crate) fn new(store: Arc<dyn SettingsStore>, snapshot: Arc<dyn SnapshotControl>) -> Self {
        Self { store, snapshot }
    }
}

#[async_trait]
impl SettingsService for DefaultSettingsService {
    async fn load(&self) -> Result<RuntimeSettings, AdminError> {
        self.store
            .load_runtime_settings()
            .await
            .map_err(|error| map_store_error(error, "runtime settings"))
    }

    async fn replace(
        &self,
        context: &MutationContext,
        command: ReplaceRuntimeSettings,
    ) -> Result<RuntimeSettings, AdminError> {
        validate_settings(&command)?;
        let settings = self
            .store
            .replace_runtime_settings(command, context)
            .await
            .map_err(|error| map_store_error(error, "runtime settings"))?;
        publish_committed(self.snapshot.as_ref(), settings.config_revision).await?;
        Ok(settings)
    }

    async fn admin_api_key_exists(&self) -> Result<bool, AdminError> {
        self.store
            .admin_api_key_exists()
            .await
            .map_err(|error| map_store_error(error, "administrator API key"))
    }

    async fn regenerate_admin_api_key(
        &self,
        context: &MutationContext,
    ) -> Result<RegeneratedAdminApiKey, AdminError> {
        let settings = self.load().await?;
        let mut bytes = [0_u8; 32];
        OsRng.fill_bytes(&mut bytes);
        let key = AdminApiKey::new(format!("admin-{}", hex::encode(bytes)));
        let mutation = self
            .store
            .replace_admin_api_key(settings.config_revision, key.clone(), context)
            .await
            .map_err(|error| map_store_error(error, "administrator API key"))?;
        publish_committed(self.snapshot.as_ref(), mutation.config_revision).await?;
        Ok(RegeneratedAdminApiKey { mutation, key })
    }

    async fn delete_admin_api_key(
        &self,
        context: &MutationContext,
    ) -> Result<AdminApiKeyMutation, AdminError> {
        let settings = self.load().await?;
        let mutation = self
            .store
            .delete_admin_api_key(settings.config_revision, context)
            .await
            .map_err(|error| map_store_error(error, "administrator API key"))?;
        publish_committed(self.snapshot.as_ref(), mutation.config_revision).await?;
        Ok(mutation)
    }
}

fn validate_settings(command: &ReplaceRuntimeSettings) -> Result<(), AdminError> {
    let valid = command.refresh_margin_seconds > 0
        && command.refresh_concurrency > 0
        && command.max_concurrent_per_account > 0
        && command.usage_retention_days >= 31
        && command.ops_event_retention_days > 0
        && command.audit_retention_days > 0
        && i64::try_from(command.request_interval_ms).is_ok();
    if valid {
        Ok(())
    } else {
        Err(AdminError::invalid("Runtime settings violate constraints"))
    }
}
