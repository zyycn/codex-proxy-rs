//! 插件中间件链的执行、失败回退与响应正文生命周期管理

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use bytes::Bytes;
use futures::future::BoxFuture;
use gateway_admin::model::plugins::instances::PluginFailurePolicy;
use gateway_core::engine::middleware::{
    MiddlewareBody, MiddlewareContext, MiddlewareError, MiddlewareFrame, MiddlewareFraming,
    MiddlewareMount, MiddlewareNext, MiddlewarePlan, MiddlewareRequest, MiddlewareResponse,
};
use gateway_core::middleware::{Middleware, Next};
use gateway_plugin_sdk::{
    ErrorCode, SendState, Stage,
    call::middleware::{
        HANDLE_METHOD, MiddlewareBodyDisposition, MiddlewareBodyFrame,
        MiddlewareMount as WireMiddlewareMount, MiddlewareRequestHead, MiddlewareResponseHead,
        MiddlewareTransport as WireMiddlewareTransport,
    },
};
use gateway_protocol::openai::sse::parse_sse_events;

use crate::{
    RpcError, RpcStream,
    callback::{MiddlewareBodyAuthority, MiddlewareCompletionBody, MiddlewareInvocation},
};

use super::{MiddlewareEntry, PluginRequestPolicyPlan};

const MAXIMUM_MAPPED_FRAMES_PER_SOURCE: usize = 64;
const MAXIMUM_MAPPED_BYTES_PER_SOURCE: usize = 8 * 1024 * 1024;

// 协议适配只提供选择条件与单层调用；顺序、上下文捕获和续体拼接共用这一个入口
type Invoke<Context, Input, Output, Error> = fn(
    Arc<MiddlewareEntry>,
    Context,
    Input,
    Next<Input, Output, Error>,
    Duration,
) -> BoxFuture<'static, Result<Output, Error>>;

impl PluginRequestPolicyPlan {
    fn chain<Context, Input, Output, Error>(
        &self,
        mount: MiddlewareMount,
        context: Context,
        select: impl Fn(&MiddlewareEntry, &Context) -> bool,
        invoke: Invoke<Context, Input, Output, Error>,
        next: Next<Input, Output, Error>,
    ) -> Next<Input, Output, Error>
    where
        Context: Clone + Send + 'static,
        Input: Send + 'static,
        Output: Send + 'static,
        Error: Send + 'static,
    {
        let Some(entries) = self.middleware.get(&mount) else {
            return next;
        };
        let layers = entries
            .iter()
            .filter(|entry| select(entry, &context))
            .map(|entry| -> Middleware<Input, Output, Error> {
                let entry = Arc::clone(entry);
                let context = context.clone();
                let timeout = self.middleware_timeout;
                Box::new(move |input, next| invoke(entry, context, input, next, timeout))
            })
            .collect();
        gateway_core::middleware::compose(layers, move |input| next.run(input))
    }
}

impl MiddlewarePlan for PluginRequestPolicyPlan {
    fn has_service(&self) -> bool {
        self.middleware.contains_key(&MiddlewareMount::Service)
    }
    fn handle_service(
        &self,
        context: gateway_core::middleware::service::Context,
        input: gateway_core::middleware::service::Value,
        next: gateway_core::middleware::service::Next,
    ) -> BoxFuture<
        'static,
        Result<gateway_core::middleware::service::Value, gateway_core::middleware::service::Error>,
    > {
        self.chain(
            MiddlewareMount::Service,
            context,
            |entry, context| !context.extensions.contains(&entry.instance_id),
            |entry, context, input, next, timeout| {
                Box::pin(super::service::invoke(entry, context, input, next, timeout))
            },
            next,
        )
        .run(input)
    }

