//! OpenAI Responses WebSocket adapter。

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::{
    extract::{
        Extension, State,
        connect_info::ConnectInfo,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade, close_code},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use futures::StreamExt;
use gateway_core::engine::execution::{AuthenticatedClient, ClientTransport, StartedExecution};
use gateway_core::engine::{CommitRequirement, CoordinatedEvent, EngineError};
use gateway_core::error::{GatewayError, GatewayErrorKind};
use gateway_core::event::ProviderResponseHeader;
use gateway_core::lifecycle::{ConnectionGuard, ConnectionLifecycle};
use gateway_core::operation::ProviderSessionState;
use serde_json::{Map, Value, json};
use thiserror::Error;

use crate::{
    ApiState,
    openai::{
        auth::{authenticate_client, authentication_error_response},
        error::{gateway_error_contract, gateway_error_from_engine, runtime_unavailable_response},
        router::MAX_CLIENT_REQUEST_BODY_BYTES,
        service::{OpenAiService, created_at_unix_seconds},
    },
};

use super::{
    DecodedResponsesRequest, OpenAiResponsesEncoder, ProtocolErrorBody, RequestDecodeError,
    http::{PendingExecution, request_client_context},
    request::OpenAiRequestHeaders,
};

const ACTIVE_RESPONSE_VIOLATION: &str = "Only one response.create may be active per connection";
const TEXT_FRAMES_ONLY: &str = "Responses WebSocket accepts text frames only";

/// 将已认证的 `GET /v1/responses` 升级为 Responses WebSocket。
pub(crate) async fn responses_websocket(
    State(state): State<ApiState>,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    websocket: WebSocketUpgrade,
) -> Response {
    let service = state.openai().clone();
    let client = match authenticate_client(&service, &headers) {
        Ok(client) => client,
        Err(error) => return authentication_error_response(error),
    };
    let (client_ip, user_agent) = request_client_context(
        &headers,
        connect_info.map(|Extension(ConnectInfo(address))| address),
    );
    ResponsesWebSocketAdapter::new(service).upgrade_with_client_context(
        websocket,
        client,
        client_ip,
        user_agent,
        OpenAiRequestHeaders::from_headers(&headers),
    )
}

/// 已鉴权 Responses WebSocket 升级边界。
#[derive(Clone)]
pub(crate) struct ResponsesWebSocketAdapter {
    service: OpenAiService,
}

impl ResponsesWebSocketAdapter {
    /// 绑定应用提供的唯一 OpenAI 客户端服务端口。
    #[must_use]
    pub const fn new(service: OpenAiService) -> Self {
        Self { service }
    }

    fn upgrade_with_client_context(
        &self,
        websocket: WebSocketUpgrade,
        client: AuthenticatedClient,
        client_ip: Option<IpAddr>,
        user_agent: Option<String>,
        request_headers: OpenAiRequestHeaders,
    ) -> Response {
        let connection_guard = match self.service.try_register_connection() {
            Ok(guard) => guard,
            Err(_) => return runtime_unavailable_response().into_response(),
        };
        let session = ResponsesWebSocketSession {
            service: self.service.clone(),
            client,
            connection_id: self.service.next_request_id().replacen("req_", "ws_", 1),
            client_ip,
            user_agent,
            request_headers,
            lifecycle: self.service.lifecycle(),
            _connection_guard: connection_guard,
        };
        websocket
            .max_message_size(MAX_CLIENT_REQUEST_BODY_BYTES)
            .max_frame_size(MAX_CLIENT_REQUEST_BODY_BYTES)
            .on_upgrade(move |socket| async move {
                serve_responses_websocket(socket, session).await;
            })
    }
}

struct ResponsesWebSocketSession {
    service: OpenAiService,
    client: AuthenticatedClient,
    connection_id: String,
    client_ip: Option<IpAddr>,
    user_agent: Option<String>,
    request_headers: OpenAiRequestHeaders,
    lifecycle: Arc<dyn ConnectionLifecycle>,
    _connection_guard: Box<dyn ConnectionGuard>,
}

async fn serve_responses_websocket(mut socket: WebSocket, session: ResponsesWebSocketSession) {
    tracing::info!(
        websocket_connection_id = %session.connection_id,
        "Responses WebSocket connected"
    );
    let mut request_count = 0_u64;
    let mut replay = ConnectionReplaySnapshot::default();

    let cancellation = session.lifecycle.cancellation();
    loop {
        let message = tokio::select! {
            () = cancellation.cancelled() => break,
            message = socket.next() => message,
        };
        let Some(message) = message else {
            break;
        };
        let payload = match message {
            Ok(Message::Text(payload)) => payload,
            Ok(Message::Ping(_) | Message::Pong(_)) => continue,
            Ok(Message::Close(_)) => break,
            Ok(Message::Binary(_)) => {
                close_for_protocol_violation(&mut socket, TEXT_FRAMES_ONLY).await;
                break;
            }
            Err(error) => {
                tracing::info!(
                    websocket_connection_id = %session.connection_id,
                    error = %error,
                    "Responses WebSocket receive failed"
                );
                break;
            }
        };
        request_count = request_count.saturating_add(1);
        let correlation_id = session.service.next_request_id();
        let decoded =
            match decode_response_create_with_context(payload.as_str(), &session.request_headers) {
                Ok(decoded) => {
                    decoded.with_client_context(session.client_ip, session.user_agent.clone())
                }
                Err(error) => {
                    tracing::info!(
                        websocket_connection_id = %session.connection_id,
                        request_id = %correlation_id,
                        error = %error,
                        "Responses WebSocket request rejected"
                    );
                    if send_protocol_error(
                        &mut socket,
                        StatusCode::BAD_REQUEST,
                        error.protocol_body(),
                        &correlation_id,
                    )
                    .await
                        == ForwardOutcome::Disconnect
                    {
                        break;
                    }
                    continue;
                }
            };
        let decoded = replay.prepare(decoded);
        let started = match session
            .service
            .start_response(
                session.client.clone(),
                decoded,
                ClientTransport::WebSocket,
                "/v1/responses",
            )
            .await
        {
            Ok(started) => started,
            Err(error) => {
                if send_gateway_error(&mut socket, &error, &correlation_id).await
                    == ForwardOutcome::Disconnect
                {
                    break;
                }
                continue;
            }
        };

        if forward_execution(&mut socket, started, &mut replay).await == ForwardOutcome::Disconnect
        {
            break;
        }
    }

    tracing::info!(
        websocket_connection_id = %session.connection_id,
        request_count,
        "Responses WebSocket disconnected"
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ForwardOutcome {
    Continue,
    Disconnect,
}

#[derive(Default)]
struct ConnectionReplaySnapshot {
    last_response_id: Option<String>,
    provider_state: Option<ProviderSessionState>,
}

impl ConnectionReplaySnapshot {
    fn prepare(&self, request: DecodedResponsesRequest) -> DecodedResponsesRequest {
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

async fn forward_execution(
    socket: &mut WebSocket,
    started: StartedExecution,
    replay: &mut ConnectionReplaySnapshot,
) -> ForwardOutcome {
    let request_id = started.request_id.to_string();
    let streaming = started.stream;
    let mut execution = PendingExecution::new(started.session);
    let created_at = match created_at_unix_seconds(started.created_at) {
        Ok(created_at) => created_at,
        Err(error) => return send_gateway_error(socket, &error, &request_id).await,
    };
    if !streaming {
        let error = GatewayError::new(
            GatewayErrorKind::Internal,
            "WebSocket execution was not initialized as a stream",
        );
        return send_gateway_error(socket, &error, &request_id).await;
    }
    let first = loop {
        match next_active_input(socket, &mut execution).await {
            ActiveInput::Event(Ok(Some(event))) => break event,
            ActiveInput::Event(Ok(None)) => {
                let error = GatewayError::new(
                    GatewayErrorKind::Internal,
                    "gateway response ended before its first event",
                );
                return send_gateway_error(socket, &error, &request_id).await;
            }
            ActiveInput::Event(Err(error)) => {
                let error = gateway_error_from_engine(&error);
                return send_gateway_error(socket, &error, &request_id).await;
            }
            ActiveInput::Control => continue,
            ActiveInput::Disconnect => return ForwardOutcome::Disconnect,
            ActiveInput::ProtocolViolation => {
                close_for_protocol_violation(socket, ACTIVE_RESPONSE_VIOLATION).await;
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
        return send_gateway_error(socket, &error, &request_id).await;
    }
    let mut encoder = OpenAiResponsesEncoder::new(created_at);
    let mut provider_state = None;
    let mut first_messages = Vec::new();
    for event in &mut first {
        if let Some(update) = event.take_session_update() {
            provider_state = Some(update);
        }
        match encoder.push_websocket(event) {
            Ok(messages) => first_messages.extend(messages),
            Err(error) => {
                return send_protocol_error(
                    socket,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    error.protocol_body(),
                    &request_id,
                )
                .await;
            }
        }
    }
    if first_messages.is_empty() {
        let error = GatewayError::new(
            GatewayErrorKind::Internal,
            "gateway commit batch encoded no output",
        );
        return send_gateway_error(socket, &error, &request_id).await;
    }
    let Some(response_session) = execution.session_mut() else {
        return ForwardOutcome::Disconnect;
    };
    let response_headers = response_session.response_headers().to_vec();
    if let Err(error) = response_session.commit_downstream(None).await {
        let error = gateway_error_from_engine(&error);
        return send_gateway_error(socket, &error, &request_id).await;
    }
    if encoder.is_completed() {
        if let Err(outcome) = confirm_completed_execution(socket, &mut execution, &request_id).await
        {
            return outcome;
        }
        if let Err(error) = commit_connection_replay(replay, &encoder, provider_state.take()) {
            return send_gateway_error(socket, &error, &request_id).await;
        }
        if !send_text(
            socket,
            response_metadata_event(&request_id, &response_headers),
        )
        .await
            || !send_messages(socket, first_messages).await
        {
            return ForwardOutcome::Disconnect;
        }
        execution.disarm();
        return ForwardOutcome::Continue;
    }
    if !send_text(
        socket,
        response_metadata_event(&request_id, &response_headers),
    )
    .await
        || !send_messages(socket, first_messages).await
    {
        return ForwardOutcome::Disconnect;
    }

    loop {
        match next_active_input(socket, &mut execution).await {
            ActiveInput::Event(Ok(Some(delivery))) => {
                let requirement = delivery.commit_requirement();
                let mut events = delivery.into_provider_events();
                if requirement != CommitRequirement::AlreadyCommitted {
                    let error = GatewayError::new(
                        GatewayErrorKind::Internal,
                        "gateway requested another downstream commit",
                    );
                    return send_gateway_error(socket, &error, &request_id).await;
                }
                let mut messages = Vec::new();
                for event in &mut events {
                    if let Some(update) = event.take_session_update() {
                        provider_state = Some(update);
                    }
                    match encoder.push_websocket(event) {
                        Ok(encoded) => messages.extend(encoded),
                        Err(error) => {
                            return send_protocol_error(
                                socket,
                                StatusCode::INTERNAL_SERVER_ERROR,
                                error.protocol_body(),
                                &request_id,
                            )
                            .await;
                        }
                    }
                }
                if encoder.is_completed() {
                    if let Err(outcome) =
                        confirm_completed_execution(socket, &mut execution, &request_id).await
                    {
                        return outcome;
                    }
                    if let Err(error) =
                        commit_connection_replay(replay, &encoder, provider_state.take())
                    {
                        return send_gateway_error(socket, &error, &request_id).await;
                    }
                    if !send_messages(socket, messages).await {
                        return ForwardOutcome::Disconnect;
                    }
                    execution.disarm();
                    return ForwardOutcome::Continue;
                }
                if !send_messages(socket, messages).await {
                    return ForwardOutcome::Disconnect;
                }
            }
            ActiveInput::Event(Ok(None)) => {
                let error = GatewayError::new(
                    GatewayErrorKind::Internal,
                    "gateway response ended without a terminal event",
                );
                return send_gateway_error(socket, &error, &request_id).await;
            }
            ActiveInput::Event(Err(error)) => {
                let error = gateway_error_from_engine(&error);
                return send_gateway_error(socket, &error, &request_id).await;
            }
            ActiveInput::Control => {}
            ActiveInput::Disconnect => return ForwardOutcome::Disconnect,
            ActiveInput::ProtocolViolation => {
                close_for_protocol_violation(socket, ACTIVE_RESPONSE_VIOLATION).await;
                return ForwardOutcome::Disconnect;
            }
        }
    }
}

fn commit_connection_replay(
    replay: &mut ConnectionReplaySnapshot,
    encoder: &OpenAiResponsesEncoder,
    provider_state: Option<ProviderSessionState>,
) -> Result<(), GatewayError> {
    let response_id = encoder.gateway_response_id().ok_or_else(|| {
        GatewayError::new(
            GatewayErrorKind::Internal,
            "gateway completed a response without an identity",
        )
    })?;
    replay.commit(response_id.to_owned(), provider_state);
    Ok(())
}

async fn confirm_completed_execution(
    socket: &mut WebSocket,
    execution: &mut PendingExecution,
    request_id: &str,
) -> Result<(), ForwardOutcome> {
    loop {
        match next_active_input(socket, execution).await {
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
                return Err(send_gateway_error(socket, &error, request_id).await);
            }
            ActiveInput::Event(Ok(Some(_))) => {
                let error = GatewayError::new(
                    GatewayErrorKind::Internal,
                    "gateway response continued after its terminal event",
                );
                return Err(send_gateway_error(socket, &error, request_id).await);
            }
            ActiveInput::Event(Err(error)) => {
                let error = gateway_error_from_engine(&error);
                return Err(send_gateway_error(socket, &error, request_id).await);
            }
            ActiveInput::Control => {}
            ActiveInput::Disconnect => return Err(ForwardOutcome::Disconnect),
            ActiveInput::ProtocolViolation => {
                close_for_protocol_violation(socket, ACTIVE_RESPONSE_VIOLATION).await;
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
    socket: &mut WebSocket,
    execution: &mut PendingExecution,
) -> ActiveInput {
    let Some(session) = execution.session_mut() else {
        return ActiveInput::Disconnect;
    };
    tokio::select! {
        event = session.next_event() => ActiveInput::Event(event),
        message = socket.next() => match message {
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => ActiveInput::Control,
            Some(Ok(Message::Close(_))) | None | Some(Err(_)) => ActiveInput::Disconnect,
            Some(Ok(Message::Text(_) | Message::Binary(_))) => ActiveInput::ProtocolViolation,
        },
    }
}

/// 解码一个官方 `response.create` 文本帧。
///
/// 缺省 `stream` 等价于 WebSocket 固有的流式语义；显式 `false` 会被拒绝。
///
/// # Errors
///
/// 帧不是合法 JSON object、消息类型错误、显式关闭 stream，或 Responses 请求
/// 无法映射到 canonical operation 时返回不包含正文内容的稳定错误。
pub fn decode_response_create(
    payload: &str,
) -> Result<DecodedResponsesRequest, ResponseCreateFrameError> {
    decode_response_create_inner(payload, &OpenAiRequestHeaders::default())
}

fn decode_response_create_with_context(
    payload: &str,
    request_headers: &OpenAiRequestHeaders,
) -> Result<DecodedResponsesRequest, ResponseCreateFrameError> {
    decode_response_create_inner(payload, request_headers)
}

fn decode_response_create_inner(
    payload: &str,
    request_headers: &OpenAiRequestHeaders,
) -> Result<DecodedResponsesRequest, ResponseCreateFrameError> {
    let Value::Object(mut body) = serde_json::from_str::<Value>(payload)
        .map_err(|_| ResponseCreateFrameError::InvalidJson)?
    else {
        return Err(ResponseCreateFrameError::ExpectedObject);
    };
    match body.remove("type") {
        Some(Value::String(message_type)) if message_type == "response.create" => {}
        _ => return Err(ResponseCreateFrameError::UnsupportedType),
    }
    match body.get("stream") {
        Some(Value::Bool(true)) => {}
        Some(Value::Bool(false)) => return Err(ResponseCreateFrameError::StreamingRequired),
        Some(_) => {
            return Err(ResponseCreateFrameError::Request(
                RequestDecodeError::InvalidType {
                    field: "stream".to_owned(),
                    expected: "a boolean",
                },
            ));
        }
        None => {
            body.insert("stream".to_owned(), Value::Bool(true));
        }
    }
    let encoded = serde_json::to_vec(&Value::Object(body))
        .map_err(|_| ResponseCreateFrameError::InvalidJson)?;
    super::request::decode_request_inner(&encoded, false, request_headers)
        .map_err(ResponseCreateFrameError::Request)
}

/// `response.create` 帧的稳定安全错误。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ResponseCreateFrameError {
    /// 文本不是合法 JSON。
    #[error("response.create frame must be valid JSON")]
    InvalidJson,
    /// 顶层不是 object。
    #[error("response.create frame must be a JSON object")]
    ExpectedObject,
    /// `type` 缺失或不是 `response.create`。
    #[error("unsupported Responses WebSocket message type")]
    UnsupportedType,
    /// WebSocket 请求显式声明 `stream=false`。
    #[error("Responses WebSocket requests require stream=true")]
    StreamingRequired,
    /// 内层 Responses 请求无法映射到 canonical operation。
    #[error(transparent)]
    Request(RequestDecodeError),
}

impl ResponseCreateFrameError {
    fn protocol_body(&self) -> ProtocolErrorBody {
        match self {
            Self::Request(error) => error.protocol_body(),
            Self::InvalidJson => RequestDecodeError::MalformedJson.protocol_body(),
            Self::ExpectedObject => RequestDecodeError::ExpectedObject.protocol_body(),
            Self::UnsupportedType => RequestDecodeError::InvalidValue {
                field: "type".to_owned(),
            }
            .protocol_body(),
            Self::StreamingRequired => RequestDecodeError::InvalidValue {
                field: "stream".to_owned(),
            }
            .protocol_body(),
        }
    }
}

fn response_metadata_event(
    request_id: &str,
    response_headers: &[ProviderResponseHeader],
) -> String {
    let mut headers = response_headers
        .iter()
        .filter_map(|header| {
            super::safe_response_header_name(header.name()).map(|name| {
                (
                    name.to_owned(),
                    Value::String(header.value().as_str().to_owned()),
                )
            })
        })
        .collect::<Map<String, Value>>();
    headers.insert(
        "x-request-id".to_owned(),
        Value::String(request_id.to_owned()),
    );
    json!({
        "type": "response.metadata",
        "headers": headers,
    })
    .to_string()
}

async fn send_gateway_error(
    socket: &mut WebSocket,
    error: &GatewayError,
    request_id: &str,
) -> ForwardOutcome {
    let (status, error_type, code) = gateway_error_contract(error.kind());
    if send_error_event(
        socket,
        status,
        error_type,
        code,
        error.safe_message(),
        None,
        request_id,
    )
    .await
    {
        ForwardOutcome::Continue
    } else {
        ForwardOutcome::Disconnect
    }
}

async fn send_protocol_error(
    socket: &mut WebSocket,
    status: StatusCode,
    body: ProtocolErrorBody,
    request_id: &str,
) -> ForwardOutcome {
    let error = body.error;
    if send_error_event(
        socket,
        status,
        error.kind,
        error.code,
        &error.message,
        error.param.as_deref(),
        request_id,
    )
    .await
    {
        ForwardOutcome::Continue
    } else {
        ForwardOutcome::Disconnect
    }
}

async fn send_error_event(
    socket: &mut WebSocket,
    status: StatusCode,
    error_type: &str,
    code: &str,
    message: &str,
    param: Option<&str>,
    request_id: &str,
) -> bool {
    let mut error = Map::new();
    error.insert("type".to_owned(), Value::String(error_type.to_owned()));
    error.insert("code".to_owned(), Value::String(code.to_owned()));
    error.insert("message".to_owned(), Value::String(message.to_owned()));
    if let Some(param) = param {
        error.insert("param".to_owned(), Value::String(param.to_owned()));
    }
    send_text(
        socket,
        json!({
            "type": "error",
            "status": status.as_u16(),
            "error": error,
            "request_id": request_id,
        })
        .to_string(),
    )
    .await
}

async fn send_messages(socket: &mut WebSocket, messages: Vec<String>) -> bool {
    for message in messages {
        if !send_text(socket, message).await {
            return false;
        }
    }
    true
}

async fn send_text(socket: &mut WebSocket, payload: String) -> bool {
    if let Err(error) = socket.send(Message::Text(payload.into())).await {
        tracing::info!(error = %error, "Responses WebSocket send failed");
        return false;
    }
    true
}

async fn close_for_protocol_violation(socket: &mut WebSocket, reason: &'static str) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code: close_code::POLICY,
            reason: reason.into(),
        })))
        .await;
}
