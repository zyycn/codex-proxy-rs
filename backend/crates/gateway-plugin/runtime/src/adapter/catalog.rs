//! 编译插件模型目录贡献并校验能力绑定

use gateway_admin::model::{
    AdminError,
    plugins::instances::{PluginCapabilityBinding, PluginInstance},
};
use gateway_core::{
    account::ProviderAccountId,
    operation::{Feature, OperationKind},
    routing::{
        ContributedModel, ContributedModelAlias, ModelCapabilities, ModelPresentation,
        ProviderKind, PublicModelId, SupportLevel, UpstreamModelId,
    },
};
use gateway_plugin_sdk::{
    Capability, Manifest, Stage,
    call::{catalog::ModelCatalogRegistrationV2, middleware::RequestFeature},
};

use crate::RpcSession;

pub(crate) struct PreparedCatalog {
    pub(crate) aliases: Vec<ContributedModelAlias>,
    pub(crate) models: Vec<ContributedModel>,
}

pub(crate) async fn prepare(
    manifest: &Manifest,
    instance: &PluginInstance,
    session: &RpcSession,
    adapters: &[super::upstream_adapter::AdapterEntry],
) -> Result<PreparedCatalog, AdminError> {
    if !manifest.contributes.contains_key(&Capability::ModelCatalog) {
        return Ok(PreparedCatalog {
            aliases: Vec::new(),
            models: Vec::new(),
        });
    }
    validate_bindings(manifest, &instance.bindings)?;
    let reply = session
        .call(
            "model_catalog.register",
            session.context(Stage::Registration, session.budget(Stage::Registration)),
            serde_json::json!({}),
            Vec::new(),
        )
        .await
        .map_err(|_| AdminError::invalid("插件模型目录注册失败"))?;
    if reply.payload.len() > 64 * 1024 {
        return Err(AdminError::invalid("插件模型目录信封无效或过大"));
    }
    let ModelCatalogRegistrationV2 { aliases, models } = serde_json::from_slice(&reply.payload)
        .map_err(|_| AdminError::invalid("插件模型目录声明无效"))?;
    if aliases.len().saturating_add(models.len()) > 256 {
        return Err(AdminError::invalid("单个插件最多贡献 256 个模型条目"));
    }
    let aliases = aliases
        .into_iter()
        .map(|model| {
            let id = PublicModelId::new(model.id)
                .map_err(|_| AdminError::invalid("模型别名 ID 无效"))?;
            Ok(ContributedModelAlias {
                owner: instance.id.clone(),
                id,
                provider: ProviderKind::new(model.provider)
                    .map_err(|_| AdminError::invalid("模型别名 Provider 无效"))?,
                target: UpstreamModelId::new(model.model)
                    .map_err(|_| AdminError::invalid("模型别名目标无效"))?,
            })
        })
        .collect::<Result<_, AdminError>>()?;
    let models = models
        .into_iter()
        .map(|model| {
            let mut targets = adapters
                .iter()
                .filter_map(|entry| entry.catalog_target(&model.adapter_id, &model.id));
            let target = targets
                .next()
                .filter(|_| targets.next().is_none())
                .ok_or_else(|| AdminError::invalid("适配模型必须关联同实例的静态账号适配器"))?;
            let mut capabilities =
                ModelCapabilities::new([OperationKind::Generate].into(), model.max_output_tokens);
            for feature in model.features {
                let feature = match feature {
                    RequestFeature::Tools => Feature::Tools,
                    RequestFeature::Vision => Feature::Vision,
                    RequestFeature::Reasoning => Feature::Reasoning,
                    RequestFeature::JsonSchema => Feature::JsonSchema,
                };
                capabilities = capabilities.with_feature(feature, SupportLevel::Native);
            }
            let p = model.presentation;
            Ok(ContributedModel {
                owner: instance.id.clone(),
                id: PublicModelId::new(model.id)
                    .map_err(|_| AdminError::invalid("适配模型 ID 无效"))?,
                provider: ProviderKind::new(target.provider.as_str())
                    .map_err(|_| AdminError::invalid("适配模型 Provider 无效"))?,
                accounts: target
                    .account_ids
                    .iter()
                    .map(|id| {
                        ProviderAccountId::new(id.clone())
                            .map_err(|_| AdminError::invalid("适配模型账号无效"))
                    })
                    .collect::<Result<_, _>>()?,
                capabilities,
                presentation: ModelPresentation::new(p.display_name, p.description)
                    .with_reasoning(p.default_reasoning_effort, p.supported_reasoning_efforts)
                    .with_reasoning_summary(p.reasoning_summary)
                    .with_context_window_tokens(p.context_window_tokens)
                    .with_max_context_window_tokens(p.max_context_window_tokens)
                    .with_image_input(p.image_input)
                    .with_agent_tools(p.agent_tools, p.parallel_tool_calls)
                    .with_search_tool(p.search_tool)
                    .with_image_detail_original(p.image_detail_original)
                    .with_verbosity(p.verbosity)
                    .with_hidden(p.hidden),
            })
        })
        .collect::<Result<_, AdminError>>()?;
    Ok(PreparedCatalog { aliases, models })
}

/// 新目录的同名画像必须一致，不由注册顺序拼接能力或改变别名目标
pub(crate) fn validate_models(
    aliases: &[ContributedModelAlias],
    models: &[ContributedModel],
) -> Result<(), AdminError> {
    for (index, model) in models.iter().enumerate() {
        if aliases.iter().any(|alias| alias.id == model.id)
            || models[..index].iter().any(|other| {
                other.id == model.id
                    && (other.provider != model.provider
                        || other.presentation != model.presentation
                        || other.capabilities != model.capabilities
                        || !other.accounts.is_disjoint(&model.accounts))
            })
        {
            return Err(AdminError::invalid("适配模型目录声明冲突"));
        }
    }
    Ok(())
}

pub(crate) fn validate_bindings(
    manifest: &Manifest,
    bindings: &[PluginCapabilityBinding],
) -> Result<(), AdminError> {
    // 目录随实例完整发布；不接受请求级绑定，以免展示与执行的可见范围分叉
    if let Some(declaration) = manifest.contributes.get(&Capability::ModelCatalog)
        && bindings
            .iter()
            .any(|binding| binding.contribution == declaration.id)
    {
        return Err(AdminError::invalid("模型目录随实例发布，不支持请求级绑定"));
    }
    Ok(())
}
