//! OpenAI HTTP JSON/SSE 的共享提交、背压与终结生命周期。

use super::chat_completions::ChatEncoder;
use super::error::{
    engine_error_response, gateway_error_contract, gateway_error_from_engine,
    gateway_error_response, protocol_error_response,
};
use super::responses::{OpenAiResponsesEncoder, ProtocolErrorBody, ResponseEncodeError};
use axum::{
    body::{Body, Bytes},
    http::{
        HeaderName, HeaderValue, StatusCode,
        header::{CACHE_CONTROL, CONTENT_TYPE},
    },
    response::Response,
};
use futures::{StreamExt, stream};
use gateway_core::diagnostics::TraceContext;
use gateway_core::engine::execution::ExecutionSession;
use gateway_core::engine::{CommitRequirement, EngineError};
use gateway_core::error::{GatewayError, GatewayErrorKind};
use gateway_core::event::{ProviderEvent, ProviderResponseHeader};
use gateway_core::lifecycle::ConnectionGuard;
use gateway_protocol::openai::sse::{DONE_SSE_FRAME, response_failed_sse_event_with_id};
use std::collections::VecDeque;
use std::convert::Infallible;
use std::time::Duration;

const SSE_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);

/// 编码完整 canonical event 集合，并在完整 JSON 成功后提交下游。
pub async fn collect_execution_response(session: Box<dyn ExecutionSession>) -> Response {
    collect_encoded_response(
        session,
        HttpEncoder::Responses(OpenAiResponsesEncoder::new()),
    )
    .await
}

pub(in crate::openai) async fn collect_chat_response(
    session: Box<dyn ExecutionSession>,
    encoder: ChatEncoder,
) -> Response {
    collect_encoded_response(session, HttpEncoder::Chat(Box::new(encoder))).await
}

async fn collect_encoded_response(
    session: Box<dyn ExecutionSession>,
    encoder: HttpEncoder,
) -> Response {
    let mut execution = PendingExecution::new(session);
    let Some(session) = execution.session_mut() else {
        return internal_gateway_response("gateway response session is unavailable");
    };
    let events = match session.collect_uncommitted().await {
        Ok(events) => events,
        Err(error) => {
            let response_headers = session.response_headers().to_vec();
            let response = engine_error_response_with_headers(&error, &response_headers);
            return execution.record_response_status(response).await;
        }
    };

    let encoded = match encode_collected_events(&events, encoder) {
        Ok(encoded) => encoded,
        Err(error) => {
            let response = protocol_error_response(StatusCode::INTERNAL_SERVER_ERROR, error);
            let response = execution.record_response_status(response).await;
            execution.cancel_and_finalize().await;
            return response;
        }
    };
    let response_headers = session.response_headers().to_vec();
    session.trace().record(
        "downstream.encoded",
        serde_json::json!({"transport": "http_json", "bytes": encoded.len()}),
    );
    session.trace().dump("downstream.body", &encoded);
    let response = json_body_response(encoded, &response_headers);

    let Some(session) = execution.session_mut() else {
        return internal_gateway_response("gateway response session is unavailable");
    };
    if let Err(error) = session
        .commit_downstream(Some(StatusCode::OK.as_u16()))
        .await
    {
        execution.cancel_and_finalize().await;
        let response = engine_error_response_with_headers(&error, &response_headers);
        return execution.record_response_status(response).await;
    }
    if !session.is_finalized() {
        execution.cancel_and_finalize().await;
        return internal_gateway_response("gateway response was not finalized after commit");
    }
    execution.disarm();
    response
}

fn encode_collected_events(
    events: &[ProviderEvent],
    mut encoder: HttpEncoder,
) -> Result<Vec<u8>, ProtocolErrorBody> {
    for event in events {
        encoder.push_sse(event)?;
    }
    encoder.validate_batch()?;
    let response = encoder.finish()?;
    serde_json::to_vec(&response).map_err(|_| ResponseEncodeError::Serialization.protocol_body())
}

// 两种协议只共享交付与终结顺序；Responses 的 wire frame 保持原样。
enum HttpEncoder {
    Responses(OpenAiResponsesEncoder),
    Chat(Box<ChatEncoder>),
}