    fn has_websocket(&self) -> bool {
        self.middleware.contains_key(&MiddlewareMount::WebSocket)
    }
    fn handle_websocket(
        &self,
        context: gateway_core::middleware::websocket::Context,
        message: gateway_core::middleware::websocket::Message,
        next: gateway_core::middleware::websocket::Next,
    ) -> BoxFuture<
        'static,
        Result<Option<gateway_core::middleware::websocket::Message>, MiddlewareError>,
    > {
        self.chain(
            MiddlewareMount::WebSocket,
            context,
            |_entry, _context| true,
            |entry, context, message, next, timeout| {
                Box::pin(super::websocket::invoke(
                    entry, context, message, next, timeout,
                ))
            },
            next,
        )
        .run(message)
    }
    fn has_http(&self) -> bool {
        self.middleware.contains_key(&MiddlewareMount::Http)
    }

    fn handle_http(
        &self,
        context: gateway_core::middleware::http::Context,
        request: gateway_core::middleware::http::Request,
        next: gateway_core::middleware::http::Next,
    ) -> BoxFuture<'static, Result<gateway_core::middleware::http::Response, MiddlewareError>> {
        self.chain(
            MiddlewareMount::Http,
            context,
            |entry, context| !context.extensions.contains(&entry.instance_id),
            |entry, context, request, next, timeout| {
                Box::pin(super::http::invoke(entry, context, request, next, timeout))
            },
            next,
        )
        .run(request)
    }

    fn handle(
        &self,
        context: MiddlewareContext,
        request: MiddlewareRequest,
        next: MiddlewareNext,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        self.chain(
            context.mount(),
            context,
            |entry, context| {
                !context.extension_scope().contains(&entry.instance_id)
                    && entry.scope.matches_processing(
                        context.client_key_id(),
                        context.account_group_ids(),
                        context.provider(),
                        context.model(),
                    )
            },
            |entry, context, request, next, timeout| {
                Box::pin(invoke_middleware(entry, context, request, next, timeout))
            },
            next,
        )
        .run(request)
    }
}

