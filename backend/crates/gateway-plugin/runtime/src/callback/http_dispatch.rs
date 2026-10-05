//! 主动 HTTP 子调用只进入 API 分派端口，正文复用当前 RPC 的资源池
use super::{CallResources, http_middleware, invalid, services::ServicePorts};
use crate::RpcReply;
use gateway_core::middleware::http as core;
use gateway_plugin_sdk::{CallContext, PluginFault, call::middleware::http as wire};

pub(super) async fn call(
    services: &ServicePorts,
    context: &CallContext,
    call: &CallResources,
    method: &str,
    params: serde_json::Value,
    payload: Vec<u8>,
    maximum_payload: usize,
) -> Result<RpcReply, PluginFault> {
    if method != wire::DISPATCH_METHOD {
        return call
            .http_resources
            .call(method, params, payload, maximum_payload)
            .await;
    }
    let request: wire::Request = serde_json::from_value(params).map_err(|_| invalid())?;
    let method = request.method.parse().map_err(|_| invalid())?;
    let uri: http::Uri = request.uri.parse().map_err(|_| invalid())?;
    if uri.scheme().is_some() || uri.authority().is_some() || !uri.path().starts_with('/') {
        return Err(invalid());
    }
    let headers = http_middleware::headers(request.headers)?;
    let dispatcher = services.http()?;
    let settings = call
        .request_settings()
        .or_else(|| dispatcher.request_settings());
    let settings = http_middleware::resolve_settings(
        settings,
        request.settings,
        request.timeout_ms,
        &context.instance_id,
    )?;
    let parent = call.scope.origin.as_ref();
    let child = core::Context {
        plugin_instance_id: Some(context.instance_id.clone()),
        request_id: context
            .request_id
            .clone()
            .unwrap_or_else(|| context.resource_scope_id.clone()),
        call_id: uuid::Uuid::now_v7().to_string(),
        parent_call_id: Some(parent.map_or_else(
            || format!("{}:{}", context.resource_scope_id, context.call_id),
            |parent| parent.call_id.clone(),
        )),
        extensions: call.scope.child_extensions(&context.instance_id)?,
        // 句柄可以在父 RPC End 后交给 HTTP 发送端；生命周期属于父调用和响应正文，不能在 RPC End 提前取消
        cancellation: parent.map_or_else(
            || call.cancellation.child_token(),
            |parent| parent.cancellation.child_token(),
        ),
        plan: parent.map(|parent| parent.plan.clone()),
    };
    let body = call.http_resources.body(request.body, payload).await?;
    let mut http = core::Request::new(body);
    *http.method_mut() = method;
    *http.uri_mut() = uri;
    *http.headers_mut() = headers;
    *http.version_mut() = http_middleware::version(request.version);
    http.extensions_mut().insert(settings);
    call.scope.observe_http_dispatch();
    let response = dispatcher
        .dispatch(child, http)
        .await
        .map_err(|error| super::error::middleware(&error))?;
    call.http_resources.response(response)
}
