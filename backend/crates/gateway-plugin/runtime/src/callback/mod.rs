//! 插件宿主回调端口的组合、授权检查与方法分派

mod accounts;
mod admin;
mod affinity;
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
mod resources;
mod scope;
pub(crate) mod services;
pub(crate) mod upstream;
pub(crate) mod websocket_middleware;

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

use futures::future::BoxFuture;
use gateway_admin::model::AdminError;
use gateway_core::{account::OutboundProxy, lifecycle::CancellationToken};
use gateway_host::outbound::{HttpBody, HttpClient, NetworkPolicy};
use gateway_plugin_sdk::{CallContext, ErrorCode, PluginFault};
use tokio::time::Instant;

use crate::{CallbackHandler, RpcReply};
pub(crate) use accounts::PluginAccountPortSlot;
pub(crate) use affinity::PluginAffinityPortSlot;
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
    services: Arc<services::ServicePorts>,
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
}

struct CallResources {
    deadline: Instant,
    operation_timeout: Duration,
    cancellation: CancellationToken,
    scope: Arc<CallbackScope>,
    http_resources: Arc<http_middleware::resources::Resources>,
    model_bindings: tokio::sync::Mutex<
        BTreeMap<String, gateway_core::engine::execution::BoundModelExecutionContext>,
    >,
    state: Mutex<CallState>,
}

impl CallResources {
    fn request_settings(&self) -> Option<gateway_core::settings::RequestSettings> {
        let middleware = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .middleware
            .clone();
        middleware.and_then(|middleware| middleware.request_settings())
    }

    /// 建立受管流后，网络操作各自计时；连接空闲时间不消耗后续操作的预算
    fn timeout(&self) -> Result<Duration, PluginFault> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return Err(denied());
        }
        if state.resource_stream {
            return Ok(self.operation_timeout);
        }
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|timeout| !timeout.is_zero())
            .ok_or_else(|| PluginFault::new(ErrorCode::Timeout, "callback deadline elapsed"))
    }
}

#[derive(Default)]
struct CallState {
    closed: bool,
    resource_stream: bool,
    middleware: Option<Arc<dyn MiddlewareCallback>>,
    streams: BTreeMap<String, Arc<HttpStream>>,
    model_streams: BTreeMap<String, Arc<model::ModelStream>>,
}

struct HttpStream {
    body: tokio::sync::Mutex<Option<HttpBody>>,
    closed: tokio::sync::watch::Sender<bool>,
}

impl HttpStream {
    fn close(&self) {
        self.closed.send_replace(true);
        if let Ok(mut body) = self.body.try_lock() {
            body.take();
        }
    }
}

impl PluginCallbacks {
    pub(crate) fn new(
        instance: &gateway_admin::model::plugins::instances::PluginInstance,
        maximum_payload: usize,
        manifest: &gateway_plugin_sdk::Manifest,
        log_slots: Arc<tokio::sync::Semaphore>,
        private_state: Arc<private_state::PluginPrivateState>,
        ports: PluginCallbackPorts,
    ) -> Result<Self, AdminError> {
        Ok(Self {
            services: ports.services,
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
            models: Arc::new(model::PluginModels::new(ports.models, maximum_payload)),
            private_state,
            http: ports.http,
            network: Arc::new(ports.network),
            scopes: Mutex::new(BTreeMap::new()),
            pending_middleware: Mutex::new(BTreeMap::new()),
            calls: Mutex::new(BTreeMap::new()),
            maximum_payload,
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
    ) -> Result<Arc<CallbackScope>, PluginFault> {
        if !matches!(
            context.stage,
            gateway_plugin_sdk::Stage::Request
                | gateway_plugin_sdk::Stage::Attempt
                | gateway_plugin_sdk::Stage::Routing
                | gateway_plugin_sdk::Stage::Scheduling
        ) {
            return Err(denied());
        }
        let mut scope = CallbackScope::for_call(context);
        if let Some(effects) = effects {
            scope = scope.with_execution_effects(effects);
        }
        scope.extension_scope = extensions;
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

    pub(crate) fn prepare_upstream(
        &self,
        context: &CallContext,
        managed: Arc<upstream::ManagedUpstream>,
        extension_scope: gateway_core::engine::extensions::ExtensionCallScope,
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
        self.register_scope(context, scope)
    }

    pub(crate) fn prepare_management(
        &self,
        context: &CallContext,
        proxy: Option<OutboundProxy>,
    ) -> Result<Arc<CallbackScope>, PluginFault> {
        if !matches!(context.stage, gateway_plugin_sdk::Stage::Management) {
            return Err(denied());
        }
        self.prepare_scope(context, proxy)
    }

    pub(crate) fn prepare_command_line(
        &self,
        context: &CallContext,
    ) -> Result<Arc<CallbackScope>, PluginFault> {
        if context.stage != gateway_plugin_sdk::Stage::CommandLine {
            return Err(denied());
        }
        self.prepare_scope(context, None)
    }

    pub(crate) fn prepare_frontend_authentication(
        &self,
        context: &CallContext,
    ) -> Result<Arc<CallbackScope>, PluginFault> {
        if context.stage != gateway_plugin_sdk::Stage::Authentication
            || context.account_id.is_some()
            || context.credential_revision.is_some()
            || context.attempt_id.is_some()
        {
            return Err(denied());
        }
        self.prepare_scope(context, None)
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
    ) -> Result<Arc<CallbackScope>, PluginFault> {
        self.register_scope(context, CallbackScope::new(context, proxy))
    }

    pub(crate) fn prepare_observation(
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
    fn begin(&self, context: &CallContext) {
        let scope = self
            .scopes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&context.resource_scope_id)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| Arc::new(CallbackScope::for_call(context)));
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
                Arc::new(CallResources {
                    deadline: Instant::now() + Duration::from_millis(context.timeout_ms),
                    operation_timeout: Duration::from_millis(context.timeout_ms),
                    cancellation: CancellationToken::new(),
                    scope,
                    http_resources: middleware
                        .as_ref()
                        .and_then(|middleware| middleware.http_resources())
                        .unwrap_or_default(),
                    model_bindings: tokio::sync::Mutex::new(BTreeMap::new()),
                    state: Mutex::new(CallState {
                        middleware,
                        ..CallState::default()
                    }),
                }),
            );
    }

    fn streaming(&self, context: &CallContext) {
        if let Some(call) = self
            .calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&context.call_id)
        {
            call.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .resource_stream = context.resource_stream;
        }
    }

    fn finish(&self, context: &CallContext) {
        if let Some(call) = self
            .calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&context.call_id)
        {
            call.cancellation.cancel();
            let mut state = call
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.closed = true;
            // RPC End 可以先于消费者取完已入队的中间件正文到达
            // 这里只撤销新的
            // callback 入口；正文包装器持有 invocation，最后一个 owner 释放时再关闭
            // 下游，避免提前丢失尚未搬运的计量/终态信封
            state.middleware.take();
            for stream in state.streams.values() {
                stream.close();
            }
            state.streams.clear();
            for stream in state.model_streams.values() {
                stream.close();
            }
            state.model_streams.clear();
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
        let affinity = self.affinity.clone();
        let services = self.services.clone();
        Box::pin(async move {
            let call = call.ok_or_else(denied)?;
            let scope = &call.scope;
            let (middleware, is_private_state) = {
                let state = call
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if state.closed || (!state.resource_stream && Instant::now() >= call.deadline) {
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
                    .call(&context, scope, call.cancellation.clone(), params, &payload)
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
