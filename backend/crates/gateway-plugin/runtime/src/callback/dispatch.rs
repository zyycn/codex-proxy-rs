//! 将有效父调用的宿主回调分派到所属原生能力，并随调用撤销资源

use std::sync::{Arc, Weak};

use futures::future::BoxFuture;
use gateway_plugin_sdk::{
    CallContext, ErrorCode, PluginFault,
    call::{host, middleware as middleware_wire, upstream_adapter as upstream_wire},
    client::CallDeadline,
};

use super::{CallResources, CallbackScope, PluginCallbacks, denied, http, http_dispatch, upstream};
use crate::{CallbackHandler, RpcReply};

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
        let clients = self.clients.clone();
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
                if method == host::LOG {
                    return log.record(&context, params, &payload);
                }
                let middleware = if matches!(
                    method.as_str(),
                    middleware_wire::NEXT_METHOD
                        | middleware_wire::BODY_READ_METHOD
                        | middleware_wire::BODY_FACTS_METHOD
                        | middleware_wire::BODY_CLOSE_METHOD
                        | middleware_wire::http::UPGRADE_METHOD
                        | middleware_wire::http::RECEIVE_METHOD
                        | middleware_wire::websocket::SEND_METHOD
                ) {
                    Some(state.middleware.clone().ok_or_else(denied)?)
                } else {
                    None
                };
                let is_private_state = matches!(
                    method.as_str(),
                    host::STATE_GET | host::STATE_PUT | host::STATE_DELETE
                );
                (middleware, is_private_state)
            };
            if matches!(
                method.as_str(),
                middleware_wire::http::DISPATCH_METHOD
                    | middleware_wire::http::BODY_READ_METHOD
                    | middleware_wire::http::BODY_CLOSE_METHOD
                    | middleware_wire::http::BODY_CREATE_METHOD
                    | middleware_wire::http::BODY_WRITE_METHOD
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
                host::KEYS_LIST
                    | gateway_plugin_sdk::call::key_budgets::RESET
                    | gateway_plugin_sdk::call::key_budgets::GET
                    | gateway_plugin_sdk::call::key_budgets::UPDATE_LIMITS
            ) {
                return keys.call(&context, &method, params, &payload).await;
            }
            if matches!(
                method.as_str(),
                gateway_plugin_sdk::call::data::CLIENT_VERIFY
                    | gateway_plugin_sdk::call::data::ACCESS_CHECK
            ) {
                return clients
                    .call(&context, &call, &method, params, payload)
                    .await;
            }
            if method == host::MODELS_LIST {
                return models.call(&context, &call, &method, params, payload).await;
            }
            if matches!(
                method.as_str(),
                host::MODEL_EXECUTE
                    | host::MODEL_EXECUTE_STREAM
                    | host::MODEL_STREAM_READ
                    | host::MODEL_STREAM_CLOSE
            ) {
                return models.call(&context, &call, &method, params, payload).await;
            }
            if method == host::AFFINITY_LOOKUP {
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
                    | gateway_plugin_sdk::call::data::QUOTA_DETAILS
            ) {
                return data.call(&method, params, &payload).await;
            }
            if matches!(
                method.as_str(),
                host::AUTH_LIST | host::AUTH_GET | host::AUTH_GET_RUNTIME | host::AUTH_SAVE
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
            if method == host::ACCOUNT_HTTP {
                return http.account(&accounts, params, payload).await;
            }
            if matches!(
                method.as_str(),
                upstream_wire::HTTP_DO
                    | upstream_wire::HTTP_DO_STREAM
                    | upstream_wire::HTTP_STREAM_READ
                    | upstream_wire::HTTP_STREAM_CLOSE
                    | upstream_wire::WEBSOCKET_OPEN
                    | upstream_wire::WEBSOCKET_SEND
                    | upstream_wire::WEBSOCKET_READ
                    | upstream_wire::WEBSOCKET_CLOSE
            ) {
                return upstream::dispatch(&http, &method, params, payload).await;
            }
            if matches!(
                method.as_str(),
                host::HTTP_DO
                    | host::HTTP_DO_STREAM
                    | host::HTTP_STREAM_READ
                    | host::HTTP_STREAM_CLOSE
            ) {
                return http.dispatch(&method, params, payload).await;
            }
            Err(PluginFault::new(
                ErrorCode::Unsupported,
                "host callback method is not supported",
            ))
        })
    }
}
