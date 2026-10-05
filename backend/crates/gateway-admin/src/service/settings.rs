//! 设置服务的唯一公开操作声明；SDK 操作和数据合同由对应合同测试生成

use crate::{
    SettingsService,
    model::{
        AdminError, MutationContext,
        pricing::{SyncPricing, UpdatePricing},
        settings::ReplaceRuntimeSettings,
    },
};
use std::sync::Arc;

pub(super) fn register(
    registry: &mut super::Registry,
    service: &Arc<dyn SettingsService>,
) -> Result<(), AdminError> {
    // 声明保留 SDK 类型名和原生参数转换，分派名称由业务方法名直接派生
    macro_rules! register {
        ($sdk:ident, $method:ident, $input:ty, $pattern:pat $(, $argument:expr)* $(,)?) => {{
            let service = service.clone();
            registry.register(concat!("settings.", stringify!($method)), move |$pattern: $input| {
                let service = service.clone();
                Box::pin(async move { service.$method($($argument),*).await })
            })?;
        }};
    }
    register!(Load, load, (), ());
    register!(
        Replace,
        replace,
        (MutationContext, ReplaceRuntimeSettings),
        (context, command),
        &context,
        command
    );
    register!(ApiKeyExists, admin_api_key_exists, (), ());
    register!(
        RegenerateApiKey,
        regenerate_admin_api_key,
        MutationContext,
        context,
        &context
    );
    register!(
        DeleteApiKey,
        delete_admin_api_key,
        MutationContext,
        context,
        &context
    );
    register!(Pricing, pricing, (), ());
    register!(PreviewPricingSync, preview_pricing_sync, (), ());
    register!(
        Sync,
        sync_pricing,
        (MutationContext, SyncPricing),
        (context, command),
        &context,
        command
    );
    register!(
        Update,
        update_pricing,
        (MutationContext, UpdatePricing),
        (context, command),
        &context,
        command
    );
    register!(
        ClientProfileOptions,
        client_profile_options,
        String,
        provider,
        &provider
    );
    register!(
        PreviewClientProfile,
        preview_client_profile,
        (String, Option<gateway_core::account::OpaqueProviderData>),
        (provider, configuration),
        &provider,
        configuration.as_ref()
    );
    Ok(())
}