async fn invoke_middleware(
    entry: Arc<MiddlewareEntry>,
    context: MiddlewareContext,
    request: MiddlewareRequest,
    next: MiddlewareNext,
    maximum_timeout: Duration,
) -> Result<MiddlewareResponse, MiddlewareError> {
    let Some(invocation_ports) = &entry.invocation else {
        return if entry.failure_policy
            == gateway_admin::model::plugins::instances::PluginFailurePolicy::Delegate
        {
            next.run(request).await
        } else {
            Err(MiddlewareError::Fault)
        };
    };
    let remaining = context.deadline().bounded(maximum_timeout);
    if remaining.is_zero() || context.cancellation().is_cancelled() {
        return Err(MiddlewareError::Fault);
    }
    let invocation = MiddlewareInvocation::new(
        &context,
        request,
        next,
        entry.instance_id.clone(),
        crate::compatibility::FastSettings::middleware(&invocation_ports.session),
    );
    let (protocol, headers, payload) = match invocation.request_parts() {
        Ok(projection) => projection,
        Err(_) => return recover_rpc(&entry, &invocation, RpcError::Context).await,
    };
    let mut call_context = invocation_ports.session.context(
        match entry.mount {
            MiddlewareMount::Http | MiddlewareMount::WebSocket | MiddlewareMount::Service => {
                return Err(MiddlewareError::InvalidState);
            }
            MiddlewareMount::Request => Stage::Request,
            MiddlewareMount::Attempt => Stage::Attempt,
        },
        remaining,
    );
    call_context.request_id = Some(context.request_id().as_str().to_owned());
    call_context.attempt_id = context
        .attempt_index()
        .map(|index| format!("{}:{index}", context.request_id().as_str()));
    call_context.account_id = context
        .account_id()
        .map(|account| account.as_str().to_owned());
    let scope = invocation_ports
        .callbacks
        .prepare_data_plane(
            &call_context,
            context.execution_effects(),
            context.extension_scope().clone(),
            context.plan(),
            context.cancellation().clone(),
        )
        .map_err(|_| MiddlewareError::Fault)?;
    let _binding = invocation_ports.callbacks.bind_middleware(
        call_context.resource_scope_id.clone(),
        invocation.clone(),
        scope,
    )?;
    let head = MiddlewareRequestHead {
        settings_sources: invocation
            .settings_sources()
            .map_err(|_| MiddlewareError::InvalidState)?,
        settings: invocation
            .settings()
            .map_err(|_| MiddlewareError::InvalidState)?,
        client_key_id: context.client_key_id().as_str().to_owned(),
        account_group_ids: context
            .account_group_ids()
            .iter()
            .map(|id| id.as_str().to_owned())
            .collect(),
        request_id: context.request_id().as_str().to_owned(),
        mount: match context.mount() {
            MiddlewareMount::Http | MiddlewareMount::WebSocket | MiddlewareMount::Service => {
                return Err(MiddlewareError::InvalidState);
            }
            MiddlewareMount::Request => WireMiddlewareMount::Request,
            MiddlewareMount::Attempt => WireMiddlewareMount::Attempt,
        },
        attempt_index: context.attempt_index().map(std::num::NonZeroU32::get),
        operation: context.operation().map_or_else(
            || "query".to_owned(),
            |operation| operation.as_str().to_owned(),
        ),
        protocol,
        endpoint: context.endpoint().to_owned(),
        transport: wire_transport(context.transport()),
        provider: context
            .provider()
            .map(|provider| provider.as_str().to_owned()),
        model: context.model().map(str::to_owned),
        account_id: context
            .account_id()
            .map(|account| account.as_str().to_owned()),
        headers,
    };
    let stage = call_context.stage;
    let stream = tokio::select! {
        biased;
        () = context.cancellation().cancelled() => return Err(MiddlewareError::Fault),
        result = invocation_ports.session.call_stream(
            HANDLE_METHOD,
            call_context,
            serde_json::to_value(head).map_err(|_| MiddlewareError::InvalidState)?,
            payload,
        ) => match result {
            Ok(stream) => stream,
            Err(error) => return recover_rpc(&entry, &invocation, error).await,
        }
    };
    if !stream.initial.payload.is_empty() {
        drop(stream);
        return recover_invalid(&entry, &invocation, stage).await;
    }
    let response: MiddlewareResponseHead =
        match serde_json::from_value(stream.initial.result.clone()) {
            Ok(response) => response,
            Err(_) => {
                drop(stream);
                return recover_invalid(&entry, &invocation, stage).await;
            }
        };
    if let Some(error) = invocation.take_downstream_error() {
        return Err(error);
    }
    let completion = match invocation.resolve_response(response).await {
        Ok(completion) => completion,
        Err(error) => {
            invocation_ports.session.invalid_response(stage);
            drop(stream);
            return Err(error);
        }
    };
    let body: Box<dyn MiddlewareBody> = match completion.body {
        MiddlewareCompletionBody::PassThrough(body) => {
            drain_empty_stream(stream, &invocation, &invocation_ports.session, stage).await?;
            body
        }
        MiddlewareCompletionBody::Empty => {
            drain_empty_stream(stream, &invocation, &invocation_ports.session, stage).await?;
            Box::new(EmptyMiddlewareBody)
        }
        MiddlewareCompletionBody::Stream {
            framing,
            downstream,
        } => Box::new(PluginMiddlewareBody {
            session: Arc::clone(&invocation_ports.session),
            stage,
            stream: Some(stream),
            framing,
            terminal_seen: false,
            next_source_id: 0,
            active_source: None,
            active_output_count: 0,
            active_output_bytes: 0,
            pending_transformed: false,
            downstream_error: invocation.downstream_error_slot(),
            downstream,
            _invocation: Arc::clone(&invocation),
        }),
    };
    let response = MiddlewareResponse::new(
        completion.protocol,
        completion.status,
        completion.headers,
        body,
    );
    Ok(match completion.envelope {
        Some(envelope) => response.with_envelope(envelope),
        None => response,
    })
}

