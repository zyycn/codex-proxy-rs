//! 校验并准备插件上游适配声明与能力绑定

use std::{collections::BTreeSet, sync::Arc, time::Duration};

use gateway_admin::model::{
    AdminError,
    plugins::instances::{PluginCapabilityBinding, PluginFailurePolicy, PluginInstance},
};
use gateway_plugin_sdk::{
    Capability, Manifest, Stage,
    call::upstream_adapter::{REGISTER_METHOD, UpstreamAdapterRegistration},
};

use super::{AdapterEntry, PluginUpstreamAdapter, target::UpstreamTarget};
use crate::{RpcSession, adapter::scope::BindingScope, callback::PluginCallbacks};

pub(crate) fn validate_bindings(
    manifest: &Manifest,
    bindings: &[PluginCapabilityBinding],
) -> Result<(), AdminError> {
    let Some(declaration) = manifest.contributes.get(&Capability::UpstreamAdapter) else {
        return Ok(());
    };
    let mut found = false;
    for binding in bindings
        .iter()
        .filter(|binding| binding.contribution == declaration.id)
    {
        if std::mem::replace(&mut found, true)
            || binding.stage != "upstream"
            || binding.failure_policy != PluginFailurePolicy::Reject
            || binding
                .provider_ids
                .iter()
                .any(|provider| !matches!(provider.as_str(), "openai" | "xai"))
        {
            return Err(AdminError::invalid(
                "上游适配器只允许一次 upstream 绑定，使用 reject 策略及内置 Provider 范围",
            ));
        }
        BindingScope::compile(binding)?;
    }
    Ok(())
}

pub(crate) async fn prepare(
    manifest: &Manifest,
    instance: &PluginInstance,
    session: Arc<RpcSession>,
    callbacks: Arc<PluginCallbacks>,
) -> Result<Vec<AdapterEntry>, AdminError> {
    let Some(contribution) = manifest.contributes.get(&Capability::UpstreamAdapter) else {
        return Ok(Vec::new());
    };
    validate_bindings(manifest, &instance.bindings)?;
    let reply = session
        .call(
            REGISTER_METHOD,
            session.context(Stage::Registration, Duration::from_secs(5)),
            serde_json::json!({}),
            vec![],
        )
        .await
        .map_err(|_| AdminError::invalid("上游适配器注册失败"))?;
    if reply.result != serde_json::json!({}) || reply.payload.len() > 64 * 1024 {
        return Err(AdminError::invalid("上游适配器注册信封无效或过大"));
    }
    let registration: UpstreamAdapterRegistration = serde_json::from_slice(&reply.payload)
        .map_err(|_| AdminError::invalid("上游适配器声明无效"))?;
    if registration.adapters.is_empty() || registration.adapters.len() > 16 {
        return Err(AdminError::invalid("单个插件须声明 1 至 16 个上游适配器"));
    }
    let mut ids = BTreeSet::new();
    let binding = instance
        .bindings
        .iter()
        .find(|binding| binding.contribution == contribution.id);
    let mut entries = Vec::new();
    for declaration in registration.adapters {
        if declaration.id.is_empty()
            || declaration.id.len() > 64
            || !declaration
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
            || !ids.insert(declaration.id.clone())
            || declaration.authentication_kinds.is_empty()
            || declaration.authentication_kinds.len() > 8
            || declaration.authentication_kinds.iter().any(|kind| {
                kind.is_empty()
                    || kind.len() > 64
                    || !kind
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            })
            || !contribution.input_formats.contains(&declaration.protocol)
            || !contribution.output_formats.contains(&declaration.protocol)
            || declaration.models.len() > 256
        {
            return Err(AdminError::invalid("上游适配器身份、认证或协议范围无效"));
        }
        let mut models = BTreeSet::new();
        for model in &declaration.models {
            gateway_core::routing::PublicModelId::new(model.clone())
                .map_err(|_| AdminError::invalid("上游适配器模型无效"))?;
            if !models.insert(model) {
                return Err(AdminError::invalid("上游适配器模型重复"));
            }
        }
        let target = Arc::new(UpstreamTarget::compile(&declaration)?);
        if let Some(binding) = binding {
            entries.push(AdapterEntry {
                instance_id: instance.id.clone(),
                scope: BindingScope::compile(binding)?,
                adapter: Some(Arc::new(PluginUpstreamAdapter {
                    instance_id: instance.id.clone(),
                    contribution_id: contribution.id.clone(),
                    declaration,
                    target,
                    session: Arc::clone(&session),
                    callbacks: Arc::clone(&callbacks),
                    connections: Arc::default(),
                })),
            });
        }
    }
    Ok(entries)
}

pub(crate) fn unavailable_entries(
    instance: &PluginInstance,
) -> Result<Vec<AdapterEntry>, AdminError> {
    instance
        .bindings
        .iter()
        .filter(|binding| binding.stage == "upstream")
        .map(|binding| {
            Ok(AdapterEntry {
                instance_id: instance.id.clone(),
                scope: BindingScope::compile(binding)?,
                adapter: None,
            })
        })
        .collect()
}
