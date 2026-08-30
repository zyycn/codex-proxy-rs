//! 已启动 Responses execution 到客户端 WebSocket wire event 的串行转发。

use std::sync::Arc;

use axum::http::StatusCode;
use gateway_core::engine::execution::StartedExecution;
use gateway_core::engine::{CommitRequirement, CoordinatedEvent, EngineError};
use gateway_core::error::{GatewayError, GatewayErrorKind};
use gateway_core::operation::ProviderSessionState;
use tokio::time::Instant;

use crate::openai::error::{gateway_error_contract, gateway_error_from_engine};

use super::{
    super::{DecodedResponsesRequest, OpenAiResponsesEncoder, PendingExecution, ProtocolErrorBody},
    connection::{ConnectionEvent, FramePhase, ResponsesWebSocketConnection, WriteContext},
    protocol::{error_event, response_metadata_event},
};

const ACTIVE_RESPONSE_VIOLATION: &str = "Only one response.create may be active per connection";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ForwardOutcome {
    Continue,
    Disconnect,
}

#[derive(Default)]
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

pub(super) async fn forward_execution(
    connection: &mut ResponsesWebSocketConnection,
    started: StartedExecution,
    replay: &mut ConnectionReplaySnapshot,
) -> ForwardOutcome {
    let request_id = Arc::<str>::from(started.request_id.to_string());
    let streaming = started.stream;
    let mut execution = PendingExecution::new(started.session);
    if !streaming {
        let error = GatewayError::new(
            GatewayErrorKind::Internal,
            "WebSocket execution was not initialized as a stream",
        );
        return send_gateway_error(connection, &error, &request_id).await;
    }
    let first = loop {
        match next_active_input(connection, &mut execution).await {
            ActiveInput::Event(Ok(Some(event))) => break event,
            ActiveInput::Event(Ok(None)) => {
                let error = GatewayError::new(
                    GatewayErrorKind::Internal,
                    "gateway response ended before its first event",
                );
                return send_gateway_error(connection, &error, &request_id).await;
            }
            ActiveInput::Event(Err(error)) => {
                let error = gateway_error_from_engine(&error);
                return send_gateway_error(connection, &error, &request_id).await;
            }
            ActiveInput::Control => continue,
            ActiveInput::Disconnect => return ForwardOutcome::Disconnect,
            ActiveInput::ProtocolViolation => {
                connection
                    .close_policy(ACTIVE_RESPONSE_VIOLATION, Some(&request_id))
                    .await;
                return ForwardOutcome::Disconnect;
            }
        }
    };
    let requirement = first.commit_requirement();
    let mut first = first.into_provider_events();
    if requirement != CommitRequirement::CommitBeforeDelivery {
        let error = GatewayError::new(
            GatewayErrorKind::Internal,
            "gateway first event did not require commit",
        );
        return send_gateway_error(connection, &error, &request_id).await;
    }
    let mut encoder = OpenAiResponsesEncoder::new();
    let mut provider_state = None;
    let mut first_messages = Vec::new();
    for event in &mut first {
        if let Some(update) = event.take_session_update() {
            provider_state = Some(update);
        }
        first_messages.extend(encoder.push_websocket(event));
    }
    if first_messages.is_empty() {
        let error = GatewayError::new(
            GatewayErrorKind::Internal,
            "gateway commit batch encoded no output",
        );
        return send_gateway_error(connection, &error, &request_id).await;
    }
    let Some(response_session) = execution.session_mut() else {
        return ForwardOutcome::Disconnect;
    };
    let response_headers = response_session.response_headers().to_vec();
    if let Err(error) = response_session.commit_downstream(None).await {
        let error = gateway_error_from_engine(&error);
        return send_gateway_error(connection, &error, &request_id).await;
    }
    let mut first_frame_written = false;
    if encoder.is_completed() {
        if let Err(outcome) =
            confirm_completed_execution(connection, &mut execution, &request_id).await
        {
            return outcome;
        }
        let provider_terminal_at = Instant::now();
        commit_connection_replay(replay, &encoder, provider_state.take());
        let writes_succeeded = send_metadata(
            connection,
            &request_id,
            response_metadata_event(&request_id, &response_headers),
        )
        .await
            && send_messages(
                connection,
                first_messages,
                &request_id,
                &mut first_frame_written,
                true,
            )
            .await;
        if !writes_succeeded {
            log_terminal_write_failure(connection, &request_id, provider_terminal_at.elapsed());
            return ForwardOutcome::Disconnect;
        }
        log_terminal_write_success(connection, &request_id, provider_terminal_at.elapsed());
        execution.disarm();
        return ForwardOutcome::Continue;
    }
    if !send_metadata(
        connection,
        &request_id,
        response_metadata_event(&request_id, &response_headers),
    )
    .await
        || !send_messages(
            connection,
            first_messages,
            &request_id,
            &mut first_frame_written,
            encoder.has_wire_failure(),
        )
        .await
    {
        return ForwardOutcome::Disconnect;
    }

    loop {
        match next_active_input(connection, &mut execution).await {
            ActiveInput::Event(Ok(Some(delivery))) => {
                let requirement = delivery.commit_requirement();
                let mut events = delivery.into_provider_events();
                if requirement != CommitRequirement::AlreadyCommitted {
                    let error = GatewayError::new(
                        GatewayErrorKind::Internal,
                        "gateway requested another downstream commit",
                    );
                    return send_gateway_error(connection, &error, &request_id).await;
                }
                let mut messages = Vec::new();
                for event in &mut events {
                    if let Some(update) = event.take_session_update() {
                        provider_state = Some(update);
                    }
                    messages.extend(encoder.push_websocket(event));
                }
                if encoder.is_completed() {
                    if let Err(outcome) =
                        confirm_completed_execution(connection, &mut execution, &request_id).await
                    {
                        return outcome;
                    }
                    let provider_terminal_at = Instant::now();
                    commit_connection_replay(replay, &encoder, provider_state.take());
                    if !send_messages(
                        connection,
                        messages,
                        &request_id,
                        &mut first_frame_written,
                        true,
                    )
                    .await
                    {
                        log_terminal_write_failure(
                            connection,
                            &request_id,
                            provider_terminal_at.elapsed(),
                        );
                        return ForwardOutcome::Disconnect;
                    }
                    log_terminal_write_success(
                        connection,
                        &request_id,
                        provider_terminal_at.elapsed(),
                    );
                    execution.disarm();
                    return ForwardOutcome::Continue;
                }
                let terminal_batch = encoder.has_wire_failure();
                if !send_messages(
                    connection,
                    messages,
                    &request_id,
                    &mut first_frame_written,
                    terminal_batch,
                )
                .await
                {
                    return ForwardOutcome::Disconnect;
                }
            }
            ActiveInput::Event(Ok(None)) => {
                if execution
                    .session_mut()
                    .is_some_and(|session| session.is_finalized())
                {
                    commit_connection_replay(replay, &encoder, provider_state.take());
                    execution.disarm();
                    return ForwardOutcome::Continue;
                }
                let error = GatewayError::new(
                    GatewayErrorKind::Internal,
                    "gateway response ended without finalizing the execution",
                );
                return send_gateway_error(connection, &error, &request_id).await;
            }
            ActiveInput::Event(Err(error)) => {
                if encoder.has_wire_failure() {
                    execution.disarm();
                    return ForwardOutcome::Continue;
                }
                let error = gateway_error_from_engine(&error);
                return send_gateway_error(connection, &error, &request_id).await;
            }
            ActiveInput::Control => {}
            ActiveInput::Disconnect => return ForwardOutcome::Disconnect,
            ActiveInput::ProtocolViolation => {
                connection
                    .close_policy(ACTIVE_RESPONSE_VIOLATION, Some(&request_id))
                    .await;
                return ForwardOutcome::Disconnect;
            }
        }
    }
}

