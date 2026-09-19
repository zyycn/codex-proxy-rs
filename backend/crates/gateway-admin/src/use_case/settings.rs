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
    async fn preview_pricing_sync(
        &self,
    ) -> Result<crate::model::pricing::PricingSyncPreview, AdminError>;
    async fn sync_pricing(
        &self,
        context: &MutationContext,
        command: crate::model::pricing::SyncPricing,
    ) -> Result<(), AdminError>;
    async fn pricing(&self) -> Result<crate::model::pricing::PricingCatalog, AdminError>;
    async fn update_pricing(
        &self,
        context: &MutationContext,
        command: crate::model::pricing::UpdatePricing,
    ) -> Result<(), AdminError>;
    async fn client_profile_options(
        &self,
        _provider: &str,
    ) -> Result<gateway_core::account::OpaqueProviderData, AdminError> {
        Err(AdminError::invalid("当前 Provider 不支持客户端身份配置"))
    }
    async fn preview_client_profile(
        &self,
        _provider: &str,
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
    providers: crate::ports::provider::ProviderAdminRegistry,
    pricing_source: Arc<dyn crate::ports::pricing::PricingSource>,
    store: Arc<dyn SettingsStore>,
    snapshot: Arc<dyn SnapshotControl>,
}

impl DefaultSettingsService {
    fn profile_provider(
        &self,
        provider: &str,
    ) -> Result<Arc<dyn crate::ports::provider::ProviderAdmin>, AdminError> {
        let kind = gateway_core::routing::ProviderKind::new(provider)
            .map_err(|_| AdminError::invalid("Provider 不合法"))?;
        self.providers
            .require(&kind)
            .map_err(|error| super::map_provider_error(error, "client profile"))
    }

    #[must_use]
    pub(crate) fn new(
        store: Arc<dyn SettingsStore>,
        snapshot: Arc<dyn SnapshotControl>,
        providers: crate::ports::provider::ProviderAdminRegistry,
        pricing_source: Arc<dyn crate::ports::pricing::PricingSource>,
    ) -> Self {
        Self {
            providers,
            pricing_source,
            store,
            snapshot,
        }
    }
}

#[async_trait]
impl SettingsService for DefaultSettingsService {
    async fn preview_pricing_sync(
        &self,
    ) -> Result<crate::model::pricing::PricingSyncPreview, AdminError> {
        self.pricing_source.fetch().await
    }

    async fn sync_pricing(
        &self,
        context: &MutationContext,
        command: crate::model::pricing::SyncPricing,
    ) -> Result<(), AdminError> {
        let count = command
            .models
            .values()
            .map(std::collections::BTreeSet::len)
            .sum::<usize>();
        if count == 0
            || count > 10_000
            || command
                .models
                .values()
                .any(std::collections::BTreeSet::is_empty)
        {
            return Err(AdminError::invalid("请选择 1 至 10000 个模型"));
        }
        let mut current = self.pricing_source.fetch().await?;
        if current != command.preview {
            return Err(AdminError::invalid(
                "models.dev 价目已变化，请重新预览后确认",
            ));
        }
        let stored = self
            .store
            .load_pricing()
            .await
            .map_err(|error| map_store_error(error, "model pricing"))?;
        let mut changes = crate::model::pricing::PricingSyncChanges::new();
        for (provider, models) in command.models {
            let selected = changes.entry(provider.clone()).or_default();
            for model in models {
                let price = current
                    .prices
                    .get_mut(&provider)
                    .and_then(|prices| prices.remove(&model));
                if price.is_none()
                    && !stored
                        .synced
                        .get(&provider)
                        .is_some_and(|prices| prices.contains_key(&model))
                {
                    return Err(AdminError::invalid("所选模型不在来源价目中，请重新预览"));
                }
                selected.insert(model, price);
            }
        }
        let revision = self
            .store
            .sync_pricing(changes, context)
            .await
            .map_err(|error| map_store_error(error, "model pricing sync"))?;
        publish_committed(self.snapshot.as_ref(), revision).await
    }

    async fn pricing(&self) -> Result<crate::model::pricing::PricingCatalog, AdminError> {
        let stored = self
            .store
            .load_pricing()
            .await
            .map_err(|error| map_store_error(error, "model pricing"))?;
        Ok(crate::model::pricing::PricingCatalog {
            defaults: self.providers.pricing_catalog(),
            overrides: stored.overrides,
            synced: stored.synced,
            synced_at: stored.synced_at,
        })
    }

