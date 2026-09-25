mod body;
mod headers;

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use gateway_core::engine::{
    execution::ClientTransport,
    middleware::{
        MiddlewareBody, MiddlewareContext, MiddlewareError, MiddlewareFraming, MiddlewareHeader,
        MiddlewareNext, MiddlewareRequest, MiddlewareResponseEnvelope,
    },
};
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::middleware::{
        BODY_CLOSE_METHOD, BODY_READ_METHOD, MiddlewareBodyClose, MiddlewareBodyCloseResult,
        MiddlewareBodyFraming, MiddlewareBodyHandle, MiddlewareBodyRead, MiddlewareBodyReadResult,
        MiddlewareHeader as WireHeader, MiddlewareNextRequest, MiddlewareNextResponse,
        MiddlewareRequestBody, MiddlewareResponseBody, MiddlewareResponseHead, NEXT_METHOD,
    },
};

use crate::RpcReply;

use super::PluginCallbacks;
use body::BodyResource;
pub(crate) use body::MiddlewareBodyAuthority;
use headers::{HeaderDirection, apply_header_mutations, project_headers, validate_headers};

fn request_feature(
    feature: gateway_plugin_sdk::call::middleware::RequestFeature,
) -> gateway_core::operation::Feature {
    use gateway_core::operation::Feature;
    use gateway_plugin_sdk::call::middleware::RequestFeature as Wire;
    match feature {
        Wire::Tools => Feature::Tools,
        Wire::Vision => Feature::Vision,
        Wire::Reasoning => Feature::Reasoning,
        Wire::JsonSchema => Feature::JsonSchema,
    }
}

pub(crate) struct MiddlewareInvocation {
    requests_authorized: bool,
    capabilities_allowed: bool,
    transport: ClientTransport,
    state: Mutex<InvocationState>,
    downstream_error: Arc<Mutex<Option<MiddlewareError>>>,
}

pub(crate) struct MiddlewareBinding {
    callbacks: Arc<PluginCallbacks>,
    resource_scope_id: String,
    invocation: Arc<MiddlewareInvocation>,
}

struct InvocationState {
    closed: bool,
    original_request: MiddlewareRequest,
    next: Option<Box<dyn MiddlewareNext>>,
    next_consumed: bool,
    response: Option<DownstreamResponse>,
}

struct DownstreamResponse {
    id: String,
    protocol: String,
    status: u16,
    headers: Vec<MiddlewareHeader>,
    body_handle: String,
    body: Arc<BodyResource>,
    envelope: Option<MiddlewareResponseEnvelope>,
}

pub(crate) struct MiddlewareCompletion {
    pub(crate) protocol: String,
    pub(crate) status: u16,
    pub(crate) headers: Vec<MiddlewareHeader>,
    pub(crate) body: MiddlewareCompletionBody,
    pub(crate) envelope: Option<MiddlewareResponseEnvelope>,
}

pub(crate) enum MiddlewareCompletionBody {
    Empty,
    PassThrough(Box<dyn MiddlewareBody>),
    Stream {
        framing: MiddlewareFraming,
        downstream: Option<MiddlewareBodyAuthority>,
    },
}

impl MiddlewareInvocation {
    pub(crate) fn new(
        context: &MiddlewareContext,
        request: MiddlewareRequest,
        next: Box<dyn MiddlewareNext>,
        requests_authorized: bool,
        capability_version: u32,
    ) -> Arc<Self> {
        Arc::new(Self {
            requests_authorized,
            capabilities_allowed: capability_version >= 2
                && context.mount() == gateway_core::engine::middleware::MiddlewareMount::Request
                && context.operation() == Some(gateway_core::operation::OperationKind::Generate),
            transport: context.transport(),
            state: Mutex::new(InvocationState {
                closed: false,
                original_request: request,
                next: Some(next),
                next_consumed: false,
                response: None,
            }),
            downstream_error: Arc::new(Mutex::new(None)),
        })
    }

