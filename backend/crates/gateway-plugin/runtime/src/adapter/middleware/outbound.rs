//! 在 Provider 发送前组合完整出站请求，并沿用模型数据面的调用资源

use std::{sync::Arc, time::Duration};

use gateway_admin::model::plugins::instances::PluginFailurePolicy;
use gateway_core::engine::middleware::{MiddlewareContext, MiddlewareError, outbound as core};
use gateway_plugin_sdk::{
    Stage,
    call::middleware::{HANDLE_METHOD, outbound as wire},
};

use super::MiddlewareEntry;
use crate::callback::outbound::{self, Invocation};

pub(super) async fn invoke(
    entry: Arc<MiddlewareEntry>,
    context: MiddlewareContext,
    request: core::Request,
    next: core::Next,
    timeout: Duration,
) -> Result<core::Request, MiddlewareError> {
    let Some(ports) = &entry.invocation else {
        return if entry.failure_policy == PluginFailurePolicy::Delegate {
            next.run(request).await
        } else {
            Err(MiddlewareError::Fault)
        };
    };
    let invocation = Invocation::new(next);
    let input = wire::CallHead {
        request: outbound::head(&request),
        metadata: wire::Metadata {
            request_id: context.request_id().as_str().to_owned(),
            client_key_id: context.client_key_id().as_str().to_owned(),
            account_group_ids: context
                .account_group_ids()
                .iter()
                .map(|id| id.as_str().to_owned())
                .collect(),
            provider: context
                .provider()
                .ok_or(MiddlewareError::InvalidState)?
                .as_str()
                .to_owned(),
            model: context.model().map(str::to_owned),
            account_id: context
                .account_id()
                .ok_or(MiddlewareError::InvalidState)?
                .as_str()
                .to_owned(),
            attempt_index: context
                .attempt_index()
                .ok_or(MiddlewareError::InvalidState)?
                .get(),
        },
    };
    let mut call = ports.session.context(Stage::Outbound, timeout);
    call.request = crate::adapter::request_context(context.extension_scope().request());
    call.request_id = Some(input.metadata.request_id.clone());
    call.attempt_id = Some(format!(
        "{}:{}",
        input.metadata.request_id, input.metadata.attempt_index
    ));
    call.account_id = Some(input.metadata.account_id.clone());
    let scope = ports
        .callbacks
        .prepare_data_plane(
            &call,
            context.execution_effects(),
            context.extension_scope().clone(),
            context.plan(),
            context.cancellation().clone(),
            context.request_body_limit(),
        )
        .map_err(|_| MiddlewareError::Fault)?;
    let _binding =
        ports
            .callbacks
            .bind_middleware(call.resource_scope_id.clone(), invocation, scope)?;
    let reply = tokio::select! {
        biased;
        () = context.cancellation().cancelled() => return Err(MiddlewareError::Fault),
        () = context.deadline().wait() => return Err(gateway_core::error::GatewayError::new(
            gateway_core::error::GatewayErrorKind::Timeout,
            "request deadline elapsed",
        ).into()),
        reply = ports.session.call(
            HANDLE_METHOD,
            call,
            serde_json::to_value(input).map_err(|_| MiddlewareError::InvalidState)?,
            request.body.into(),
        ) => reply.map_err(crate::callback::error::rpc_middleware)?,
    };
    let head = ports
        .session
        .decode_response(Stage::Outbound, reply.result)
        .map_err(|_| MiddlewareError::InvalidState)?;
    outbound::decode(head, reply.payload).map_err(|_| MiddlewareError::InvalidState)
}