async fn recover_rpc(
    entry: &MiddlewareEntry,
    invocation: &MiddlewareInvocation,
    error: RpcError,
) -> Result<MiddlewareResponse, MiddlewareError> {
    if let Some(error) = invocation.take_downstream_error() {
        return Err(error);
    }
    let rejected = matches!(
        &error,
        RpcError::Remote(fault) if fault.code == ErrorCode::Rejected
    );
    let delegate_safe = !rejected
        && !matches!(
            &error,
            RpcError::Remote(fault) if fault.send_state != SendState::NotSent
        );
    if delegate_safe
        && entry.failure_policy == PluginFailurePolicy::Delegate
        && let Some((next, request)) = invocation.take_delegate()
    {
        tracing::warn!(
            plugin_id = entry.plugin_id,
            instance_id = entry.instance_id,
            "插件中间件在调用 next 前失败，按绑定委托下游"
        );
        return next.run(request).await;
    }
    Err(crate::callback::error::rpc_middleware(error))
}

async fn recover_invalid(
    entry: &MiddlewareEntry,
    invocation: &MiddlewareInvocation,
    stage: Stage,
) -> Result<MiddlewareResponse, MiddlewareError> {
    if let Some(ports) = &entry.invocation {
        ports.session.invalid_response(stage);
    }
    if entry.failure_policy == PluginFailurePolicy::Delegate
        && let Some((next, request)) = invocation.take_delegate()
    {
        tracing::warn!(
            plugin_id = entry.plugin_id,
            instance_id = entry.instance_id,
            "插件中间件在调用 next 前返回无效结果，按绑定委托下游"
        );
        next.run(request).await
    } else {
        Err(MiddlewareError::InvalidState)
    }
}

async fn drain_empty_stream(
    mut stream: RpcStream,
    invocation: &MiddlewareInvocation,
    session: &crate::RpcSession,
    stage: Stage,
) -> Result<(), MiddlewareError> {
    match stream.next().await {
        Ok(None) => invocation.take_downstream_error().map_or(Ok(()), Err),
        Ok(Some(_)) => {
            session.invalid_response(stage);
            Err(MiddlewareError::InvalidState)
        }
        Err(error) => invocation
            .take_downstream_error()
            .map_or_else(|| Err(crate::callback::error::rpc_middleware(error)), Err),
    }
}

struct EmptyMiddlewareBody;

impl MiddlewareBody for EmptyMiddlewareBody {
    fn next_frame(&mut self) -> BoxFuture<'_, Result<Option<MiddlewareFrame>, MiddlewareError>> {
        Box::pin(async { Ok(None) })
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async {})
    }
}

struct PluginMiddlewareBody {
    session: Arc<crate::RpcSession>,
    stage: Stage,
    stream: Option<RpcStream>,
    framing: MiddlewareFraming,
    terminal_seen: bool,
    next_source_id: u64,
    active_source: Option<u64>,
    active_output_count: usize,
    active_output_bytes: usize,
    pending_transformed: bool,
    downstream_error: Arc<Mutex<Option<MiddlewareError>>>,
    downstream: Option<MiddlewareBodyAuthority>,
    _invocation: Arc<MiddlewareInvocation>,
}