    pub(crate) fn request_projection(
        &self,
    ) -> Result<(String, Vec<WireHeader>, bool, Vec<u8>), PluginFault> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let headers = if self.requests_authorized {
            project_headers(state.original_request.headers(), HeaderDirection::Request)?
        } else {
            Vec::new()
        };
        let body_visible = self.requests_authorized;
        let body = if body_visible {
            state.original_request.body().to_vec()
        } else {
            Vec::new()
        };
        Ok((
            state.original_request.protocol().to_owned(),
            headers,
            body_visible,
            body,
        ))
    }

    pub(crate) fn take_delegate(&self) -> Option<(Box<dyn MiddlewareNext>, MiddlewareRequest)> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.next_consumed {
            return None;
        }
        state
            .next
            .take()
            .map(|next| (next, state.original_request.clone()))
    }

    pub(crate) fn take_downstream_error(&self) -> Option<MiddlewareError> {
        self.downstream_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    pub(crate) fn close(&self) {
        let body = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.closed = true;
            state.response.take().map(|response| response.body)
        };
        if let Some(body) = body {
            body.close_detached();
        }
    }

    pub(crate) async fn call(
        self: &Arc<Self>,
        method: &str,
        params: serde_json::Value,
        payload: Vec<u8>,
        maximum_payload: usize,
    ) -> Result<RpcReply, PluginFault> {
        match method {
            NEXT_METHOD => self.call_next(params, payload).await,
            BODY_READ_METHOD => self.call_body_read(params, payload, maximum_payload).await,
            BODY_CLOSE_METHOD => self.call_body_close(params, payload).await,
            _ => Err(denied()),
        }
    }

    async fn call_next(
        self: &Arc<Self>,
        params: serde_json::Value,
        payload: Vec<u8>,
    ) -> Result<RpcReply, PluginFault> {
        let request: MiddlewareNextRequest =
            serde_json::from_value(params).map_err(|_| invalid())?;
        let (next, original) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.closed || state.next_consumed {
                return Err(conflict());
            }
            state.next_consumed = true;
            let next = state.next.take().ok_or_else(conflict)?;
            (next, state.original_request.clone())
        };
        let request = self.apply_next_request(original, request, payload)?;
        let response = match next.run(request).await {
            Ok(response) => response,
            Err(error) => {
                self.record_downstream_error(error);
                return Err(PluginFault::new(
                    ErrorCode::Fault,
                    "middleware downstream failed",
                ));
            }
        };
        let (protocol, status, headers, body, envelope) = response.into_parts();
        validate_protocol(&protocol)?;
        validate_status(status)?;
        validate_headers(&headers)?;
        let expected_framing = framing_for_response(self.transport, status);
        let response_id = uuid::Uuid::new_v4().to_string();
        let body_handle = uuid::Uuid::new_v4().to_string();
        let body = BodyResource::new(expected_framing, Arc::clone(&self.downstream_error), body);
        let visible_headers = if self.requests_authorized {
            project_headers(&headers, HeaderDirection::Response)?
        } else {
            Vec::new()
        };
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.closed || state.response.is_some() {
                drop(state);
                body.close_detached();
                return Err(denied());
            }
            state.response = Some(DownstreamResponse {
                id: response_id.clone(),
                protocol: protocol.clone(),
                status,
                headers,
                body_handle: body_handle.clone(),
                body,
                envelope,
            });
        }
        let result = MiddlewareNextResponse {
            response: response_id,
            protocol,
            status,
            headers: visible_headers,
            body: Some(MiddlewareBodyHandle {
                handle: body_handle,
                framing: wire_framing(expected_framing),
            }),
        };
        Ok(RpcReply {
            result: serde_json::to_value(result).map_err(|_| invalid())?,
            payload: Vec::new(),
        })
    }

    fn apply_next_request(
        &self,
        original: MiddlewareRequest,
        request: MiddlewareNextRequest,
        payload: Vec<u8>,
    ) -> Result<MiddlewareRequest, PluginFault> {
        let (mut protocol, mut headers, original_body) = original.clone().into_parts();
        let declaration = request
            .capabilities
            .map(|declaration| {
                if !self.requests_authorized || !self.capabilities_allowed {
                    return Err(denied());
                }
                if request.body != MiddlewareRequestBody::Replace {
                    return Err(invalid());
                }
                let handled = declaration
                    .handled
                    .iter()
                    .copied()
                    .map(request_feature)
                    .collect::<std::collections::BTreeSet<_>>();
                let required = declaration
                    .required
                    .iter()
                    .copied()
                    .map(request_feature)
                    .collect::<std::collections::BTreeSet<_>>();
                if handled.len() != declaration.handled.len()
                    || required.len() != declaration.required.len()
                {
                    return Err(invalid());
                }
                Ok(
                    gateway_core::engine::middleware::MiddlewareCapabilityDeclaration {
                        handled,
                        required,
                    },
                )
            })
            .transpose()?;
        if let Some(replacement) = request.protocol {
            if !self.requests_authorized {
                return Err(denied());
            }
            validate_protocol(&replacement)?;
            protocol = replacement;
        }
        if !request.header_mutations.is_empty() {
            if !self.requests_authorized {
                return Err(denied());
            }
            apply_header_mutations(
                &mut headers,
                &request.header_mutations,
                HeaderDirection::Request,
            )?;
        }
        let body = match request.body {
            MiddlewareRequestBody::Preserve if payload.is_empty() => original_body,
            MiddlewareRequestBody::Preserve => return Err(invalid()),
            MiddlewareRequestBody::Replace if self.requests_authorized => Bytes::from(payload),
            MiddlewareRequestBody::Replace => return Err(denied()),
        };
        original
            .replace_parts(protocol, headers, body, declaration)
            .map_err(|_| invalid())
    }

    async fn call_body_read(
        &self,
        params: serde_json::Value,
        payload: Vec<u8>,
        maximum_payload: usize,
    ) -> Result<RpcReply, PluginFault> {
        if !payload.is_empty() {
            return Err(invalid());
        }
        if !self.requests_authorized {
            return Err(denied());
        }
        let request: MiddlewareBodyRead = serde_json::from_value(params).map_err(|_| invalid())?;
        let maximum = usize::try_from(request.maximum_bytes).map_err(|_| invalid())?;
        if maximum == 0 || maximum > maximum_payload {
            return Err(invalid());
        }
        let body = self.body(&request.handle)?;
        let frame = body.read(maximum).await?;
        let (result, payload) = match frame {
            Some(frame) => (
                MiddlewareBodyReadResult {
                    framing: wire_framing(frame.framing),
                    source_id: frame.source_id,
                    eof: false,
                    terminal: frame.terminal,
                },
                frame.bytes.to_vec(),
            ),
            None => (
                MiddlewareBodyReadResult {
                    framing: wire_framing(body.expected_framing()),
                    source_id: 0,
                    eof: true,
                    terminal: false,
                },
                Vec::new(),
            ),
        };
        Ok(RpcReply {
            result: serde_json::to_value(result).map_err(|_| invalid())?,
            payload,
        })
    }

    async fn call_body_close(
        &self,
        params: serde_json::Value,
        payload: Vec<u8>,
    ) -> Result<RpcReply, PluginFault> {
        if !payload.is_empty() || !self.requests_authorized {
            return Err(denied());
        }
        let request: MiddlewareBodyClose = serde_json::from_value(params).map_err(|_| invalid())?;
        let body = self.body(&request.handle)?;
        body.close().await;
        Ok(RpcReply {
            result: serde_json::to_value(MiddlewareBodyCloseResult {}).map_err(|_| invalid())?,
            payload: Vec::new(),
        })
    }

    fn body(&self, handle: &str) -> Result<Arc<BodyResource>, PluginFault> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let response = state.response.as_ref().ok_or_else(denied)?;
        if state.closed || response.body_handle != handle {
            return Err(denied());
        }
        Ok(Arc::clone(&response.body))
    }

    pub(crate) async fn resolve_response(
        &self,
        response: MiddlewareResponseHead,
    ) -> Result<MiddlewareCompletion, MiddlewareError> {
        if let Some(error) = self.take_downstream_error() {
            return Err(error);
        }
        let downstream = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match response.response.as_deref() {
                Some(id) => {
                    let stored = state
                        .response
                        .as_mut()
                        .ok_or(MiddlewareError::InvalidState)?;
                    if stored.id != id {
                        return Err(MiddlewareError::InvalidState);
                    }
                    Some((
                        stored.protocol.clone(),
                        stored.status,
                        stored.headers.clone(),
                        stored.body_handle.clone(),
                        Arc::clone(&stored.body),
                        stored.envelope.take(),
                    ))
                }
                None => {
                    if state.response.is_some() {
                        return Err(MiddlewareError::InvalidState);
                    }
                    None
                }
            }
        };
        let (mut protocol, mut status, mut headers) = match &downstream {
            Some((protocol, status, headers, ..)) => (protocol.clone(), *status, headers.clone()),
            None => (
                response
                    .protocol
                    .clone()
                    .ok_or(MiddlewareError::InvalidState)?,
                response.status.ok_or(MiddlewareError::InvalidState)?,
                Vec::new(),
            ),
        };
        if let Some(replacement) = response.protocol {
            validate_protocol(&replacement).map_err(|_| MiddlewareError::InvalidState)?;
            if downstream.is_some() && replacement != protocol && !self.requests_authorized {
                return Err(MiddlewareError::InvalidState);
            }
            protocol = replacement;
        }
        if let Some(replacement) = response.status {
            validate_status(replacement).map_err(|_| MiddlewareError::InvalidState)?;
            if downstream.is_some() && replacement != status && !self.requests_authorized {
                return Err(MiddlewareError::InvalidState);
            }
            status = replacement;
        }
        if !response.header_mutations.is_empty() {
            if downstream.is_some() && !self.requests_authorized {
                return Err(MiddlewareError::InvalidState);
            }
            apply_header_mutations(
                &mut headers,
                &response.header_mutations,
                HeaderDirection::Response,
            )
            .map_err(|_| MiddlewareError::InvalidState)?;
        }
        let body = match response.body {
            MiddlewareResponseBody::PassThrough { body } => {
                let (_, _, _, body_handle, stored_body, _) =
                    downstream.as_ref().ok_or(MiddlewareError::InvalidState)?;
                if body_handle != &body.handle
                    || wire_framing(stored_body.expected_framing()) != body.framing
                {
                    return Err(MiddlewareError::InvalidState);
                }
                MiddlewareCompletionBody::PassThrough(
                    stored_body
                        .take_unread()
                        .await
                        .ok_or(MiddlewareError::InvalidState)?,
                )
            }
            MiddlewareResponseBody::Empty => {
                // 调用过 next 后不能靠关闭下游流伪装成已完成；需要丢弃正文时也必须
                // 通过 preserving mapper 拉取到底，才能保留计量、终态和取消合同。
                if downstream.is_some() {
                    return Err(MiddlewareError::InvalidState);
                }
                MiddlewareCompletionBody::Empty
            }
            MiddlewareResponseBody::Stream { framing } => {
                let framing = core_framing(framing);
                let expected = downstream.as_ref().map_or_else(
                    || framing_for_response(self.transport, status),
                    |(_, _, _, _, body, _)| body.expected_framing(),
                );
                if !self.requests_authorized || framing != expected {
                    return Err(MiddlewareError::InvalidState);
                }
                MiddlewareCompletionBody::Stream {
                    framing,
                    downstream: downstream.as_ref().map(|(_, _, _, _, body, _)| {
                        MiddlewareBodyAuthority::new(Arc::clone(body))
                    }),
                }
            }
        };
        Ok(MiddlewareCompletion {
            protocol,
            status,
            headers,
            body,
            envelope: downstream.and_then(|(_, _, _, _, _, envelope)| envelope),
        })
    }

    fn record_downstream_error(&self, error: MiddlewareError) {
        let mut stored = self
            .downstream_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if stored.is_none() {
            *stored = Some(error);
        }
    }

    pub(crate) fn downstream_error_slot(&self) -> Arc<Mutex<Option<MiddlewareError>>> {
        Arc::clone(&self.downstream_error)
    }
}

