//! Responses WebSocket 洋葱响应体与串行 transport 交付。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use bytes::Bytes;
use futures::future::BoxFuture;
use gateway_core::engine::CommitRequirement;
use gateway_core::engine::execution::{ExecutionSession, StartedExecution};
use gateway_core::engine::middleware::{
    MiddlewareBody, MiddlewareError, MiddlewareFrame, MiddlewareFraming, MiddlewareHeader,
    MiddlewareResponse,
};
use gateway_core::engine::response_control::ResponseControl;
use gateway_core::error::{GatewayError, GatewayErrorKind};
use gateway_core::event::ProviderResponseHeader;
use gateway_core::operation::ProviderSessionState;

use crate::openai::error::{gateway_error_contract, gateway_error_from_engine};
use crate::openai::middleware::PendingExecution;
use crate::openai::responses::validation::{ResponseValidationFacts, ResponsesDeliveryValidator};

use super::{
    super::{DecodedResponsesRequest, OpenAiResponsesEncoder, ProtocolErrorBody},
    connection::{ConnectionEvent, FramePhase, ResponsesWebSocketConnection, WriteContext},
    protocol::{
        decode_response_interrupt, error_event, initial_engine_error_event, response_metadata_event,
    },
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ForwardOutcome {
    Continue,
    Disconnect,
}

#[derive(Clone, Default)]
pub(super) struct ConnectionReplaySnapshot {
    last_response_id: Option<String>,
    provider_state: Option<ProviderSessionState>,
}

impl ConnectionReplaySnapshot {
    pub(super) fn prepare(&self, request: DecodedResponsesRequest) -> DecodedResponsesRequest {
        match (
            request.metadata().continuation().previous_response_id(),
            self.last_response_id.as_deref(),
            self.provider_state.as_ref(),
        ) {
            (Some(previous), Some(last), Some(state)) if previous == last => {
                request.with_provider_session_state(state.clone())
            }
            _ => request,
        }
    }

    fn commit(&mut self, response_id: String, provider_state: Option<ProviderSessionState>) {
        self.last_response_id = Some(response_id);
        self.provider_state = provider_state;
    }
}

#[derive(Default)]
pub(super) struct ReplayCapture {
    response_id: Option<String>,
    provider_state: Option<ProviderSessionState>,
}

pub(super) type ReplayCaptureHandle = Arc<Mutex<ReplayCapture>>;

pub(super) fn new_replay_capture() -> ReplayCaptureHandle {
    Arc::new(Mutex::new(ReplayCapture::default()))
}

pub(super) async fn execution_response(
    started: StartedExecution,
    capture: ReplayCaptureHandle,
    validation: ResponseValidationFacts,
) -> Result<MiddlewareResponse, MiddlewareError> {
    if !started.stream {
        return Err(MiddlewareError::InvalidState);
    }
    let request_id = Arc::<str>::from(started.request_id.to_string());
    let mut body = WebSocketExecutionBody::new(
        started.session,
        Arc::clone(&request_id),
        Arc::clone(&capture),
        validation,
    );
    if let Err(error) = body.prime().await {
        let headers = body.response_headers().to_vec();
        let status = middleware_error_status(&error);
        let _ = body.record_client_status(status).await;
        Box::new(body).close().await;
        if let MiddlewareError::Engine(error) = error {
            let response_headers = headers
                .iter()
                .map(|header| MiddlewareHeader::new(header.name(), header.value().clone()))
                .collect();
            return Ok(MiddlewareResponse::new(
                "openai".to_owned(),
                status,
                response_headers,
                Box::new(SingleFrameBody(Some(MiddlewareFrame::new(
                    Bytes::from(initial_engine_error_event(
                        &error,
                        request_id.as_ref(),
                        &headers,
                    )),
                    MiddlewareFraming::JsonDocument,
                    true,
                )))),
            ));
        }
        return Err(error);
    }
    let headers = body
        .response_headers()
        .iter()
        .map(|header| MiddlewareHeader::new(header.name(), header.value().clone()))
        .collect();
    Ok(MiddlewareResponse::new(
        "openai".to_owned(),
        200,
        headers,
        Box::new(body),
    ))
}

struct SingleFrameBody(Option<MiddlewareFrame>);

impl MiddlewareBody for SingleFrameBody {
    fn next_frame(&mut self) -> BoxFuture<'_, Result<Option<MiddlewareFrame>, MiddlewareError>> {
        Box::pin(async { Ok(self.0.take()) })
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async move { drop(self) })
    }
}

