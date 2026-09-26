mod accounts;
mod affinity;
mod data;
mod http;
mod keys;
mod log;
mod middleware;
mod model;
pub(crate) mod private_state;
mod resources;
mod scope;

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
    MiddlewareBinding, MiddlewareBodyAuthority, MiddlewareCompletionBody, MiddlewareInvocation,
};
pub(crate) use model::PluginModelPortSlot;
pub(crate) use resources::PluginResourcePorts;
pub(crate) use scope::NetworkScope;

pub(crate) struct PluginCallbackPorts {
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
        http: Arc<HttpClient>,
        network: NetworkPolicy,
        accounts: Arc<PluginAccountPortSlot>,
        keys: Arc<PluginClientKeyPortSlot>,
        models: Arc<PluginModelPortSlot>,
        affinity: Arc<PluginAffinityPortSlot>,
        resources: Arc<PluginResourcePorts>,
    ) -> Self {
        Self {
            http,
            network,
            accounts,
            keys,
            models,
            affinity,
            resources,
        }
    }
}

pub(crate) struct PluginCallbacks {
    accounts: Arc<accounts::PluginAccounts>,
    resources: Arc<resources::PluginResources>,
    data: Arc<data::PluginData>,
    keys: Arc<PluginClientKeyPortSlot>,
    models_authorized: bool,
    affinity: Arc<affinity::PluginAffinity>,
    log: Arc<log::PluginLog>,
    models: Arc<model::PluginModels>,
    private_state: Arc<private_state::PluginPrivateState>,
    http: Arc<HttpClient>,
    http_authorization: Arc<HttpAuthorization>,
    scopes: Mutex<BTreeMap<String, Weak<NetworkScope>>>,
    pending_middleware: Mutex<BTreeMap<String, Arc<MiddlewareInvocation>>>,
    calls: Mutex<BTreeMap<u64, Arc<CallResources>>>,
    maximum_payload: usize,
}

struct HttpAuthorization {
    network: NetworkPolicy,
    authorized: bool,
}

struct CallResources {
    deadline: Instant,
    cancellation: CancellationToken,
    scope: Arc<NetworkScope>,
    model_bindings: tokio::sync::Mutex<
        BTreeMap<String, gateway_core::engine::execution::BoundModelExecutionContext>,
    >,
    state: Mutex<CallState>,
}