impl MiddlewareBody for PluginMiddlewareBody {
    fn next_frame(&mut self) -> BoxFuture<'_, Result<Option<MiddlewareFrame>, MiddlewareError>> {
        Box::pin(async move {
            let invalid = || {
                self.session.invalid_response(self.stage);
                MiddlewareError::InvalidState
            };
            if let Some(error) = take_error(&self.downstream_error) {
                self.stream.take();
                return Err(error);
            }
            loop {
                let Some(stream) = self.stream.as_mut() else {
                    return Ok(None);
                };
                let chunk = match stream.next().await {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        self.stream.take();
                        return Err(take_error(&self.downstream_error)
                            .unwrap_or_else(|| crate::callback::error::rpc_middleware(error)));
                    }
                };
                let Some(chunk) = chunk else {
                    self.stream.take();
                    if self.active_source.is_some()
                        || self.pending_transformed
                        || match &self.downstream {
                            Some(downstream) => downstream.has_pending_sources().await,
                            None => false,
                        }
                    {
                        return Err(invalid());
                    }
                    return take_error(&self.downstream_error).map_or(Ok(None), Err);
                };
                if self.terminal_seen {
                    self.stream.take();
                    return Err(invalid());
                }
                let wire_frame = MiddlewareBodyFrame::decode(&chunk).map_err(|_| invalid())?;
                let source_id = wire_frame.source_id();
                let disposition = wire_frame.disposition();
                let frame = match (&self.downstream, disposition) {
                    (None, MiddlewareBodyDisposition::Standalone) => Some(
                        MiddlewareFrame::new(
                            Bytes::from(wire_frame.payload),
                            self.framing,
                            wire_frame.terminal,
                        )
                        .with_transformed(true),
                    ),
                    (Some(_), MiddlewareBodyDisposition::Standalone) | (None, _) => {
                        return Err(invalid());
                    }
                    (Some(downstream), MiddlewareBodyDisposition::Only)
                    | (Some(downstream), MiddlewareBodyDisposition::Drop) => {
                        if self.active_source.is_some()
                            || source_id != self.next_source_id.saturating_add(1)
                            || wire_frame.payload.len() > MAXIMUM_MAPPED_BYTES_PER_SOURCE
                        {
                            return Err(invalid());
                        }
                        self.next_source_id = source_id;
                        downstream
                            .resolve_source_output(
                                source_id,
                                disposition,
                                Bytes::from(wire_frame.payload),
                            )
                            .await
                            .map_err(|_| invalid())?
                    }
                    (Some(downstream), MiddlewareBodyDisposition::First) => {
                        if self.active_source.is_some()
                            || source_id != self.next_source_id.saturating_add(1)
                            || wire_frame.payload.len() > MAXIMUM_MAPPED_BYTES_PER_SOURCE
                        {
                            return Err(invalid());
                        }
                        self.active_source = Some(source_id);
                        self.active_output_count = 1;
                        self.active_output_bytes = wire_frame.payload.len();
                        downstream
                            .resolve_source_output(
                                source_id,
                                disposition,
                                Bytes::from(wire_frame.payload),
                            )
                            .await
                            .map_err(|_| invalid())?
                    }
                    (Some(downstream), MiddlewareBodyDisposition::More) => {
                        self.active_output_count = self
                            .active_output_count
                            .checked_add(1)
                            .ok_or_else(invalid)?;
                        self.active_output_bytes = self
                            .active_output_bytes
                            .checked_add(wire_frame.payload.len())
                            .ok_or_else(invalid)?;
                        if self.active_source != Some(source_id)
                            || self.active_output_count > MAXIMUM_MAPPED_FRAMES_PER_SOURCE
                            || self.active_output_bytes > MAXIMUM_MAPPED_BYTES_PER_SOURCE
                        {
                            return Err(invalid());
                        }
                        downstream
                            .resolve_source_output(
                                source_id,
                                disposition,
                                Bytes::from(wire_frame.payload),
                            )
                            .await
                            .map_err(|_| invalid())?
                    }
                    (Some(downstream), MiddlewareBodyDisposition::Last) => {
                        self.active_output_count = self
                            .active_output_count
                            .checked_add(1)
                            .ok_or_else(invalid)?;
                        self.active_output_bytes = self
                            .active_output_bytes
                            .checked_add(wire_frame.payload.len())
                            .ok_or_else(invalid)?;
                        if self.active_source != Some(source_id)
                            || self.active_output_count > MAXIMUM_MAPPED_FRAMES_PER_SOURCE
                            || self.active_output_bytes > MAXIMUM_MAPPED_BYTES_PER_SOURCE
                        {
                            return Err(invalid());
                        }
                        self.active_source = None;
                        self.active_output_count = 0;
                        self.active_output_bytes = 0;
                        self.next_source_id = source_id;
                        downstream
                            .resolve_source_output(
                                source_id,
                                disposition,
                                Bytes::from(wire_frame.payload),
                            )
                            .await
                            .map_err(|_| invalid())?
                    }
                };
                if disposition == MiddlewareBodyDisposition::Drop {
                    self.pending_transformed = true;
                    if let Some(frame) = frame {
                        return Ok(Some(frame.with_transformed(true)));
                    }
                    continue;
                }
                let mut frame = frame.ok_or_else(invalid)?;
                // 只有宿主来源事件本就没有 wire 时，空 payload 才是事实帧
                let facts_only = frame.bytes().is_empty()
                    && frame
                        .event()
                        .is_some_and(|event| event.wire_event().is_none());
                if !facts_only && !valid_plugin_frame(self.framing, frame.bytes()) {
                    return Err(invalid());
                }
                if self.pending_transformed {
                    frame = frame.with_transformed(true);
                    self.pending_transformed = false;
                }
                if frame.terminal() {
                    if self.active_source.is_some()
                        || match &self.downstream {
                            Some(downstream) => downstream.has_pending_sources().await,
                            None => false,
                        }
                    {
                        self.stream.take();
                        return Err(invalid());
                    }
                    self.terminal_seen = true;
                    let terminal = stream.next().await;
                    self.stream.take();
                    if matches!(&terminal, Ok(Some(_))) {
                        self.session.invalid_response(self.stage);
                    }
                    validate_terminal_end(terminal, &self.downstream_error)?;
                }
                return Ok(Some(frame));
            }
        })
    }

    fn commit_downstream(
        &mut self,
        client_status_code: Option<u16>,
    ) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        Box::pin(async move {
            match &self.downstream {
                Some(downstream) => downstream.commit_downstream(client_status_code).await,
                None => Ok(()),
            }
        })
    }

    fn record_client_status(
        &mut self,
        client_status_code: u16,
    ) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        Box::pin(async move {
            match &self.downstream {
                Some(downstream) => downstream.record_client_status(client_status_code).await,
                None => Ok(()),
            }
        })
    }

    fn is_finalized(&self) -> bool {
        self.downstream
            .as_ref()
            .is_none_or(MiddlewareBodyAuthority::is_finalized)
    }

    fn close(mut self: Box<Self>) -> BoxFuture<'static, ()> {
        self.stream.take();
        Box::pin(async {})
    }
}