impl HttpEncoder {
    fn validate_batch(&self) -> Result<(), ProtocolErrorBody> {
        match self {
            Self::Responses(_) => Ok(()),
            Self::Chat(encoder) => encoder.validate_batch(),
        }
    }
    fn push_sse(&mut self, event: &ProviderEvent) -> Result<Vec<Bytes>, ProtocolErrorBody> {
        match self {
            Self::Responses(encoder) => Ok(encoder.push_sse(event)),
            Self::Chat(encoder) => encoder.push_sse(event),
        }
    }

    fn finish(self) -> Result<serde_json::Value, ProtocolErrorBody> {
        match self {
            Self::Responses(encoder) => encoder.finish().map_err(|error| error.protocol_body()),
            Self::Chat(encoder) => encoder.finish(),
        }
    }

    fn is_completed(&self) -> bool {
        match self {
            Self::Responses(encoder) => encoder.is_completed(),
            Self::Chat(encoder) => encoder.is_completed(),
        }
    }

    fn has_wire_failure(&self) -> bool {
        match self {
            Self::Responses(encoder) => encoder.has_wire_failure(),
            Self::Chat(encoder) => encoder.has_wire_failure(),
        }
    }

    fn requires_terminal(&self) -> bool {
        matches!(self, Self::Chat(_))
    }

    fn completed_frames(&mut self) -> Vec<Bytes> {
        match self {
            Self::Responses(_) => Vec::new(),
            Self::Chat(encoder) => encoder.completed_frames(),
        }
    }

    fn error_frame(&self, kind: &str, code: &str, message: &str) -> Bytes {
        match self {
            Self::Responses(encoder) => Bytes::from(response_failed_sse_event_with_id(
                encoder.response_id(),
                kind,
                code,
                message,
            )),
            Self::Chat(_) => Bytes::from(format!(
                "data: {}\n\n",
                serde_json::json!({
                    "error": {"type": kind, "code": code, "message": message, "param": null}
                })
            )),
        }
    }
}

fn json_body_response(encoded: Vec<u8>, response_headers: &[ProviderResponseHeader]) -> Response {
    let mut response = Response::new(Body::from(encoded));
    *response.status_mut() = StatusCode::OK;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    apply_response_headers(response, response_headers)
}

fn apply_response_headers(
    mut response: Response,
    response_headers: &[ProviderResponseHeader],
) -> Response {
    let connection_options = super::responses::response_connection_options(response_headers);
    for header in response_headers {
        if !super::responses::response_header_is_forwardable(header.name(), &connection_options) {
            continue;
        }
        let Ok(name) = HeaderName::from_bytes(header.name().as_bytes()) else {
            continue;
        };
        let Ok(value) = HeaderValue::from_bytes(header.value()) else {
            continue;
        };
        response.headers_mut().append(name, value);
    }
    response
}

fn engine_error_response_with_headers(
    error: &EngineError,
    response_headers: &[ProviderResponseHeader],
) -> Response {
    let response = engine_error_response(error);
    let failure_request_ids: Vec<_> = ["x-request-id", "x-oai-request-id"]
        .into_iter()
        .flat_map(|name| {
            response
                .headers()
                .get_all(name)
                .iter()
                .cloned()
                .map(move |value| (name, value))
        })
        .collect();
    // 失败后仍须交付已采集的 turn state；只隔离 opening 身份，不丢弃会话状态。
    let mut response = apply_response_headers(response, response_headers);
    response.headers_mut().remove("x-request-id");
    response.headers_mut().remove("x-oai-request-id");
    for (name, value) in failure_request_ids {
        response.headers_mut().append(name, value);
    }
    response
}

/// 编码首个 SSE frame 后提交下游，再持续驱动同一执行会话。
pub async fn stream_execution_response(
    session: Box<dyn ExecutionSession>,
    connection_guard: Option<Box<dyn ConnectionGuard>>,
) -> Response {
    stream_encoded_response(
        session,
        connection_guard,
        HttpEncoder::Responses(OpenAiResponsesEncoder::new()),
    )
    .await
}

