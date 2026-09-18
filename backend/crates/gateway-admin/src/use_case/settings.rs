//! Runtime settings 与管理员 API Key 用例。

use std::sync::Arc;

use async_trait::async_trait;
use gateway_core::policy::CodexClientVersion;
use gateway_core::runtime::SnapshotControl;
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
    async fn client_profile_options(
        &self,
    ) -> Result<gateway_core::account::OpaqueProviderData, AdminError> {
        Err(AdminError::invalid("当前 Provider 不支持客户端身份配置"))
    }
    async fn preview_client_profile(
        &self,
        _configuration: Option<&gateway_core::account::OpaqueProviderData>,
    ) -> Result<gateway_core::account::OpaqueProviderData, AdminError> {
        Err(AdminError::invalid("当前 Provider 不支持客户端身份配置"))
    }
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
    profile_provider: Arc<dyn crate::ports::provider::ProviderAdmin>,
    store: Arc<dyn SettingsStore>,
    snapshot: Arc<dyn SnapshotControl>,
}

impl DefaultSettingsService {
    #[must_use]
    pub(crate) fn new(
        store: Arc<dyn SettingsStore>,
        snapshot: Arc<dyn SnapshotControl>,
        profile_provider: Arc<dyn crate::ports::provider::ProviderAdmin>,
    ) -> Self {
        Self {
            store,
            snapshot,
            profile_provider,
        }
    }
}

#[async_trait]
impl SettingsService for DefaultSettingsService {
    async fn client_profile_options(
        &self,
    ) -> Result<gateway_core::account::OpaqueProviderData, AdminError> {
        let mut options = self
            .profile_provider
            .client_profile_options()
            .map_err(|error| super::map_provider_error(error, "client profile"))?
            .into_inner();
        let configuration = self
            .load()
            .await?
            .openai_client_profile
            .ok_or_else(|| AdminError::internal("通用客户端身份尚未初始化"))?;
        options.insert(
            "globalConfiguration".to_owned(),
            serde_json::Value::Object(configuration.into_inner()),
        );
        Ok(gateway_core::account::OpaqueProviderData::new(options))
    }

    async fn preview_client_profile(
        &self,
        configuration: Option<&gateway_core::account::OpaqueProviderData>,
    ) -> Result<gateway_core::account::OpaqueProviderData, AdminError> {
        let global;
        let (configuration, source) = if let Some(configuration) = configuration {
            (configuration, "override")
        } else {
            global = self
                .load()
                .await?
                .openai_client_profile
                .ok_or_else(|| AdminError::internal("通用客户端身份尚未初始化"))?;
            (&global, "global")
        };
        let mut preview = self
            .profile_provider
            .preview_client_profile(configuration)
            .map_err(|error| super::map_provider_error(error, "client profile"))?
            .into_inner();
        preview.insert("source".to_owned(), serde_json::Value::from(source));
        Ok(gateway_core::account::OpaqueProviderData::new(preview))
    }

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
        if let Some(profile) = &command.openai_client_profile {
            self.profile_provider
                .preview_client_profile(profile)
                .map_err(|error| super::map_provider_error(error, "client profile"))?;
        }
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
        let mut bytes = [0_u8; 32];
        OsRng.fill_bytes(&mut bytes);
        let key = AdminApiKey::new(format!("admin-{}", hex::encode(bytes)));
        let mutation = self
            .store
            .replace_admin_api_key(key.clone(), context)
            .await
            .map_err(|error| map_store_error(error, "administrator API key"))?;
        publish_committed(self.snapshot.as_ref(), mutation.config_revision).await?;
        Ok(RegeneratedAdminApiKey { mutation, key })
    }

    async fn delete_admin_api_key(
        &self,
        context: &MutationContext,
    ) -> Result<AdminApiKeyMutation, AdminError> {
        let mutation = self
            .store
            .delete_admin_api_key(context)
            .await
            .map_err(|error| map_store_error(error, "administrator API key"))?;
        publish_committed(self.snapshot.as_ref(), mutation.config_revision).await?;
        Ok(mutation)
    }
}

fn validate_settings(command: &ReplaceRuntimeSettings) -> Result<(), AdminError> {
    let valid = command.request_location.validate().is_ok()
        && command.responses_max_decompressed_body_bytes > 0
        && isize::try_from(command.responses_max_decompressed_body_bytes).is_ok()
        && command.refresh_margin_seconds > 0
        && command.refresh_concurrency > 0
        && command.max_concurrent_per_account > 0
        && command.max_waiting_per_key <= 1_000
        && command.max_waiting_per_account <= 1_000
        && (1..=120).contains(&command.concurrency_wait_timeout_seconds)
        && command.usage_retention_days >= 31
        && command.ops_event_retention_days > 0
        && command.audit_retention_days > 0
        && valid_client_version(command.min_codex_desktop_version.as_deref())
        && valid_client_version(command.min_codex_cli_version.as_deref())
        && valid_probe_model(command.account_auto_freeze_probe_model.as_deref())
        && i64::try_from(command.request_interval_ms).is_ok()
        && (2..=1_000).contains(&command.account_auto_freeze_threshold)
        && (60..=3_600).contains(&command.account_auto_freeze_window_seconds)
        && (300..=604_800).contains(&command.account_auto_freeze_duration_seconds);
    if valid {
        Ok(())
    } else {
        Err(AdminError::invalid("运行时设置不满足约束"))
    }
}

fn valid_client_version(value: Option<&str>) -> bool {
    value.is_none_or(|value| CodexClientVersion::parse(value).is_ok())
}

fn valid_probe_model(value: Option<&str>) -> bool {
    value.is_none_or(|value| {
        !value.is_empty()
            && value.len() <= 128
            && value == value.trim()
            && !value.bytes().any(|byte| byte.is_ascii_control())
    })
}
