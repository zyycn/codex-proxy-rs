//! 插件宿主回调端口的组合、授权检查与方法分派

mod accounts;
mod admin;
mod affinity;
mod call_resources;
mod data;
pub(crate) mod error;
mod facts;
mod http;
mod http_dispatch;
pub(crate) mod http_middleware;
mod keys;
mod log;
mod middleware;
mod model;
pub(crate) mod private_state;
pub(crate) mod request_limits;
mod resources;
mod scope;
pub(crate) mod services;
pub(crate) mod upstream;
pub(crate) mod websocket_middleware;

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
};

use futures::future::BoxFuture;
use gateway_admin::model::AdminError;
use gateway_core::{account::OutboundProxy, lifecycle::CancellationToken};
use gateway_host::outbound::{HttpClient, NetworkPolicy};
use gateway_plugin_sdk::{CallContext, ErrorCode, PluginFault, client::CallDeadline};

use crate::{CallbackHandler, RpcReply};
pub(crate) use accounts::PluginAccountPortSlot;
pub(crate) use affinity::PluginAffinityPortSlot;
use call_resources::{CallResources, HttpStream};
pub(crate) use keys::PluginClientKeyPortSlot;
pub(crate) use middleware::{
    MiddlewareBinding, MiddlewareBodyAuthority, MiddlewareCallback, MiddlewareCompletionBody,
    MiddlewareInvocation,
};
pub(crate) use model::PluginModelPortSlot;
pub(crate) use resources::PluginResourcePorts;
pub(crate) use scope::{CallbackScope, InvocationContext};

pub(crate) struct PluginCallbackPorts {
    services: Arc<services::ServicePorts>,
    http: Arc<HttpClient>,
    network: NetworkPolicy,
    accounts: Arc<PluginAccountPortSlot>,
    keys: Arc<PluginClientKeyPortSlot>,
    models: Arc<PluginModelPortSlot>,
    affinity: Arc<PluginAffinityPortSlot>,
    resources: Arc<PluginResourcePorts>,
}

impl PluginCallbackPorts {
    pub(crate) fn new(
        services: Arc<services::ServicePorts>,
        http: Arc<HttpClient>,
        accounts: Arc<PluginAccountPortSlot>,
        keys: Arc<PluginClientKeyPortSlot>,
        models: Arc<PluginModelPortSlot>,
        affinity: Arc<PluginAffinityPortSlot>,
        resources: Arc<PluginResourcePorts>,
    ) -> Self {
        Self {
            services,
            http,
            network: NetworkPolicy::unrestricted(),
            accounts,
            keys,
            models,
            affinity,
            resources,
        }
    }
}

pub(crate) struct PluginCallbacks {
    pub(crate) registration_body_limit: Option<usize>,
    services: Arc<services::ServicePorts>,
    service_actor: gateway_admin::model::usage_data::UsageActor,
    accounts: Arc<accounts::PluginAccounts>,
    resources: Arc<resources::PluginResources>,
    data: Arc<data::PluginData>,
    keys: Arc<keys::PluginClientKeys>,
    affinity: Arc<affinity::PluginAffinity>,
    log: Arc<log::PluginLog>,
    models: Arc<model::PluginModels>,
    private_state: Arc<private_state::PluginPrivateState>,
    http: Arc<HttpClient>,
    network: Arc<NetworkPolicy>,
    scopes: Mutex<BTreeMap<String, Weak<CallbackScope>>>,
    pending_middleware: Mutex<BTreeMap<String, Arc<dyn MiddlewareCallback>>>,
    calls: Mutex<BTreeMap<u64, Arc<CallResources>>>,
    maximum_payload: usize,
    request_limits: Arc<request_limits::RequestLimits>,
}