pub(in crate::openai) async fn stream_chat_response(
    session: Box<dyn ExecutionSession>,
    connection_guard: Option<Box<dyn ConnectionGuard>>,
    encoder: ChatEncoder,
) -> Response {
    stream_encoded_response(
        session,
        connection_guard,
        HttpEncoder::Chat(Box::new(encoder)),
    )
    .await
}

async fn stream_encoded_response(
    session: Box<dyn ExecutionSession>,
    connection_guard: Option<Box<dyn ConnectionGuard>>,
    mut encoder: HttpEncoder,
) -> Response {
    let mut execution = PendingExecution::new(session);
    let Some(session) = execution.session_mut() else {
        return internal_gateway_response("gateway response session is unavailable");
    };
    let first = match session.next_event().await {
        Ok(Some(event)) => event,
        Ok(None) => {
            let response = gateway_error_response(&GatewayError::new(
                GatewayErrorKind::Internal,
                "gateway response ended before its first event",
            ));
            let response = execution.record_response_status(response).await;
            execution.cancel_and_finalize().await;
            return response;
        }
        Err(error) => {
            let response_headers = session.response_headers().to_vec();
            let response = engine_error_response_with_headers(&error, &response_headers);
            return execution.record_response_status(response).await;
        }
    };
    let first_requirement = first.commit_requirement();
    let first_events = first.into_provider_events();
    if first_requirement != CommitRequirement::CommitBeforeDelivery {
        let response = internal_gateway_response("gateway first event did not require commit");
        let response = execution.record_response_status(response).await;
        execution.cancel_and_finalize().await;
        return response;
    }
    let mut frames = Vec::new();
    for event in &first_events {
        match encoder.push_sse(event) {
            Ok(encoded) => frames.extend(encoded),
            Err(error) => {
                let response = protocol_error_response(StatusCode::INTERNAL_SERVER_ERROR, error);
                let response = execution.record_response_status(response).await;
                execution.cancel_and_finalize().await;
                return response;
            }
        }
    }
    if let Err(error) = encoder.validate_batch() {
        let response = protocol_error_response(StatusCode::INTERNAL_SERVER_ERROR, error);
        let response = execution.record_response_status(response).await;
        execution.cancel_and_finalize().await;
        return response;
    }
    if frames.is_empty() {
        let response = internal_gateway_response("gateway commit batch encoded no output");
        let response = execution.record_response_status(response).await;
        execution.cancel_and_finalize().await;
        return response;
    }
    let Some(session) = execution.session_mut() else {
        return internal_gateway_response("gateway response session is unavailable");
    };
    let response_headers = session.response_headers().to_vec();
    if let Err(error) = session
        .commit_downstream(Some(StatusCode::OK.as_u16()))
        .await
    {
        execution.cancel_and_finalize().await;
        let response = engine_error_response_with_headers(&error, &response_headers);
        return execution.record_response_status(response).await;
    }
    let Some(session) = execution.into_session() else {
        return internal_gateway_response("gateway response session is unavailable");
    };
    let mut state = ResponsesStreamState::new(session, encoder, frames, connection_guard);
    if state.encoder.is_completed() {
        state.finish_completed(Vec::new()).await;
    }
    let output = Box::pin(stream::unfold(state, |mut state| async move {
        loop {
            if let Some(chunk) = state.pending.pop_front() {
                state.trace.dump("downstream.chunk", &chunk);
                state.handed_off_bytes += chunk.len() as u64;
                return Some((Ok::<Bytes, Infallible>(chunk), state));
            }
            if state.output_finished {
                return None;
            }
            state.advance().await;
        }
    }));
    // 保活只轮询已固定的输出 stream；不能取消并重建 advance/next_event future，
    // 否则一次心跳就可能丢失正在等待的上游事件或执行终态清理。
    let body = Body::from_stream(stream::unfold(output, |mut output| async move {
        let chunk = tokio::select! {
            biased;
            chunk = output.next() => chunk?,
            () = tokio::time::sleep(SSE_KEEPALIVE_INTERVAL) => {
                Ok(Bytes::from_static(b": keep-alive\n\n"))
            }
        };
        Some((chunk, output))
    }));
    event_stream_response(body, &response_headers)
}

fn event_stream_response(body: Body, response_headers: &[ProviderResponseHeader]) -> Response {
    let mut response = apply_response_headers(Response::new(body), response_headers);
    *response.status_mut() = StatusCode::OK;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-transform"),
    );
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
}