impl MiddlewareBinding {
    pub(super) fn new(
        callbacks: Arc<PluginCallbacks>,
        resource_scope_id: String,
        invocation: Arc<MiddlewareInvocation>,
    ) -> Self {
        Self {
            callbacks,
            resource_scope_id,
            invocation,
        }
    }
}

impl Drop for MiddlewareBinding {
    fn drop(&mut self) {
        self.callbacks
            .unbind_middleware(&self.resource_scope_id, &self.invocation);
    }
}

impl Drop for MiddlewareInvocation {
    fn drop(&mut self) {
        self.close();
    }
}

fn validate_protocol(protocol: &str) -> Result<(), PluginFault> {
    if protocol.is_empty() || protocol.len() > 64 || protocol.chars().any(char::is_control) {
        return Err(invalid());
    }
    Ok(())
}

fn validate_status(status: u16) -> Result<(), PluginFault> {
    if !(100..=599).contains(&status) {
        return Err(invalid());
    }
    Ok(())
}

const fn framing_for_response(transport: ClientTransport, status: u16) -> MiddlewareFraming {
    if status < 200 || status >= 300 {
        return MiddlewareFraming::RawBytes;
    }
    match transport {
        ClientTransport::HttpJson => MiddlewareFraming::JsonDocument,
        ClientTransport::HttpSse => MiddlewareFraming::SseEvent,
        ClientTransport::WebSocket
        | ClientTransport::InternalProbe
        | ClientTransport::InternalPlugin => MiddlewareFraming::RawBytes,
    }
}

const fn wire_framing(framing: MiddlewareFraming) -> MiddlewareBodyFraming {
    match framing {
        MiddlewareFraming::JsonDocument => MiddlewareBodyFraming::JsonDocument,
        MiddlewareFraming::SseEvent => MiddlewareBodyFraming::SseEvent,
        MiddlewareFraming::RawBytes => MiddlewareBodyFraming::RawBytes,
    }
}

pub(crate) const fn core_framing(framing: MiddlewareBodyFraming) -> MiddlewareFraming {
    match framing {
        MiddlewareBodyFraming::JsonDocument => MiddlewareFraming::JsonDocument,
        MiddlewareBodyFraming::SseEvent => MiddlewareFraming::SseEvent,
        MiddlewareBodyFraming::RawBytes => MiddlewareFraming::RawBytes,
    }
}

fn denied() -> PluginFault {
    PluginFault::new(
        ErrorCode::PermissionDenied,
        "middleware resource is not authorized",
    )
}

fn invalid() -> PluginFault {
    PluginFault::new(ErrorCode::InvalidInput, "middleware input is invalid")
}

fn conflict() -> PluginFault {
    PluginFault::new(ErrorCode::Conflict, "middleware next was already consumed")
}