fn commit_connection_replay(
    replay: &mut ConnectionReplaySnapshot,
    encoder: &OpenAiResponsesEncoder,
    provider_state: Option<ProviderSessionState>,
) {
    if let Some(response_id) = encoder.response_id() {
        replay.commit(response_id.to_owned(), provider_state);
    }
}

async fn confirm_completed_execution(
    connection: &mut ResponsesWebSocketConnection,
    execution: &mut PendingExecution,
    request_id: &Arc<str>,
) -> Result<(), ForwardOutcome> {
    loop {
        match next_active_input(connection, execution).await {
            ActiveInput::Event(Ok(None))
                if execution
                    .session_mut()
                    .is_some_and(|session| session.is_finalized()) =>
            {
                return Ok(());
            }
            ActiveInput::Event(Ok(None)) => {
                let error = GatewayError::new(
                    GatewayErrorKind::Internal,
                    "gateway response was not finalized after its terminal event",
                );
                return Err(send_gateway_error(connection, &error, request_id).await);
            }
            ActiveInput::Event(Ok(Some(_))) => {
                let error = GatewayError::new(
                    GatewayErrorKind::Internal,
                    "gateway response continued after its terminal event",
                );
                return Err(send_gateway_error(connection, &error, request_id).await);
            }
            ActiveInput::Event(Err(error)) => {
                let error = gateway_error_from_engine(&error);
                return Err(send_gateway_error(connection, &error, request_id).await);
            }
            ActiveInput::Control => {}
            ActiveInput::Disconnect => return Err(ForwardOutcome::Disconnect),
            ActiveInput::ProtocolViolation => {
                connection
                    .close_policy(ACTIVE_RESPONSE_VIOLATION, Some(request_id))
                    .await;
                return Err(ForwardOutcome::Disconnect);
            }
        }
    }
}