#[derive(Default)]
struct CallState {
    closed: bool,
    middleware: Option<Arc<MiddlewareInvocation>>,
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
        let grants = &instance.grants;
        Ok(Self {
            resources: Arc::new(resources::PluginResources::new(instance, ports.resources)),
            data: Arc::new(data::PluginData::new(ports.accounts.clone(), grants)),
            accounts: Arc::new(accounts::PluginAccounts::new(ports.accounts, grants)),
            keys: ports.keys,
            models_authorized: grants.iter().any(|grant| grant.permission == "models"),
            affinity: Arc::new(affinity::PluginAffinity::new(ports.affinity, grants)),
            log: Arc::new(
                log::PluginLog::new(manifest, log_slots)
                    .map_err(|_| AdminError::invalid("插件清单身份不合法"))?,
            ),
            models: Arc::new(model::PluginModels::new(
                ports.models,
                grants,
                maximum_payload,
            )),
            private_state,
            http: ports.http,
            http_authorization: Arc::new(HttpAuthorization {
                network: ports.network,
                authorized: grants.iter().any(|grant| grant.permission == "network"),
            }),
            scopes: Mutex::new(BTreeMap::new()),
            pending_middleware: Mutex::new(BTreeMap::new()),
            calls: Mutex::new(BTreeMap::new()),
            maximum_payload,
        })
    }

    pub(crate) fn bind_middleware(
        self: &Arc<Self>,
        resource_scope_id: String,
        invocation: Arc<MiddlewareInvocation>,
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
        ))
    }

    fn unbind_middleware(&self, resource_scope_id: &str, invocation: &Arc<MiddlewareInvocation>) {
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
        effects: Arc<gateway_core::engine::nested::ExecutionEffects>,
    ) -> Result<Arc<NetworkScope>, PluginFault> {
        if !matches!(
            context.stage,
            gateway_plugin_sdk::Stage::Request
                | gateway_plugin_sdk::Stage::Attempt
                | gateway_plugin_sdk::Stage::Routing
                | gateway_plugin_sdk::Stage::Scheduling
        ) {
            return Err(denied());
        }
        self.register_scope(
            context,
            NetworkScope::for_call(context).with_execution_effects(effects),
        )
    }

    pub(crate) fn prepare_management(
        &self,
        context: &CallContext,
        proxy: Option<OutboundProxy>,
    ) -> Result<Arc<NetworkScope>, PluginFault> {
        if !matches!(context.stage, gateway_plugin_sdk::Stage::Management) {
            return Err(denied());
        }
        self.prepare_scope(context, proxy)
    }

    pub(crate) fn prepare_command_line(
        &self,
        context: &CallContext,
    ) -> Result<Arc<NetworkScope>, PluginFault> {
        if context.stage != gateway_plugin_sdk::Stage::CommandLine {
            return Err(denied());
        }
        self.prepare_scope(context, None)
    }

    pub(crate) fn prepare_frontend_authentication(
        &self,
        context: &CallContext,
    ) -> Result<Arc<NetworkScope>, PluginFault> {
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
        scope: &NetworkScope,
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
    ) -> Result<Arc<NetworkScope>, PluginFault> {
        self.register_scope(context, NetworkScope::new(context, proxy))
    }

    pub(crate) fn prepare_observation(
        &self,
        context: &CallContext,
        extension_scope: gateway_core::engine::extensions::ExtensionCallScope,
    ) -> Result<Arc<NetworkScope>, PluginFault> {
        let mut scope = NetworkScope::for_call(context);
        scope.extension_scope = extension_scope;
        self.register_scope(context, scope)
    }

    fn register_scope(
        &self,
        context: &CallContext,
        scope: NetworkScope,
    ) -> Result<Arc<NetworkScope>, PluginFault> {
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
            .unwrap_or_else(|| Arc::new(NetworkScope::for_call(context)));
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
                    cancellation: CancellationToken::new(),
                    scope,
                    model_bindings: tokio::sync::Mutex::new(BTreeMap::new()),
                    state: Mutex::new(CallState {
                        middleware,
                        ..CallState::default()
                    }),
                }),
            );
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
            // RPC End 可以先于消费者取完已入队的中间件正文到达。这里只撤销新的
            // callback 入口；正文包装器持有 invocation，最后一个 owner 释放时再关闭
            // 下游，避免提前丢失尚未搬运的计量/终态信封。
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
        let authorization = self.http_authorization.clone();
        let maximum_payload = self.maximum_payload;
        let log = self.log.clone();
        let private_state = self.private_state.clone();
        let accounts = self.accounts.clone();
        let resources = self.resources.clone();
        let data = self.data.clone();
        let keys = self.keys.clone();
        let models_authorized = self.models_authorized;
        let models = self.models.clone();
        let affinity = self.affinity.clone();
        Box::pin(async move {
            let call = call.ok_or_else(denied)?;
            let scope = &call.scope;
            let (middleware, is_private_state) = {
                let state = call
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if state.closed || Instant::now() >= call.deadline {
                    return Err(denied());
                }
                if method == "host.log" {
                    return log.record(&context, params, &payload);
                }
                let middleware = if matches!(
                    method.as_str(),
                    gateway_plugin_sdk::call::middleware::NEXT_METHOD
                        | gateway_plugin_sdk::call::middleware::BODY_READ_METHOD
                        | gateway_plugin_sdk::call::middleware::BODY_CLOSE_METHOD
                ) {
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
            if let Some(middleware) = middleware {
                return middleware
                    .call(&method, params, payload, maximum_payload)
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
            if method == "host.keys.list" {
                if !models_authorized {
                    return Err(denied());
                }
                return keys.list(params, &payload).await;
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
                    | gateway_plugin_sdk::call::data::QUOTA_GET
            ) {
                return data.call(&context, &method, params, &payload).await;
            }
            if matches!(
                method.as_str(),
                "host.auth.list" | "host.auth.get" | "host.auth.get_runtime" | "host.auth.save"
            ) {
                return accounts
                    .call(&context, scope, &method, params, &payload)
                    .await;
            }
            if !authorization.authorized {
                return Err(denied());
            }
            http::dispatch(
                &http,
                &authorization,
                scope,
                &call,
                &method,
                params,
                payload,
                maximum_payload,
            )
            .await
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