fn take_error(slot: &Mutex<Option<MiddlewareError>>) -> Option<MiddlewareError> {
    slot.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
}

fn validate_terminal_end(
    terminal: Result<Option<Vec<u8>>, RpcError>,
    downstream_error: &Mutex<Option<MiddlewareError>>,
) -> Result<(), MiddlewareError> {
    // 只有插件在终帧后继续输出才属于状态无效；RPC 传输失败保持为调用故障，
    // 两条失败路径都不能覆盖已经记录的下游域错误
    match terminal {
        Ok(None) => Ok(()),
        Ok(Some(_)) => Err(take_error(downstream_error).unwrap_or(MiddlewareError::InvalidState)),
        Err(error) => Err(take_error(downstream_error)
            .unwrap_or_else(|| crate::callback::error::rpc_middleware(error))),
    }
}

const fn wire_transport(
    transport: gateway_core::engine::execution::ClientTransport,
) -> WireMiddlewareTransport {
    match transport {
        gateway_core::engine::execution::ClientTransport::HttpJson => {
            WireMiddlewareTransport::HttpJson
        }
        gateway_core::engine::execution::ClientTransport::HttpSse => {
            WireMiddlewareTransport::HttpSse
        }
        gateway_core::engine::execution::ClientTransport::WebSocket => {
            WireMiddlewareTransport::WebSocket
        }
        gateway_core::engine::execution::ClientTransport::InternalProbe
        | gateway_core::engine::execution::ClientTransport::InternalPlugin => {
            WireMiddlewareTransport::Internal
        }
    }
}

fn valid_plugin_frame(framing: MiddlewareFraming, bytes: &[u8]) -> bool {
    match framing {
        MiddlewareFraming::JsonDocument => {
            serde_json::from_slice::<serde::de::IgnoredAny>(bytes).is_ok()
        }
        MiddlewareFraming::SseEvent => {
            (bytes.ends_with(b"\n\n") || bytes.ends_with(b"\r\n\r\n"))
                && std::str::from_utf8(bytes).is_ok_and(|event| parse_sse_events(event).is_ok())
        }
        MiddlewareFraming::RawBytes => true,
    }
}