    async fn update_pricing(
        &self,
        context: &MutationContext,
        command: crate::model::pricing::UpdatePricing,
    ) -> Result<(), AdminError> {
        use crate::model::pricing::PricingChange;
        let catalog = self.pricing().await?;
        if !catalog.defaults.contains_key(&command.provider)
            || command.models.is_empty()
            || command.models.len() > 500
            || command.models.iter().any(|model| {
                model.is_empty()
                    || model.len() > 128
                    || model.trim() != model
                    || model.chars().any(char::is_whitespace)
                    || model.chars().any(char::is_control)
            })
        {
            return Err(AdminError::invalid("Provider、模型 ID 或批量数量不合法"));
        }
        if command.change == PricingChange::Delete
            && command.models.iter().any(|model| {
                catalog
                    .defaults
                    .get(&command.provider)
                    .is_some_and(|models| models.contains_key(model))
            })
        {
            return Err(AdminError::invalid("内置价目不能删除，仅支持人工覆盖"));
        }
        let defaults = gateway_core::metering::merge_pricing(catalog.defaults, &catalog.synced);
        let defaults = defaults.get(&command.provider);
        match &command.change {
            PricingChange::Replace(pricing) => {
                pricing.validate().map_err(AdminError::invalid)?;
                if command.provider == "xai"
                    && pricing
                        .bands
                        .keys()
                        .any(|band| matches!(band.as_str(), "image" | "flex" | "long_flex"))
                {
                    return Err(AdminError::invalid("xAI 不支持此价格档位"));
                }
                if !pricing.bands.contains_key("standard")
                    && command
                        .models
                        .iter()
                        .any(|model| !defaults.is_some_and(|p| p.contains_key(model)))
                {
                    return Err(AdminError::invalid("未知模型必须提供标准档价格"));
                }
            }
            PricingChange::Multiplier(bps) if *bps > 1_000_000 => {
                return Err(AdminError::invalid("倍率必须在 0 至 100 倍之间"));
            }
            PricingChange::Multiplier(_)
                if command.models.iter().any(|model| {
                    !defaults.is_some_and(|p| p.contains_key(model))
                        && !catalog
                            .overrides
                            .get(&command.provider)
                            .and_then(|p| p.get(model))
                            .is_some_and(|p| p.bands.contains_key("standard"))
                }) =>
            {
                return Err(AdminError::invalid("请先为未知模型配置标准档价格"));
            }
            _ => {}
        }
        let revision = self
            .store
            .update_pricing(command, context)
            .await
            .map_err(|error| map_store_error(error, "model pricing"))?;
        publish_committed(self.snapshot.as_ref(), revision).await
    }

    async fn client_profile_options(
        &self,
        provider: &str,
    ) -> Result<gateway_core::account::OpaqueProviderData, AdminError> {
        let mut options = self
            .profile_provider(provider)?
            .client_profile_options()
            .map_err(|error| super::map_provider_error(error, "client profile"))?
            .into_inner();
        let configuration = self
            .load()
            .await?
            .client_profile(provider)
            .cloned()
            .ok_or_else(|| AdminError::internal("通用客户端身份尚未初始化"))?;
        options.insert(
            "globalConfiguration".to_owned(),
            serde_json::Value::Object(configuration.into_inner()),
        );
        Ok(gateway_core::account::OpaqueProviderData::new(options))
    }

    async fn preview_client_profile(
        &self,
        provider: &str,
        configuration: Option<&gateway_core::account::OpaqueProviderData>,
    ) -> Result<gateway_core::account::OpaqueProviderData, AdminError> {
        let profile_provider = self.profile_provider(provider)?;
        let global;
        let (configuration, source) = if let Some(configuration) = configuration {
            (configuration, "override")
        } else {
            global = self
                .load()
                .await?
                .client_profile(provider)
                .cloned()
                .ok_or_else(|| AdminError::internal("通用客户端身份尚未初始化"))?;
            (&global, "global")
        };
        let mut preview = profile_provider
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
        for (provider, profile) in [
            ("openai", &command.openai_client_profile),
            ("xai", &command.xai_client_profile),
        ] {
            if let Some(profile) = profile {
                self.profile_provider(provider)?
                    .preview_client_profile(profile)
                    .map_err(|error| super::map_provider_error(error, "client profile"))?;
            }
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
