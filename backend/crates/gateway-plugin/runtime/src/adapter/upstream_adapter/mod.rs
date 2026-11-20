//! 将插件上游适配贡献编译为按请求与账号范围解析的执行计划

mod event;
mod execution;
mod failure;
mod observation;
mod registration;
pub(crate) mod target;

use std::{fmt, sync::Arc};

use gateway_core::{
    account::ProviderAccountId,
    engine::ModelRouteScope,
    engine::upstream_adapter::{
        UpstreamAdapter, UpstreamAdapterInvocation, UpstreamAdapterPlan, UpstreamAdapterSelection,
    },
    error::{ProviderError, ProviderErrorKind},
    identity::ProviderKind,
    upstream::UpstreamSendState,
};
use gateway_plugin_sdk::call::upstream_adapter::UpstreamAdapterDeclaration;

use super::scope::BindingScope;
use crate::{RpcSession, callback::PluginCallbacks};

pub(crate) use registration::{prepare, unavailable_entries, validate_bindings};

#[derive(Clone)]
pub(crate) struct AdapterEntry {
    instance_id: String,
    scope: BindingScope,
    adapter: Option<Arc<PluginUpstreamAdapter>>,
}

impl AdapterEntry {
    pub(crate) fn catalog_target(
        &self,
        id: &str,
        model: &str,
    ) -> Option<&UpstreamAdapterDeclaration> {
        let adapter = self.adapter.as_ref()?;
        let declaration = &adapter.declaration;
        let provider = ProviderKind::new(declaration.provider.as_str()).ok()?;
        (declaration.id == id
            && !declaration.account_ids.is_empty()
            && (declaration.models.is_empty()
                || declaration.models.iter().any(|value| value == model))
            && self.scope.matches_catalog(&provider, model))
        .then_some(declaration)
    }
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

    fn supports_authentication(&self, kind: &str) -> bool {
        self.declaration
            .authentication_kinds
            .iter()
            .any(|candidate| candidate == kind)
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
    pub(crate) fn compile(entries: Vec<AdapterEntry>) -> Option<Arc<dyn UpstreamAdapterPlan>> {
        (!entries.is_empty()).then(|| Arc::new(Self { entries }) as Arc<dyn UpstreamAdapterPlan>)
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
        context: ModelRouteScope<'_>,
        provider: &ProviderKind,
        account: Option<&ProviderAccountId>,
    ) -> Result<UpstreamAdapterSelection, ProviderError> {
        let model = context.model;
        for entry in &self.entries {
            if context.extensions.contains(&entry.instance_id)
                || !entry.scope.matches_processing(
                    context.client_api_key,
                    context.account_groups,
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
                if !adapter.declaration.account_ids.is_empty() {
                    let Some(account) = account else {
                        // 后置声明只覆盖未命中此账号范围的候选，不能提前以其故障拒绝整个请求
                        return Ok(UpstreamAdapterSelection::PendingAccount);
                    };
                    if !adapter
                        .declaration
                        .account_ids
                        .iter()
                        .any(|id| id == account.as_str())
                    {
                        continue;
                    }
                }
                if !adapter.session.is_ready() {
                    return Err(unavailable());
                }
                return Ok(UpstreamAdapterSelection::Adapter(
                    Arc::clone(adapter) as Arc<dyn UpstreamAdapter>
                ));
            }
        }
        Ok(UpstreamAdapterSelection::Native)
    }
}

fn unavailable() -> ProviderError {
    ProviderError::new(ProviderErrorKind::Unavailable, UpstreamSendState::NotSent)
}