pub(super) async fn forward_response(
    connection: &mut ResponsesWebSocketConnection,
    response: MiddlewareResponse,
    request_id: Arc<str>,
    replay: &mut ConnectionReplaySnapshot,
    capture: ReplayCaptureHandle,
    validation: ResponseValidationFacts,
    response_control: ResponseControl,
) -> ForwardOutcome {
    let (protocol, status, headers, mut body, _) = response.into_parts();
    if protocol != "openai" || StatusCode::from_u16(status).is_err() {
        return fail_body(connection, body, MiddlewareError::InvalidState, &request_id).await;
    }
    let response_headers = headers
        .into_iter()
        .map(|header| {
            let (name, value) = header.into_parts();
            ProviderResponseHeader::new(name, value)
        })
        .collect::<Vec<_>>();
    let first = match next_body_input(connection, body.as_mut(), &response_control, &request_id)
        .await
    {
        BodyInput::Frame(Ok(Some(frame))) => frame,
        BodyInput::Frame(Ok(None)) => {
            return fail_body(connection, body, MiddlewareError::InvalidState, &request_id).await;
        }
        BodyInput::Frame(Err(error)) => {
            return fail_body(connection, body, error, &request_id).await;
        }
        BodyInput::Disconnect => {
            detach_body(body);
            return ForwardOutcome::Disconnect;
        }
    };
    if let Err(error) = validate_frame(&first) {
        return fail_body(connection, body, error, &request_id).await;
    }
    let mut delivery_validator = ResponsesDeliveryValidator::default();
    if delivery_validator
        .validate_websocket_frame(first.bytes(), first.transformed(), &validation)
        .is_err()
    {
        return fail_body(connection, body, MiddlewareError::InvalidState, &request_id).await;
    }
    if let Err(error) = body.commit_downstream(None).await {
        return fail_body(connection, body, error, &request_id).await;
    }
    if !send_metadata(
        connection,
        &request_id,
        response_metadata_event(&request_id, &response_headers),
    )
    .await
    {
        detach_body(body);
        return ForwardOutcome::Disconnect;
    }

    let mut first_frame_written = false;
    let mut current = Some(first);
    let mut terminal_seen = false;
    let mut first_protocol_validated = true;
    loop {
        let frame = match current.take() {
            Some(frame) => frame,
            None => {
                match next_body_input(connection, body.as_mut(), &response_control, &request_id)
                    .await
                {
                    BodyInput::Frame(Ok(Some(frame))) => frame,
                    BodyInput::Frame(Ok(None)) if terminal_seen && body.is_finalized() => {
                        if delivery_validator.finish_websocket_delivery().is_err() {
                            return fail_body(
                                connection,
                                body,
                                MiddlewareError::InvalidState,
                                &request_id,
                            )
                            .await;
                        }
                        {
                            let captured = capture
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            if let Some(response_id) = captured.response_id.clone() {
                                replay.commit(response_id, captured.provider_state.clone());
                            }
                        }
                        body.close().await;
                        return ForwardOutcome::Continue;
                    }
                    BodyInput::Frame(Ok(None)) => {
                        return fail_body(
                            connection,
                            body,
                            MiddlewareError::InvalidState,
                            &request_id,
                        )
                        .await;
                    }
                    BodyInput::Frame(Err(error)) => {
                        return fail_body(connection, body, error, &request_id).await;
                    }
                    BodyInput::Disconnect => {
                        detach_body(body);
                        return ForwardOutcome::Disconnect;
                    }
                }
            }
        };
        if terminal_seen || validate_frame(&frame).is_err() {
            return fail_body(connection, body, MiddlewareError::InvalidState, &request_id).await;
        }
        if first_protocol_validated {
            first_protocol_validated = false;
        } else if delivery_validator
            .validate_websocket_frame(frame.bytes(), frame.transformed(), &validation)
            .is_err()
        {
            return fail_body(connection, body, MiddlewareError::InvalidState, &request_id).await;
        }
        let terminal = frame.terminal();
        let Ok(message) = String::from_utf8(frame.into_bytes().to_vec()) else {
            return fail_body(connection, body, MiddlewareError::InvalidState, &request_id).await;
        };
        let phase = match (first_frame_written, terminal) {
            (false, true) => FramePhase::FirstAndTerminal,
            (false, false) => FramePhase::First,
            (true, true) => FramePhase::Terminal,
            (true, false) => FramePhase::Data,
        };
        if connection
            .send_text(message, WriteContext::request(&request_id, phase))
            .await
            .is_err()
        {
            detach_body(body);
            return ForwardOutcome::Disconnect;
        }
        first_frame_written = true;
        terminal_seen = terminal;
    }
}

