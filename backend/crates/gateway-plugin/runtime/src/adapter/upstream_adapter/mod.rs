//! 将插件上游适配贡献编译为可按请求选择的执行计划

mod event;
mod execution;
mod registration;
pub(crate) mod target;

use std::{fmt, sync::Arc};

use gateway_admin::model::AdminError;
use gateway_core::{
    engine::{
        AttemptContext,
        upstream_adapter::{UpstreamAdapter, UpstreamAdapterInvocation, UpstreamAdapterPlan},
    },
    error::{ProviderError, ProviderErrorKind},
    identity::ProviderKind,
    routing::UpstreamModelId,
    upstream::UpstreamSendState,
};
use gateway_plugin_sdk::call::upstream_adapter::UpstreamAdapterDeclaration;

use super::scope::BindingScope;
use crate::{RpcSession, callback::PluginCallbacks};

pub(crate) use registration::{prepare, unavailable_entries, validate_bindings};

pub(crate) struct AdapterEntry {
    instance_id: String,
    scope: BindingScope,
    adapter: Option<Arc<PluginUpstreamAdapter>>,
}

pub(crate) struct PluginUpstreamAdapter {
    instance_id: String,
    contribution_id: String,
    declaration: UpstreamAdapterDeclaration,
    target: Arc<target::UpstreamTarget>,
    session: Arc<RpcSession>,
    callbacks: Arc<PluginCallbacks>,
    connections: Arc<crate::callback::upstream::ConnectionPool>,
}

impl fmt::Debug for PluginUpstreamAdapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PluginUpstreamAdapter")
            .field("instance_id", &self.instance_id)
            .field("adapter_id", &self.declaration.id)
            .finish_non_exhaustive()
    }
}

impl UpstreamAdapter for PluginUpstreamAdapter {
    fn transport(&self) -> &str {
        self.declaration.transport.as_str()
    }

    fn execute(
        self: Arc<Self>,
        invocation: UpstreamAdapterInvocation,
    ) -> gateway_core::engine::provider::EventStream {
        execution::execute(self, invocation)
    }
}

pub(crate) struct PluginUpstreamAdapterPlan {
    entries: Vec<AdapterEntry>,
}

impl PluginUpstreamAdapterPlan {
    pub(crate) fn compile(
        entries: Vec<AdapterEntry>,
    ) -> Result<Option<Arc<dyn UpstreamAdapterPlan>>, AdminError> {
        for (index, left) in entries.iter().enumerate() {
            for right in &entries[index + 1..] {
                if left.scope.overlaps(&right.scope)
                    && adapters_overlap(left.adapter.as_deref(), right.adapter.as_deref())
                {
                    return Err(AdminError::invalid(
                        "上游适配器的 Provider 与模型绑定范围重叠",
                    ));
                }
            }
        }
        Ok((!entries.is_empty())
            .then(|| Arc::new(Self { entries }) as Arc<dyn UpstreamAdapterPlan>))
    }
}

impl fmt::Debug for PluginUpstreamAdapterPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PluginUpstreamAdapterPlan")
            .field("adapters", &self.entries.len())
            .finish()
    }
}

impl UpstreamAdapterPlan for PluginUpstreamAdapterPlan {
    fn select(
        &self,
        context: &AttemptContext,
        provider: &ProviderKind,
        model: &UpstreamModelId,
    ) -> Result<Option<Arc<dyn UpstreamAdapter>>, ProviderError> {
        let model = context
            .requested_model()
            .map_or(model.as_str(), |model| model.as_str());
        for entry in &self.entries {
            if context.extension_scope().contains(&entry.instance_id)
                || !entry.scope.matches_processing(
                    context.client_api_key_ref(),
                    context.account_group_ids(),
                    Some(provider),
                    Some(model),
                )
            {
                continue;
            }
            let adapter = entry.adapter.as_ref().ok_or_else(unavailable)?;
            if adapter.declaration.provider.as_str() == provider.as_str()
                && (adapter.declaration.models.is_empty()
                    || adapter
                        .declaration
                        .models
                        .iter()
                        .any(|candidate| candidate == model))
            {
                if !adapter.session.is_ready() {
                    return Err(unavailable());
                }
                return Ok(Some(Arc::clone(adapter) as Arc<dyn UpstreamAdapter>));
            }
        }
        Ok(None)
    }
}

fn adapters_overlap(
    left: Option<&PluginUpstreamAdapter>,
    right: Option<&PluginUpstreamAdapter>,
) -> bool {
    let (Some(left), Some(right)) = (left, right) else {
        return true;
    };
    left.declaration.provider == right.declaration.provider
        && (left.declaration.models.is_empty()
            || right.declaration.models.is_empty()
            || left
                .declaration
                .models
                .iter()
                .any(|model| right.declaration.models.contains(model)))
}

fn unavailable() -> ProviderError {
    ProviderError::new(ProviderErrorKind::Unavailable, UpstreamSendState::NotSent)
}
