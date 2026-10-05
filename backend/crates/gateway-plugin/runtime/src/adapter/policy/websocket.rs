//! 将宿主 WebSocket 调用接入插件中间件，并管理消息与回调作用域

use std::sync::Arc;

use super::MiddlewareEntry;
use crate::callback::websocket_middleware::Invocation;
use gateway_admin::model::plugins::instances::PluginFailurePolicy;
use gateway_core::{engine::middleware::MiddlewareError, middleware::websocket as core};
use gateway_plugin_sdk::{
    Stage,
    call::middleware::{HANDLE_METHOD, MiddlewareHeader, websocket as wire},
};

pub(super) async fn invoke(
    entry: Arc<MiddlewareEntry>,
    context: core::Context,
    message: core::Message,
    next: core::Next,
    timeout: std::time::Duration,
) -> Result<Option<core::Message>, MiddlewareError> {
    let Some(ports) = &entry.invocation else {
        return if entry.failure_policy == PluginFailurePolicy::Delegate {
            next.run(message).await
        } else {
            Err(MiddlewareError::Fault)
        };
    };
    let invocation = Invocation::new(next, context.sender.clone());
    let input = wire::Call {
        connection_id: context.connection_id.clone(),
        direction: match context.direction {
            core::Direction::Incoming => wire::Direction::Incoming,
            core::Direction::Outgoing => wire::Direction::Outgoing,
        },
        headers: context
            .headers
            .iter()
            .map(|header| MiddlewareHeader {
                name: header.name().to_owned(),
                value: header.value().to_vec(),
            })
            .collect(),
        message: invocation
            .encode(message)
            .map_err(|_| MiddlewareError::Fault)?,
    };
    let mut call = ports.session.context(Stage::WebSocket, timeout);
    call.request_id = Some(context.connection_id.clone());
    let _binding = ports
        .callbacks
        .bind_invocation(
            &call,
            context.plan.map(|plan| crate::callback::InvocationContext {
                request_id: context.connection_id.clone(),
                call_id: call.resource_scope_id.clone(),
                cancellation: context.cancellation.clone(),
                plan,
            }),
            Default::default(),
            invocation.clone(),
        )
        .map_err(|_| MiddlewareError::Fault)?;
    let reply = tokio::select! {
        biased;
        () = context.cancellation.cancelled() => return Err(MiddlewareError::Fault),
        reply = ports.session.call(HANDLE_METHOD, call, serde_json::to_value(input).map_err(|_| MiddlewareError::InvalidState)?, Vec::new()) => reply.map_err(crate::callback::error::rpc_middleware)?,
    };
    let output: Option<wire::Message> = ports
        .session
        .decode_response(Stage::WebSocket, reply.result)
        .map_err(|_| MiddlewareError::InvalidState)?;
    let invalid = || {
        ports.session.invalid_response(Stage::WebSocket);
        MiddlewareError::InvalidState
    };
    match output {
        Some(message) => invocation
            .decode(message, reply.payload)
            .map(Some)
            .map_err(|_| invalid()),
        None if reply.payload.is_empty() => Ok(None),
        None => Err(invalid()),
    }
}