fn detach_body(body: Box<dyn MiddlewareBody>) {
    // 客户端已离线时不能让连接 handler 等待可能仍在结算的请求；close future
    // 继续持有唯一正文和执行守卫，保证已启动的费用与租约清理不会被取消。
    drop(tokio::spawn(body.close()));
}

fn validate_frame(frame: &MiddlewareFrame) -> Result<(), MiddlewareError> {
    if frame.framing() != MiddlewareFraming::JsonDocument
        || serde_json::from_slice::<serde::de::IgnoredAny>(frame.bytes()).is_err()
    {
        return Err(MiddlewareError::InvalidState);
    }
    Ok(())
}

enum BodyInput {
    Frame(Result<Option<MiddlewareFrame>, MiddlewareError>),
    Disconnect,
}

async fn next_body_input(
    connection: &mut ResponsesWebSocketConnection,
    body: &mut dyn MiddlewareBody,
    response_control: &ResponseControl,
    request_id: &Arc<str>,
) -> BodyInput {
    // 处理控制帧时继续持有同一个读取 future，不能取消正在加工正文的中间件。
    let frame = body.next_frame();
    tokio::pin!(frame);
    loop {
        tokio::select! {
            event = connection.next_active_event() => {
                let Some(event) = event else { return BodyInput::Disconnect; };
                match &event.event {
                    ConnectionEvent::Text(payload) => {
                        let error = match decode_response_interrupt(payload) {
                            Ok(Some(response_id)) => response_control.interrupt(&response_id).err().map(|_| {
                                super::super::RequestDecodeError::InvalidValue { field: "response_id".to_owned() }.protocol_body()
                            }),
                            Ok(None) => {
                                connection.defer(event);
                                continue;
                            }
                            Err(error) => Some(error.protocol_body()),
                        };
                        if let Some(error) = error
                            && send_protocol_error(connection, StatusCode::BAD_REQUEST, error, request_id).await == ForwardOutcome::Disconnect
                        {
                            return BodyInput::Disconnect;
                        }
                    }
                    ConnectionEvent::Expired => {
                        // 允许当前响应收尾，外层仍会在下一轮准入前关闭过期连接。
                    }
                    ConnectionEvent::Binary => {
                        connection.defer(event);
                    }
                    ConnectionEvent::Exited(_) => return BodyInput::Disconnect,
                }
            }
            frame = &mut frame => return BodyInput::Frame(frame),
        }
    }
}

async fn fail_body(
    connection: &mut ResponsesWebSocketConnection,
    mut body: Box<dyn MiddlewareBody>,
    error: MiddlewareError,
    request_id: &Arc<str>,
) -> ForwardOutcome {
    let gateway = middleware_gateway_error(error);
    let _ = body
        .record_client_status(gateway_error_contract(gateway.kind()).0.as_u16())
        .await;
    body.close().await;
    send_gateway_error(connection, &gateway, request_id).await
}

fn middleware_gateway_error(error: MiddlewareError) -> GatewayError {
    match error {
        MiddlewareError::Gateway(error) => error,
        MiddlewareError::Engine(error) => gateway_error_from_engine(&error),
        MiddlewareError::Provider(error) => GatewayError::from_provider(&error),
        MiddlewareError::Rejected => GatewayError::new(
            GatewayErrorKind::PolicyDenied,
            "request middleware rejected the request",
        ),
        MiddlewareError::Fault | MiddlewareError::InvalidState => GatewayError::new(
            GatewayErrorKind::Internal,
            "request middleware returned an invalid response",
        ),
    }
}

fn middleware_error_status(error: &MiddlewareError) -> u16 {
    match error {
        MiddlewareError::Gateway(error) => gateway_error_contract(error.kind()).0.as_u16(),
        MiddlewareError::Engine(error) => {
            gateway_error_contract(gateway_error_from_engine(error).kind())
                .0
                .as_u16()
        }
        MiddlewareError::Provider(error) => {
            gateway_error_contract(GatewayError::from_provider(error).kind())
                .0
                .as_u16()
        }
        MiddlewareError::Rejected => StatusCode::FORBIDDEN.as_u16(),
        MiddlewareError::Fault | MiddlewareError::InvalidState => {
            StatusCode::INTERNAL_SERVER_ERROR.as_u16()
        }
    }
}