fn internal_gateway_response(message: &'static str) -> Response {
    gateway_error_response(&GatewayError::new(GatewayErrorKind::Internal, message))
}

pub(in crate::openai) struct PendingExecution {
    session: Option<Box<dyn ExecutionSession>>,
}

impl PendingExecution {
    pub(in crate::openai) fn new(session: Box<dyn ExecutionSession>) -> Self {
        Self {
            session: Some(session),
        }
    }

    pub(in crate::openai) fn session_mut(
        &mut self,
    ) -> Option<&mut (dyn ExecutionSession + 'static)> {
        self.session.as_deref_mut()
    }

    pub(in crate::openai) async fn cancel_and_finalize(&mut self) {
        if let Some(session) = self.session.as_mut() {
            session.cancel();
            if !session.is_finalized() {
                let _ = session.next_event().await;
            }
        }
    }

    pub(in crate::openai) async fn record_response_status(
        &mut self,
        response: Response,
    ) -> Response {
        if let Some(session) = self.session.as_mut() {
            let _ = session
                .record_client_status(response.status().as_u16())
                .await;
        }
        response
    }

    pub(in crate::openai) fn disarm(&mut self) {
        self.session = None;
    }

    fn into_session(mut self) -> Option<Box<dyn ExecutionSession>> {
        self.session.take()
    }
}

impl Drop for PendingExecution {
    fn drop(&mut self) {
        let Some(session) = self.session.take() else {
            return;
        };
        if session.is_finalized() {
            return;
        }
        session.trace().record(
            "downstream.cancelled",
            serde_json::json!({"reason": "response_guard_dropped"}),
        );
        session.cancel();
        detach_finalize(session);
    }
}

struct ResponsesStreamState {
    trace: TraceContext,
    handed_off_bytes: u64,
    session: Option<Box<dyn ExecutionSession>>,
    encoder: HttpEncoder,
    pending: VecDeque<Bytes>,
    output_finished: bool,
    execution_terminal: bool,
    _connection_guard: Option<Box<dyn ConnectionGuard>>,
}

impl ResponsesStreamState {
    fn new(
        session: Box<dyn ExecutionSession>,
        encoder: HttpEncoder,
        initial_frames: Vec<Bytes>,
        connection_guard: Option<Box<dyn ConnectionGuard>>,
    ) -> Self {
        Self {
            trace: session.trace(),
            handed_off_bytes: 0,
            session: Some(session),
            encoder,
            pending: initial_frames.into_iter().collect(),
            output_finished: false,
            execution_terminal: false,
            _connection_guard: connection_guard,
        }
    }

    async fn advance(&mut self) {
        let next = match self.session.as_mut() {
            Some(session) => session.next_event().await,
            None => {
                self.finish_with_gateway_error(GatewayError::new(
                    GatewayErrorKind::Internal,
                    "gateway response session is unavailable",
                ));
                return;
            }
        };
        match next {
            Ok(Some(event)) => {
                let requirement = event.commit_requirement();
                let events = event.into_provider_events();
                if requirement != CommitRequirement::AlreadyCommitted {
                    self.cancel_execution().await;
                    self.finish_with_gateway_error(GatewayError::new(
                        GatewayErrorKind::Internal,
                        "gateway requested another downstream commit",
                    ));
                } else {
                    for event in events {
                        self.push_event(event).await;
                        if self.output_finished {
                            break;
                        }
                    }
                    if !self.output_finished
                        && let Err(error) = self.encoder.validate_batch()
                    {
                        self.finish_with_protocol_error(error).await;
                    }
                }
            }
            Ok(None) => {
                self.execution_terminal = self
                    .session
                    .as_ref()
                    .is_some_and(|session| session.is_finalized());
                if self.execution_terminal
                    && (!self.encoder.requires_terminal() || self.encoder.has_wire_failure())
                {
                    self.pending
                        .push_back(Bytes::from_static(DONE_SSE_FRAME.as_bytes()));
                    self.output_finished = true;
                } else {
                    self.finish_with_gateway_error(GatewayError::new(
                        GatewayErrorKind::Internal,
                        "gateway response ended without finalizing the execution",
                    ));
                }
            }
            Err(error) => {
                self.execution_terminal = self
                    .session
                    .as_ref()
                    .is_some_and(|session| session.is_finalized());
                if self.encoder.has_wire_failure() {
                    self.pending
                        .push_back(Bytes::from_static(DONE_SSE_FRAME.as_bytes()));
                    self.output_finished = true;
                } else {
                    self.finish_with_gateway_error(gateway_error_from_engine(&error));
                }
            }
        }
    }

