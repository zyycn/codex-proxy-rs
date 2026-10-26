//! 为具体入口登记回调作用域和一次性中间件绑定，不持有 RPC 调度状态

use std::sync::{Arc, Weak};

use gateway_core::{account::OutboundProxy, lifecycle::CancellationToken};
use gateway_plugin_sdk::{CallContext, PluginFault};

use super::{
    CallbackScope, InvocationContext, MiddlewareBinding, MiddlewareCallback, PluginCallbacks,
    denied, upstream,
};

impl PluginCallbacks {
    pub(crate) fn bind_middleware(
        self: &Arc<Self>,
        resource_scope_id: String,
        invocation: Arc<dyn MiddlewareCallback>,
        scope: Arc<CallbackScope>,
    ) -> Result<MiddlewareBinding, gateway_core::engine::middleware::MiddlewareError> {
        let mut pending = self
            .pending_middleware
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if pending
            .insert(resource_scope_id.clone(), Arc::clone(&invocation))
            .is_some()
        {
            return Err(gateway_core::engine::middleware::MiddlewareError::InvalidState);
        }
        Ok(MiddlewareBinding::new(
            Arc::clone(self),
            resource_scope_id,
            invocation,
            scope,
        ))
    }

    pub(super) fn unbind_middleware(
        &self,
        resource_scope_id: &str,
        invocation: &Arc<dyn MiddlewareCallback>,
    ) {
        let mut pending = self
            .pending_middleware
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if pending
            .get(resource_scope_id)
            .is_some_and(|pending| Arc::ptr_eq(pending, invocation))
        {
            pending.remove(resource_scope_id);
        }
    }

    pub(crate) fn prepare_data_plane(
        &self,
        context: &CallContext,
        effects: Option<Arc<gateway_core::engine::nested::ExecutionEffects>>,
        extensions: gateway_core::engine::extensions::ExtensionCallScope,
        plan: Option<gateway_core::engine::middleware::FrozenMiddlewarePlan>,
        cancellation: CancellationToken,
        request_body_limit: Option<usize>,
    ) -> Result<Arc<CallbackScope>, PluginFault> {
        if !matches!(
            context.stage,
            gateway_plugin_sdk::Stage::Request
                | gateway_plugin_sdk::Stage::Attempt
                | gateway_plugin_sdk::Stage::Outbound
                | gateway_plugin_sdk::Stage::Routing
                | gateway_plugin_sdk::Stage::Scheduling
                | gateway_plugin_sdk::Stage::Retry
        ) {
            return Err(denied());
        }
        let mut scope = CallbackScope::for_call(context);
        if let Some(effects) = effects {
            scope = scope.with_execution_effects(effects);
        }
        scope.extension_scope = extensions;
        scope.request_body_limit = request_body_limit;
        scope.origin = plan.map(|plan| InvocationContext {
            request_id: context
                .request_id
                .clone()
                .unwrap_or_else(|| context.resource_scope_id.clone()),
            call_id: context
                .request_id
                .clone()
                .unwrap_or_else(|| context.resource_scope_id.clone()),
            cancellation,
            extensions: scope.extension_scope.clone(),
            effects: scope.execution_effects.clone(),
            plan: Some(plan),
        });
        self.register_scope(context, scope)
    }

    pub(crate) fn request_body_limit(&self) -> Result<usize, PluginFault> {
        self.request_limits.current_body_bytes()
    }

    pub(crate) fn prepare_upstream(
        &self,
        context: &CallContext,
        managed: Arc<upstream::ManagedUpstream>,
        extension_scope: gateway_core::engine::extensions::ExtensionCallScope,
        request_body_limit: usize,
    ) -> Result<Arc<CallbackScope>, PluginFault> {
        if context.stage != gateway_plugin_sdk::Stage::Upstream
            || context.account_id.as_deref() != Some(managed.account.account_id().as_str())
            || context.credential_revision != Some(managed.account.credential_revision().get())
        {
            return Err(denied());
        }
        let mut scope = CallbackScope::new(context, managed.account.outbound_proxy().cloned())
            .with_upstream(managed);
        scope.extension_scope = extension_scope;
        scope.request_body_limit = Some(request_body_limit);
        self.register_scope(context, scope)
    }