pub(super) async fn send_middleware_error(
    connection: &mut ResponsesWebSocketConnection,
    error: MiddlewareError,
    request_id: &Arc<str>,
) -> ForwardOutcome {
    send_gateway_error(connection, &middleware_gateway_error(error), request_id).await
}

struct WebSocketExecutionBody {
    execution: PendingExecution,
    encoder: OpenAiResponsesEncoder,
    validation: ResponseValidationFacts,
    pending: VecDeque<MiddlewareFrame>,
    provider_state: Option<ProviderSessionState>,
    request_id: Arc<str>,
    capture: ReplayCaptureHandle,
    batch_pending: bool,
    transformed_pending: bool,
    awaiting_terminal_eof: bool,
    committed: bool,
    finished: bool,
}

impl WebSocketExecutionBody {
    fn new(
        session: Box<dyn ExecutionSession>,
        request_id: Arc<str>,
        capture: ReplayCaptureHandle,
        validation: ResponseValidationFacts,
    ) -> Self {
        Self {
            execution: PendingExecution::new(session),
            encoder: OpenAiResponsesEncoder::new(),
            validation,
            pending: VecDeque::new(),
            provider_state: None,
            request_id,
            capture,
            batch_pending: false,
            transformed_pending: false,
            awaiting_terminal_eof: false,
            committed: false,
            finished: false,
        }
    }

    async fn prime(&mut self) -> Result<(), MiddlewareError> {
        self.fill_pending().await?;
        if self.pending.is_empty() {
            return Err(MiddlewareError::InvalidState);
        }
        Ok(())
    }

    fn response_headers(&mut self) -> &[ProviderResponseHeader] {
        self.execution
            .session_mut()
            .map_or(&[], |session| session.response_headers())
    }

    async fn fill_pending(&mut self) -> Result<(), MiddlewareError> {
        while self.pending.is_empty() && !self.finished {
            if self.awaiting_terminal_eof {
                if !self.committed {
                    return Err(MiddlewareError::InvalidState);
                }
                let result = self
                    .execution
                    .session_mut()
                    .ok_or(MiddlewareError::InvalidState)?
                    .next_event()
                    .await;
                match result {
                    Ok(None) if self.execution.is_finalized() => {
                        let mut capture = self
                            .capture
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        capture.response_id = self.encoder.response_id().map(str::to_owned);
                        capture.provider_state = self.provider_state.take();
                        drop(capture);
                        self.execution.disarm();
                        self.finished = true;
                        continue;
                    }
                    Err(_) if self.encoder.has_wire_failure() && self.execution.is_finalized() => {
                        self.execution.disarm();
                        self.finished = true;
                        continue;
                    }
                    Ok(None | Some(_)) => return Err(MiddlewareError::InvalidState),
                    Err(error) => return Err(MiddlewareError::Engine(error)),
                }
            }
            if self.batch_pending {
                if !self.committed {
                    self.execution
                        .session_mut()
                        .ok_or(MiddlewareError::InvalidState)?
                        .discard_pending_delivery()
                        .map_err(MiddlewareError::Engine)?;
                }
                self.batch_pending = false;
            }
            let result = self
                .execution
                .session_mut()
                .ok_or(MiddlewareError::InvalidState)?
                .next_event()
                .await;
            let delivery = match result {
                Ok(Some(delivery)) => delivery,
                Ok(None) => return Err(MiddlewareError::InvalidState),
                Err(error) if self.committed => {
                    self.push_gateway_error(gateway_error_from_engine(&error));
                    continue;
                }
                Err(error) => return Err(MiddlewareError::Engine(error)),
            };
            let expected = if self.committed {
                CommitRequirement::AlreadyCommitted
            } else {
                CommitRequirement::CommitBeforeDelivery
            };
            if delivery.commit_requirement() != expected {
                return Err(MiddlewareError::InvalidState);
            }
            let mut messages = Vec::new();
            for mut event in delivery.into_provider_events() {
                self.validation.observe_event(&event);
                let transformed = self.transformed_pending || event.middleware_transformed();
                if let Some(update) = event.take_session_update() {
                    self.provider_state = Some(update);
                }
                let encoded = self.encoder.push_websocket(&event);
                if encoded.is_empty() {
                    self.transformed_pending = transformed;
                    continue;
                }
                messages.extend(encoded.into_iter().map(|message| (message, transformed)));
                self.transformed_pending = false;
            }
            let terminal = self.encoder.is_completed() || self.encoder.has_wire_failure();
            let last = messages.len().saturating_sub(1);
            self.pending.extend(messages.into_iter().enumerate().map(
                |(index, (message, transformed))| {
                    MiddlewareFrame::new(
                        Bytes::from(message),
                        MiddlewareFraming::JsonDocument,
                        terminal && index == last,
                    )
                    .with_transformed(transformed)
                },
            ));
            self.awaiting_terminal_eof = terminal;
            self.batch_pending = !self.committed;
            if self.pending.is_empty() {
                if terminal {
                    return Err(MiddlewareError::InvalidState);
                }
                continue;
            }
        }
        Ok(())
    }

