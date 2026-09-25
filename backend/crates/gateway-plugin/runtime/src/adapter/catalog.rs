use std::{collections::BTreeSet, time::Duration};

use gateway_admin::model::{
    AdminError,
    plugins::instances::{PluginCapabilityBinding, PluginInstance},
};
use gateway_core::routing::{ContributedModelAlias, ProviderKind, PublicModelId, UpstreamModelId};
use gateway_plugin_sdk::{Capability, Manifest, Stage, call::catalog::ModelCatalogRegistration};

use crate::RpcSession;

pub(crate) async fn prepare(
    manifest: &Manifest,
    instance: &PluginInstance,
    session: &RpcSession,
) -> Result<Vec<ContributedModelAlias>, AdminError> {
    if !manifest.contributes.contains_key(&Capability::ModelCatalog) {
        return Ok(Vec::new());
    }
    validate_bindings(manifest, &instance.bindings)?;
    let reply = session
        .call(
            "model_catalog.register",
            session.context(Stage::Registration, Duration::from_secs(5)),
            serde_json::json!({}),
            Vec::new(),
        )
        .await
        .map_err(|_| AdminError::invalid("插件模型目录注册失败"))?;
    if reply.result != serde_json::json!({}) || reply.payload.len() > 64 * 1024 {
        return Err(AdminError::invalid("插件模型目录信封无效或过大"));
    }
    let registration: ModelCatalogRegistration = serde_json::from_slice(&reply.payload)
        .map_err(|_| AdminError::invalid("插件模型目录声明无效"))?;
    if registration.models.len() > 256 {
        return Err(AdminError::invalid("单个插件最多贡献 256 个模型别名"));
    }
    let mut ids = BTreeSet::new();
    registration
        .models
        .into_iter()
        .map(|model| {
            if !matches!(model.provider.as_str(), "openai" | "xai") || model.id == model.model {
                return Err(AdminError::invalid(
                    "模型别名必须指向内置 Provider 的不同上游模型",
                ));
            }
            let id = PublicModelId::new(model.id)
                .map_err(|_| AdminError::invalid("模型别名 ID 无效"))?;
            if !ids.insert(id.clone()) {
                return Err(AdminError::invalid(format!("插件模型别名重复：{id}")));
            }
            Ok(ContributedModelAlias {
                owner: instance.id.clone(),
                id,
                provider: ProviderKind::new(model.provider)
                    .map_err(|_| AdminError::invalid("模型别名 Provider 无效"))?,
                target: UpstreamModelId::new(model.model)
                    .map_err(|_| AdminError::invalid("模型别名目标无效"))?,
            })
        })
        .collect()
}

pub(crate) fn validate_bindings(
    manifest: &Manifest,
    bindings: &[PluginCapabilityBinding],
) -> Result<(), AdminError> {
    // 目录随实例完整发布；不接受请求级绑定，以免展示与执行的可见范围分叉。
    if let Some(declaration) = manifest.contributes.get(&Capability::ModelCatalog)
        && bindings
            .iter()
            .any(|binding| binding.contribution == declaration.id)
    {
        return Err(AdminError::invalid("模型目录随实例发布，不支持请求级绑定"));
    }
    Ok(())
}