    async fn push_event(&mut self, event: ProviderEvent) {
        let frames = match self.encoder.push_sse(&event) {
            Ok(frames) => frames,
            Err(error) => {
                self.finish_with_protocol_error(error).await;
                return;
            }
        };
        if self.encoder.is_completed() {
            self.finish_completed(frames).await;
        } else {
            self.pending.extend(frames);
        }
    }

    async fn finish_with_protocol_error(&mut self, error: ProtocolErrorBody) {
        self.cancel_execution().await;
        self.pending.push_back(self.encoder.error_frame(
            error.error.kind,
            error.error.code,
            &error.error.message,
        ));
        self.pending
            .push_back(Bytes::from_static(DONE_SSE_FRAME.as_bytes()));
        self.output_finished = true;
    }

    async fn finish_completed(&mut self, frames: Vec<Bytes>) {
        let next = match self.session.as_mut() {
            Some(session) => session.next_event().await,
            None => {
                self.finish_with_gateway_error(GatewayError::new(
                    GatewayErrorKind::Internal,
                    "gateway response session is unavailable",
                ));
                return;
            }
        };
        match next {
            Ok(None)
                if self
                    .session
                    .as_ref()
                    .is_some_and(|session| session.is_finalized()) =>
            {
                self.pending.extend(frames);
                self.pending.extend(self.encoder.completed_frames());
                self.execution_terminal = true;
                self.pending
                    .push_back(Bytes::from_static(DONE_SSE_FRAME.as_bytes()));
                self.output_finished = true;
            }
            Ok(None) => {
                self.cancel_execution().await;
                self.finish_with_gateway_error(GatewayError::new(
                    GatewayErrorKind::Internal,
                    "gateway response was not finalized after its terminal event",
                ));
            }
            Ok(Some(_)) => {
                self.cancel_execution().await;
                self.finish_with_gateway_error(GatewayError::new(
                    GatewayErrorKind::Internal,
                    "gateway response continued after its terminal event",
                ));
            }
            Err(error) => {
                self.execution_terminal = self
                    .session
                    .as_ref()
                    .is_some_and(|session| session.is_finalized());
                self.finish_with_gateway_error(gateway_error_from_engine(&error));
            }
        }
    }

    async fn cancel_execution(&mut self) {
        if let Some(session) = self.session.as_mut() {
            session.cancel();
            if !session.is_finalized() {
                let _ = session.next_event().await;
            }
            self.execution_terminal = session.is_finalized();
        }
    }

    fn finish_with_gateway_error(&mut self, error: GatewayError) {
        let (_, default_type, default_code) = gateway_error_contract(error.kind());
        self.pending.push_back(self.encoder.error_frame(
            error.client_error_type().unwrap_or(default_type),
            error.client_error_code().unwrap_or(default_code),
            error.client_message(),
        ));
        self.pending
            .push_back(Bytes::from_static(DONE_SSE_FRAME.as_bytes()));
        self.output_finished = true;
    }
}

impl Drop for ResponsesStreamState {
    fn drop(&mut self) {
        self.trace.record("downstream.body.closed", serde_json::json!({
            "handedOffBytes": self.handed_off_bytes,
            "pendingChunks": self.pending.len(),
            "outputFinished": self.output_finished, "executionTerminal": self.execution_terminal,
        }));
        if self.execution_terminal {
            return;
        }
        let Some(session) = self.session.take() else {
            return;
        };
        session.cancel();
        // Body drop 后由 Core 继续状态机清理，HTTP body 生命周期不等待它。
        detach_finalize(session);
    }
}

fn detach_finalize(session: Box<dyn ExecutionSession>) {
    let finalize = session.detach_finalize();
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        drop(runtime.spawn(finalize));
    }
}