enum ActiveInput {
    Event(Result<Option<CoordinatedEvent>, EngineError>),
    Control,
    Disconnect,
    ProtocolViolation,
}

async fn next_active_input(
    connection: &mut ResponsesWebSocketConnection,
    execution: &mut PendingExecution,
) -> ActiveInput {
    let Some(session) = execution.session_mut() else {
        return ActiveInput::Disconnect;
    };
    tokio::select! {
        event = session.next_event() => ActiveInput::Event(event),
        message = connection.next_event() => match message {
            Some(ConnectionEvent::Expired) => ActiveInput::Control,
            Some(ConnectionEvent::Text(_) | ConnectionEvent::Binary) => {
                ActiveInput::ProtocolViolation
            }
            Some(ConnectionEvent::Exited(_)) | None => ActiveInput::Disconnect,
        },
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
    if connection
        .send_text(
            error_event(status, error_type, code, message, param, Some(request_id)),
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

async fn send_messages(
    connection: &mut ResponsesWebSocketConnection,
    messages: Vec<String>,
    request_id: &Arc<str>,
    first_frame_written: &mut bool,
    terminal_batch: bool,
) -> bool {
    let message_count = messages.len();
    for (index, message) in messages.into_iter().enumerate() {
        let is_terminal = terminal_batch && index + 1 == message_count;
        let phase = match (*first_frame_written, is_terminal) {
            (false, true) => FramePhase::FirstAndTerminal,
            (false, false) => FramePhase::First,
            (true, true) => FramePhase::Terminal,
            (true, false) => FramePhase::Data,
        };
        if connection
            .send_text(message, WriteContext::request(request_id, phase))
            .await
            .is_err()
        {
            return false;
        }
        *first_frame_written = true;
    }
    true
}

fn log_terminal_write_success(
    connection: &ResponsesWebSocketConnection,
    request_id: &Arc<str>,
    provider_terminal_to_write: std::time::Duration,
) {
    tracing::info!(
        websocket_connection_id = connection.id(),
        request_id = %request_id,
        provider_terminal_to_terminal_write_ms = provider_terminal_to_write.as_millis(),
        terminal_frame_written = true,
        "Provider execution terminal was written to the WebSocket transport"
    );
}

fn log_terminal_write_failure(
    connection: &ResponsesWebSocketConnection,
    request_id: &Arc<str>,
    provider_terminal_to_write: std::time::Duration,
) {
    tracing::info!(
        websocket_connection_id = connection.id(),
        request_id = %request_id,
        provider_terminal_to_terminal_write_ms = provider_terminal_to_write.as_millis(),
        provider_succeeded_but_terminal_write_failed = true,
        "Provider execution succeeded before the terminal WebSocket frame write failed"
    );
}
