//! 服务组合与 HTTP 使用相同续体；不在这里解释操作参数或执行管理业务

use std::sync::Arc;

use super::MiddlewareEntry;
use crate::callback::services::Invocation;
use gateway_admin::model::plugins::instances::PluginFailurePolicy;
use gateway_core::middleware::service as core;
use gateway_plugin_sdk::{
    Stage,
    call::{middleware::HANDLE_METHOD, services as wire},
};

pub(super) async fn invoke(
    entry: Arc<MiddlewareEntry>,
    context: core::Context,
    input: core::Value,
    next: core::Next,
    timeout: std::time::Duration,
) -> Result<core::Value, core::Error> {
    let Some(ports) = &entry.invocation else {
        return if entry.failure_policy == PluginFailurePolicy::Delegate {
            next.run(input).await
        } else {
            Err(core::Error::unavailable("服务中间件不可用"))
        };
    };
    let invocation = Invocation::new(next);
    let mut call = ports.session.context(Stage::Service, timeout);
    call.request_id = Some(context.request_id.clone());
    let _binding = ports
        .callbacks
        .bind_invocation(
            &call,
            Some(crate::callback::InvocationContext {
                request_id: context.request_id.clone(),
                call_id: context.call_id.clone(),
                cancellation: context.cancellation.clone(),
                plan: context.plan.clone(),
            }),
            context.extensions.clone(),
            invocation,
        )
        .map_err(|_| core::Error::unavailable("服务调用上下文不可用"))?;
    let request = wire::Call {
        operation: context.operation.into(),
        input,
        request_id: context.request_id,
        call_id: context.call_id,
        parent_call_id: context.parent_call_id,
    };
    let reply = tokio::select! {
        biased;
        () = context.cancellation.cancelled() => return Err(core::Error::unavailable("服务调用已取消")),
        reply = ports.session.call(HANDLE_METHOD, call, serde_json::to_value(request).map_err(|_| core::Error::invalid("服务输入编码失败"))?, Vec::new()) => reply.map_err(|error| {
            let fault = crate::callback::error::rpc(&error);
            core::Error {kind:"unavailable".into(),message:fault.message.clone(),details:Some(serde_json::json!(fault))}
        })?,
    };
    if !reply.payload.is_empty() {
        return Err(core::Error::invalid("服务返回了意外的二进制正文"));
    }
    let response: wire::Response = serde_json::from_value(reply.result)
        .map_err(|_| core::Error::invalid("服务结果类型无效"))?;
    response.map_err(|error| core::Error {
        kind: error.kind,
        message: error.message,
        details: error.details,
    })
}