    pub(crate) fn prepare_management(
        &self,
        context: &CallContext,
        proxy: Option<OutboundProxy>,
        request: Option<Arc<gateway_core::engine::extensions::RequestContext>>,
    ) -> Result<Arc<CallbackScope>, PluginFault> {
        if !matches!(
            context.stage,
            gateway_plugin_sdk::Stage::Management | gateway_plugin_sdk::Stage::PublicManagement
        ) {
            return Err(denied());
        }
        self.prepare_scope(context, proxy, request)
    }

    pub(crate) fn prepare_command_line(
        &self,
        context: &CallContext,
    ) -> Result<Arc<CallbackScope>, PluginFault> {
        if context.stage != gateway_plugin_sdk::Stage::CommandLine {
            return Err(denied());
        }
        self.prepare_scope(context, None, None)
    }

    pub(crate) fn prepare_frontend_authentication(
        &self,
        context: &CallContext,
        request: &gateway_core::engine::authentication::ClientAuthenticationRequest,
    ) -> Result<Arc<CallbackScope>, PluginFault> {
        if context.stage != gateway_plugin_sdk::Stage::Authentication
            || context.account_id.is_some()
            || context.credential_revision.is_some()
            || context.attempt_id.is_some()
        {
            return Err(denied());
        }
        let mut scope = CallbackScope::for_call(context);
        scope.settings = request.settings().cloned();
        if let Some(origin) = request.invocation() {
            scope.extension_scope = origin.extensions.clone();
            scope.execution_effects = origin.effects.clone();
            scope.origin = Some(origin.clone());
        } else if let Some(request) = request.shared_request_context() {
            scope.extension_scope = scope.extension_scope.with_request(request);
        }
        self.register_scope(context, scope)
    }

    pub(crate) async fn save_command_account(
        &self,
        context: &CallContext,
        scope: &CallbackScope,
        request: gateway_plugin_sdk::call::host::AuthSaveRequest,
    ) -> Result<gateway_plugin_sdk::call::host::AuthSaveResult, PluginFault> {
        if context.stage != gateway_plugin_sdk::Stage::CommandLine || !scope.authorizes(context) {
            return Err(denied());
        }
        self.accounts.save(context, scope, request).await
    }

    fn prepare_scope(
        &self,
        context: &CallContext,
        proxy: Option<OutboundProxy>,
        request: Option<Arc<gateway_core::engine::extensions::RequestContext>>,
    ) -> Result<Arc<CallbackScope>, PluginFault> {
        let mut scope = CallbackScope::new(context, proxy);
        if let Some(request) = request {
            scope.extension_scope = scope.extension_scope.with_request(request);
        }
        self.register_scope(context, scope)
    }

    pub(crate) fn prepare_extensions(
        &self,
        context: &CallContext,
        extension_scope: gateway_core::engine::extensions::ExtensionCallScope,
    ) -> Result<Arc<CallbackScope>, PluginFault> {
        let mut scope = CallbackScope::for_call(context);
        scope.extension_scope = extension_scope;
        self.register_scope(context, scope)
    }

    pub(crate) fn bind_invocation(
        self: &Arc<Self>,
        call: &CallContext,
        origin: Option<InvocationContext>,
        extensions: gateway_core::engine::extensions::ExtensionCallScope,
        invocation: Arc<dyn MiddlewareCallback>,
    ) -> Result<MiddlewareBinding, gateway_core::engine::middleware::MiddlewareError> {
        let mut scope = CallbackScope::for_call(call);
        scope.extension_scope = extensions;
        scope.origin = origin;
        let scope = self
            .register_scope(call, scope)
            .map_err(|_| gateway_core::engine::middleware::MiddlewareError::Fault)?;
        self.bind_middleware(call.resource_scope_id.clone(), invocation, scope)
    }

    fn register_scope(
        &self,
        context: &CallContext,
        scope: CallbackScope,
    ) -> Result<Arc<CallbackScope>, PluginFault> {
        let scope = Arc::new(scope);
        let mut scopes = self
            .scopes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        scopes.retain(|_, scope| scope.strong_count() != 0);
        if scopes
            .get(&context.resource_scope_id)
            .and_then(Weak::upgrade)
            .is_some()
        {
            return Err(denied());
        }
        scopes.insert(context.resource_scope_id.clone(), Arc::downgrade(&scope));
        Ok(scope)
    }
}