impl PluginCallbacks {
    pub(crate) fn new(
        instance: &gateway_admin::model::plugins::instances::PluginInstance,
        maximum_payload: usize,
        manifest: &gateway_plugin_sdk::Manifest,
        log_slots: Arc<tokio::sync::Semaphore>,
        private_state: Arc<private_state::PluginPrivateState>,
        ports: PluginCallbackPorts,
        request_limits: Arc<request_limits::RequestLimits>,
    ) -> Result<Self, AdminError> {
        Ok(Self {
            registration_body_limit: None,
            services: ports.services,
            service_actor: gateway_admin::model::usage_data::UsageActor::Plugin {
                publisher: manifest.publisher.clone(),
                name: manifest.name.clone(),
                instance_id: instance.id.clone(),
            },
            resources: Arc::new(resources::PluginResources::new(instance, ports.resources)),
            data: Arc::new(data::PluginData::new(
                ports.accounts.clone(),
                ports.keys.clone(),
            )),
            accounts: Arc::new(accounts::PluginAccounts::new(ports.accounts)),
            keys: Arc::new(keys::PluginClientKeys::new(instance, ports.keys)),
            affinity: Arc::new(affinity::PluginAffinity::new(ports.affinity)),
            log: Arc::new(
                log::PluginLog::new(manifest, log_slots)
                    .map_err(|_| AdminError::invalid("插件清单身份不合法"))?,
            ),
            models: Arc::new(model::PluginModels::new(
                ports.models,
                request_limits.clone(),
                maximum_payload,
            )),
            private_state,
            http: ports.http,
            network: Arc::new(ports.network),
            scopes: Mutex::new(BTreeMap::new()),
            pending_middleware: Mutex::new(BTreeMap::new()),
            calls: Mutex::new(BTreeMap::new()),
            maximum_payload,
            request_limits,
        })
    }

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

