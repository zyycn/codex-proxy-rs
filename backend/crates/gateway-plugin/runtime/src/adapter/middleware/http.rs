//! 将宿主 HTTP 调用接入插件中间件，并管理正文与回调作用域

use std::sync::Arc;

use futures::stream;
use gateway_admin::model::plugins::instances::PluginFailurePolicy;
use gateway_core::{engine::middleware::MiddlewareError, engine::middleware::http as core};
use gateway_plugin_sdk::{
    Stage,
    call::middleware::{HANDLE_METHOD, http as wire},
};
use http_body::Frame;
use http_body_util::{BodyExt as _, StreamBody};

use super::MiddlewareEntry;
use crate::callback::http_middleware::{Invocation, headers};

pub(super) async fn invoke(
    entry: Arc<MiddlewareEntry>,
    context: core::Context,
    request: core::Request,
    next: core::Next,
    timeout: std::time::Duration,
) -> Result<core::Response, MiddlewareError> {
    let Some(ports) = &entry.invocation else {
        return if entry.failure_policy == PluginFailurePolicy::Delegate {
            next.run(request).await
        } else {
            Err(MiddlewareError::Fault)
        };
    };
    let settings_sources = request
        .extensions()
        .get::<core::Settings>()
        .and_then(|settings| settings.runtime.as_ref())
        .map_or(serde_json::Value::Null, |settings| settings.inspect());
    let (invocation, request) = Invocation::new(request, next, entry.instance_id.clone())?;
    let mut call = ports.session.context(Stage::Http, timeout);
    call.request_id = Some(context.request_id.clone());
    call.resource_stream = true;
    let _binding = ports
        .callbacks
        .bind_invocation(
            &call,
            context
                .plan
                .clone()
                .map(|plan| crate::callback::InvocationContext {
                    request_id: context.request_id.clone(),
                    call_id: context.call_id.clone(),
                    cancellation: context.cancellation.clone(),
                    plan,
                }),
            context.extensions.clone(),
            invocation.clone(),
        )
        .map_err(|_| MiddlewareError::Fault)?;
    let mut stream = tokio::select! {
        biased;
        () = context.cancellation.cancelled() => return Err(MiddlewareError::Fault),
        result = ports.session.call_stream(HANDLE_METHOD, call, serde_json::to_value(wire::Call { settings_sources, request_id: context.request_id, call_id: context.call_id, parent_call_id: context.parent_call_id, request }).map_err(|_| MiddlewareError::InvalidState)?, Vec::new()) => result.map_err(crate::callback::error::rpc_middleware)?,
    };
    let response: wire::Response = ports
        .session
        .decode_response(Stage::Http, std::mem::take(&mut stream.initial.result))
        .map_err(|_| MiddlewareError::InvalidState)?;
    let payload = std::mem::take(&mut stream.initial.payload);
    let invalid = || {
        ports.session.invalid_response(Stage::Http);
        MiddlewareError::InvalidState
    };
    if response.session {
        let status = invocation.session_status().ok_or_else(invalid)?;
        if response.status != status.as_u16()
            || response.response.is_none()
            || !matches!(response.body, wire::Body::Empty)
            || !payload.is_empty()
        {
            return Err(invalid());
        }
        let mut response = invocation
            .complete(response, payload, None)
            .await
            .map_err(|_| invalid())?;
        let session = ports.session.clone();
        let task = Box::pin(async move {
            let _binding = _binding;
            tokio::select! {
                biased;
                () = context.cancellation.cancelled() => Err(core::upgrade::SessionError::Cancelled),
                result = async {
                    invocation.session_ready().await?;
                    match stream.next().await {
                        Ok(None) => Ok(()),
                        Ok(Some(_)) => {
                            let error = session.invalid_response(Stage::Http);
                            Err(session_error(error, &entry.instance_id))
                        }
                        Err(error) => Err(session_error(error, &entry.instance_id)),
                    }
                } => result,
            }
        });
        response
            .extensions_mut()
            .insert(core::upgrade::Upgraded::new(status, task));
        return Ok(response);
    }
    let body = if matches!(response.body, wire::Body::Stream) {
        let body = StreamBody::new(stream::try_unfold((stream, invocation.clone(), context.cancellation, ports.session.clone()), |(mut stream, invocation, cancellation, session)| async move {
            let chunk = tokio::select! {
                biased;
                () = cancellation.cancelled() => return Err(Box::new(MiddlewareError::Fault) as Box<dyn std::error::Error + Send + Sync>),
                chunk = stream.next() => chunk.map_err(|error| Box::new(crate::callback::error::rpc_middleware(error)) as Box<dyn std::error::Error + Send + Sync>)?,
            };
            let Some(chunk) = chunk else { return Ok(None); };
            let invalid = || {
                session.invalid_response(Stage::Http);
                MiddlewareError::InvalidState
            };
            let frame = match chunk.split_first() {
                Some((0, data)) => Frame::data(bytes::Bytes::copy_from_slice(data)),
                Some((1, data)) => Frame::trailers(headers(serde_json::from_slice(data).map_err(|_| invalid())?).map_err(|_| invalid())?),
                _ => return Err(Box::new(invalid()) as _),
            };
            Ok(Some((frame, (stream, invocation, cancellation, session))))
        })).boxed_unsync();
        Some(body)
    } else {
        // 返回句柄时不拉取正文，仍消费唯一 End，释放本次 RPC 槽位
        if stream
            .next()
            .await
            .map_err(crate::callback::error::rpc_middleware)?
            .is_some()
        {
            return Err(invalid());
        }
        None
    };
    invocation
        .complete(response, payload, body)
        .await
        .map_err(|_| invalid())
}

fn session_error(error: crate::RpcError, instance_id: &str) -> core::upgrade::SessionError {
    use core::upgrade::SessionError;
    use gateway_plugin_sdk::ErrorCode;

    let kind = match &error {
        crate::RpcError::Cancelled => return SessionError::Cancelled,
        // 本地期限已经按调用关联记录，终态消费不能再记同一次超时
        crate::RpcError::Timeout => return SessionError::Reported,
        crate::RpcError::Remote(fault) => match fault.code {
            ErrorCode::Cancelled => return SessionError::Cancelled,
            ErrorCode::Unsupported => "unsupported",
            ErrorCode::Rejected => "rejected",
            ErrorCode::InvalidInput => "invalid_input",
            ErrorCode::PermissionDenied => "permission_denied",
            ErrorCode::Upstream => "upstream",
            ErrorCode::Fault => "fault",
            ErrorCode::Timeout => "timeout",
            ErrorCode::Uncertain => "uncertain",
            ErrorCode::Capacity => "capacity",
            ErrorCode::Conflict => "conflict",
        },
        _ => error.diagnostic().0,
    };
    SessionError::Failed {
        kind,
        plugin_instance_id: Some(instance_id.to_owned()),
    }
}