    fn push_gateway_error(&mut self, error: GatewayError) {
        let (status, default_type, default_code) = gateway_error_contract(error.kind());
        self.pending.push_back(MiddlewareFrame::new(
            Bytes::from(error_event(
                status,
                error.client_error_type().unwrap_or(default_type),
                error.client_error_code().unwrap_or(default_code),
                error.client_message(),
                None,
                Some(&self.request_id),
                serde_json::Map::new(),
            )),
            MiddlewareFraming::JsonDocument,
            true,
        ));
        self.finished = true;
    }
}

impl MiddlewareBody for WebSocketExecutionBody {
    fn next_frame(&mut self) -> BoxFuture<'_, Result<Option<MiddlewareFrame>, MiddlewareError>> {
        Box::pin(async move {
            if let Some(frame) = self.pending.pop_front() {
                return Ok(Some(frame));
            }
            if self.finished {
                return Ok(None);
            }
            self.fill_pending().await?;
            Ok(self.pending.pop_front())
        })
    }

    fn commit_downstream(
        &mut self,
        status: Option<u16>,
    ) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        Box::pin(async move {
            if self.committed {
                return Ok(());
            }
            self.execution
                .session_mut()
                .ok_or(MiddlewareError::InvalidState)?
                .commit_downstream(status)
                .await
                .map_err(MiddlewareError::Engine)?;
            self.committed = true;
            self.batch_pending = false;
            Ok(())
        })
    }

    fn record_client_status(&mut self, status: u16) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        Box::pin(async move {
            let Some(session) = self.execution.session_mut() else {
                return Ok(());
            };
            session
                .record_client_status(status)
                .await
                .map_err(MiddlewareError::Engine)
        })
    }

    fn is_finalized(&self) -> bool {
        self.execution.is_finalized()
    }

    fn close(mut self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async move { self.execution.cancel_and_finalize().await })
    }
}

pub(super) async fn send_gateway_error(
    connection: &mut ResponsesWebSocketConnection,
    error: &GatewayError,
    request_id: &Arc<str>,
) -> ForwardOutcome {
    let (status, default_type, default_code) = gateway_error_contract(error.kind());
    send_error(
        connection,
        status,
        error.client_error_type().unwrap_or(default_type),
        error.client_error_code().unwrap_or(default_code),
        error.client_message(),
        None,
        request_id,
    )
    .await
}

pub(super) async fn send_protocol_error(
    connection: &mut ResponsesWebSocketConnection,
    status: StatusCode,
    body: ProtocolErrorBody,
    request_id: &Arc<str>,
) -> ForwardOutcome {
    let error = body.error;
    send_error(
        connection,
        status,
        error.kind,
        error.code,
        &error.message,
        error.param.as_deref(),
        request_id,
    )
    .await
}

async fn send_error(
    connection: &mut ResponsesWebSocketConnection,
    status: StatusCode,
    error_type: &str,
    code: &str,
    message: &str,
    param: Option<&str>,
    request_id: &Arc<str>,
) -> ForwardOutcome {
    let message = error_event(
        status,
        error_type,
        code,
        message,
        param,
        Some(request_id),
        serde_json::Map::new(),
    );
    if connection
        .send_text(
            message,
            WriteContext::request(request_id, FramePhase::Error),
        )
        .await
        .is_ok()
    {
        ForwardOutcome::Continue
    } else {
        ForwardOutcome::Disconnect
    }
}

async fn send_metadata(
    connection: &mut ResponsesWebSocketConnection,
    request_id: &Arc<str>,
    message: String,
) -> bool {
    connection
        .send_text(
            message,
            WriteContext::request(request_id, FramePhase::Metadata),
        )
        .await
        .is_ok()
}