    fn unbind_middleware(&self, resource_scope_id: &str, invocation: &Arc<dyn MiddlewareCallback>) {
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
            plan,
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
        request: Option<Arc<gateway_core::engine::extensions::RequestContext>>,
    ) -> Result<Arc<CallbackScope>, PluginFault> {
        if context.stage != gateway_plugin_sdk::Stage::Authentication
            || context.account_id.is_some()
            || context.credential_revision.is_some()
            || context.attempt_id.is_some()
        {
            return Err(denied());
        }
        self.prepare_scope(context, None, request)
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

impl CallbackHandler for PluginCallbacks {
    fn begin(&self, context: &CallContext, deadline: &CallDeadline) {
        let scope = self
            .scopes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&context.resource_scope_id)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| {
                let mut scope = CallbackScope::for_call(context);
                if context.stage == gateway_plugin_sdk::Stage::Registration {
                    scope.request_body_limit = self.registration_body_limit;
                }
                Arc::new(scope)
            });
        let middleware = self
            .pending_middleware
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&context.resource_scope_id);
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                context.call_id,
                Arc::new(CallResources::new(context, deadline, scope, middleware)),
            );
    }

    fn finish(&self, context: &CallContext) {
        if let Some(call) = self
            .calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&context.call_id)
        {
            call.close();
        }
    }

    fn call(
        &self,
        context: CallContext,
        method: String,
        params: serde_json::Value,
        payload: Vec<u8>,
    ) -> BoxFuture<'static, Result<RpcReply, PluginFault>> {
        let call = self
            .calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&context.call_id)
            .cloned();
        let http = self.http.clone();
        let network = self.network.clone();
        let maximum_payload = self.maximum_payload;
        let log = self.log.clone();
        let private_state = self.private_state.clone();
        let accounts = self.accounts.clone();
        let resources = self.resources.clone();
        let data = self.data.clone();
        let keys = self.keys.clone();
        let models = self.models.clone();
        let request_limits = self.request_limits.clone();
        let affinity = self.affinity.clone();
        let services = self.services.clone();
        let service_actor = self.service_actor.clone();
        Box::pin(async move {
            let call = call.ok_or_else(denied)?;
            let scope = &call.scope;
            let (middleware, is_private_state) = {
                let state = call
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if state.closed || call.deadline.is_elapsed() {
                    return Err(denied());
                }
                if method == "host.log" {
                    return log.record(&context, params, &payload);
                }
                let middleware = if method.starts_with("host.middleware.") {
                    Some(state.middleware.clone().ok_or_else(denied)?)
                } else {
                    None
                };
                let is_private_state = matches!(
                    method.as_str(),
                    "host.state.get" | "host.state.put" | "host.state.delete"
                );
                (middleware, is_private_state)
            };
            if matches!(
                method.as_str(),
                gateway_plugin_sdk::call::middleware::http::DISPATCH_METHOD
                    | gateway_plugin_sdk::call::middleware::http::BODY_READ_METHOD
                    | gateway_plugin_sdk::call::middleware::http::BODY_CLOSE_METHOD
                    | gateway_plugin_sdk::call::middleware::http::BODY_CREATE_METHOD
                    | gateway_plugin_sdk::call::middleware::http::BODY_WRITE_METHOD
            ) {
                return tokio::select! {
                    biased;
                    () = call.cancellation.cancelled() => Err(PluginFault::new(ErrorCode::Cancelled, "HTTP callback cancelled")),
                    result = http_dispatch::call(&services, &context, &call, &method, params, payload, maximum_payload) => result,
                };
            }
            if let Some(middleware) = middleware {
                return middleware
                    .invoke(method, params, payload, maximum_payload)
                    .await;
            }
            if method == gateway_plugin_sdk::call::services::CALL_METHOD {
                return services
                    .call(&context, &call, service_actor, params, &payload)
                    .await;
            }
            if is_private_state {
                return private_state.call(&method, params, &payload).await;
            }
            if matches!(
                method.as_str(),
                gateway_plugin_sdk::call::resources::GROUP_ENSURE
                    | gateway_plugin_sdk::call::resources::GROUP_MEMBERS
                    | gateway_plugin_sdk::call::resources::KEY_ENSURE
            ) {
                return resources.call(&context, &method, params, &payload).await;
            }
            if matches!(
                method.as_str(),
                "host.keys.list"
                    | gateway_plugin_sdk::call::key_budgets::RESET
                    | gateway_plugin_sdk::call::key_budgets::GET
                    | gateway_plugin_sdk::call::key_budgets::UPDATE_LIMITS
            ) {
                return keys.call(&context, &method, params, &payload).await;
            }
            if method == "host.models.list" {
                return models.call(&context, &call, &method, params, payload).await;
            }
            if matches!(
                method.as_str(),
                "host.model.execute"
                    | "host.model.execute_stream"
                    | "host.model.stream_read"
                    | "host.model.stream_close"
            ) {
                return models.call(&context, &call, &method, params, payload).await;
            }
            if method == "host.affinity.lookup" {
                return affinity.call(&context, params, &payload).await;
            }
            let scope = Some(scope)
                .filter(|scope| scope.authorizes(&context))
                .ok_or_else(denied)?;
            if matches!(
                method.as_str(),
                gateway_plugin_sdk::call::data::ACCOUNTS_LIST
                    | gateway_plugin_sdk::call::data::KEYS_GET
                    | gateway_plugin_sdk::call::data::QUOTA_GET
                    | gateway_plugin_sdk::call::data::QUOTA_REFRESH
            ) {
                return data.call(&method, params, &payload).await;
            }
            if matches!(
                method.as_str(),
                "host.auth.list" | "host.auth.get" | "host.auth.get_runtime" | "host.auth.save"
            ) {
                return accounts
                    .call(&context, scope, &method, params, &payload)
                    .await;
            }
            let http = http::HttpCallbacks {
                client: &http,
                network: &network,
                scope,
                call: &call,
                maximum_payload,
                request_limits: &request_limits,
            };
            if method.starts_with("host.upstream.") {
                return upstream::dispatch(&http, &method, params, payload).await;
            }
            http.dispatch(&method, params, payload).await
        })
    }
}

fn denied() -> PluginFault {
    PluginFault::new(
        ErrorCode::PermissionDenied,
        "callback resource is not authorized",
    )
}
fn invalid() -> PluginFault {
    PluginFault::new(ErrorCode::InvalidInput, "callback input is invalid")
}
