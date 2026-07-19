//! OpenAI Responses WebSocket adapter。

use std::net::{IpAddr, SocketAddr};

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
use gateway_core::engine::{CommitRequirement, EngineError};
use gateway_core::error::{GatewayError, GatewayErrorKind};
use gateway_core::event::GatewayEvent;
use serde_json::{Map, Value, json};
use thiserror::Error;

use crate::openai::{
    auth::{authenticate_client, authentication_error_response},
    error::{gateway_error_contract, gateway_error_from_engine, runtime_unavailable_response},
    router::MAX_CLIENT_REQUEST_BODY_BYTES,
    service::{
        DeliveryEvent, OpenAiApiState, OpenAiClientService, ResponseExecutionSession,
        ResponsesTransport, StartedResponse,
    },
};

use super::{
    DecodedResponsesRequest, ProtocolErrorBody, RequestDecodeError, ResponsesCollector,
    decode_request,
    http::{PendingExecution, request_client_context},
};

const ACTIVE_RESPONSE_VIOLATION: &str = "Only one response.create may be active per connection";
const TEXT_FRAMES_ONLY: &str = "Responses WebSocket accepts text frames only";

/// 将已认证的 `GET /v1/responses` 升级为 Responses WebSocket。
pub async fn responses_websocket<S>(
    State(state): State<S>,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    websocket: WebSocketUpgrade,
) -> Response
where
    S: OpenAiApiState,
{
    let service = state.openai_client_api();
    let client = match authenticate_client(&service, &headers) {
        Ok(client) => client,
        Err(error) => return authentication_error_response(error),
    };
    let (client_ip, user_agent) = request_client_context(
        &headers,
        connect_info.map(|Extension(ConnectInfo(address))| address),
    );
    ResponsesWebSocketAdapter::new(service)
        .upgrade_with_client_context(websocket, client, client_ip, user_agent)
}

/// 已鉴权 Responses WebSocket 升级边界。
#[derive(Clone)]
pub struct ResponsesWebSocketAdapter<S> {
    service: S,
}

impl<S> ResponsesWebSocketAdapter<S>
where
    S: OpenAiClientService,
{
    /// 绑定应用提供的唯一 OpenAI 客户端服务端口。
    #[must_use]
    pub const fn new(service: S) -> Self {
        Self { service }
    }

    /// 升级一个已认证的请求；明文 API key 不进入长连接 session。
    #[must_use]
    pub fn upgrade(&self, websocket: WebSocketUpgrade, client: S::Client) -> Response {
        self.upgrade_with_client_context(websocket, client, None, None)
    }

    fn upgrade_with_client_context(
        &self,
        websocket: WebSocketUpgrade,
        client: S::Client,
        client_ip: Option<IpAddr>,
        user_agent: Option<String>,
    ) -> Response {
        if self.service.is_shutting_down() {
            return runtime_unavailable_response().into_response();
        }
        let session = ResponsesWebSocketSession {
            service: self.service.clone(),
            client,
            connection_id: self.service.next_connection_id(),
            client_ip,
            user_agent,
        };
        let lifecycle = self.service.clone();
        websocket
            .max_message_size(MAX_CLIENT_REQUEST_BODY_BYTES)
            .max_frame_size(MAX_CLIENT_REQUEST_BODY_BYTES)
            .on_upgrade(move |socket| async move {
                lifecycle.spawn_connection(Box::pin(serve_responses_websocket(socket, session)));
            })
    }
}

struct ResponsesWebSocketSession<S>
where
    S: OpenAiClientService,
{
    service: S,
    client: S::Client,
    connection_id: String,
    client_ip: Option<IpAddr>,
    user_agent: Option<String>,
}

async fn serve_responses_websocket<S>(mut socket: WebSocket, session: ResponsesWebSocketSession<S>)
where
    S: OpenAiClientService,
{
    tracing::info!(
        websocket_connection_id = %session.connection_id,
        "Responses WebSocket connected"
    );
    let mut request_count = 0_u64;

    while let Some(message) = socket.next().await {
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
        let decoded = match decode_response_create(payload.as_str()) {
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
        let started = match session
            .service
            .start_response(
                session.client.clone(),
                decoded,
                ResponsesTransport::WebSocket,
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

        if forward_execution(&mut socket, started).await == ForwardOutcome::Disconnect {
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

async fn forward_execution<S>(socket: &mut WebSocket, started: StartedResponse<S>) -> ForwardOutcome
where
    S: ResponseExecutionSession,
{
    let request_id = started.request_id().to_owned();
    let (session, created_at, streaming) = started.into_parts();
    let mut execution = PendingExecution::new(session);
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
    let (first, requirement) = first.into_parts();
    if requirement != CommitRequirement::CommitBeforeDelivery {
        let error = GatewayError::new(
            GatewayErrorKind::Internal,
            "gateway first event did not require commit",
        );
        return send_gateway_error(socket, &error, &request_id).await;
    }
    let mut collector = ResponsesCollector::new(created_at);
    let first_messages = match collector.push_websocket_events(&first) {
        Ok(messages) if !messages.is_empty() => messages,
        Ok(_) => {
            let error = GatewayError::new(
                GatewayErrorKind::Internal,
                "gateway first event encoded no WebSocket message",
            );
            return send_gateway_error(socket, &error, &request_id).await;
        }
        Err(error) => {
            return send_protocol_error(
                socket,
                StatusCode::INTERNAL_SERVER_ERROR,
                error.protocol_body(),
                &request_id,
            )
            .await;
        }
    };
    let Some(response_session) = execution.session_mut() else {
        return ForwardOutcome::Disconnect;
    };
    if let Err(error) = response_session.commit_downstream(None).await {
        let error = gateway_error_from_engine(&error);
        return send_gateway_error(socket, &error, &request_id).await;
    }
    if !send_text(socket, response_metadata_event(&request_id)).await
        || !send_messages(socket, first_messages).await
    {
        return ForwardOutcome::Disconnect;
    }

    loop {
        match next_active_input(socket, &mut execution).await {
            ActiveInput::Event(Ok(Some(delivery))) => {
                let (event, requirement) = delivery.into_parts();
                if requirement != CommitRequirement::AlreadyCommitted {
                    let error = GatewayError::new(
                        GatewayErrorKind::Internal,
                        "gateway requested another downstream commit",
                    );
                    return send_gateway_error(socket, &error, &request_id).await;
                }
                let terminal = matches!(event, GatewayEvent::Completed(_));
                let messages = match collector.push_websocket_events(&event) {
                    Ok(messages) => messages,
                    Err(error) => {
                        return send_protocol_error(
                            socket,
                            StatusCode::INTERNAL_SERVER_ERROR,
                            error.protocol_body(),
                            &request_id,
                        )
                        .await;
                    }
                };
                if terminal {
                    if collector.finish().is_err() {
                        let error = GatewayError::new(
                            GatewayErrorKind::Internal,
                            "gateway terminal event did not finish the response encoder",
                        );
                        return send_gateway_error(socket, &error, &request_id).await;
                    }
                    loop {
                        match next_active_input(socket, &mut execution).await {
                            ActiveInput::Event(Ok(None))
                                if execution
                                    .session_mut()
                                    .is_some_and(|session| session.is_finalized()) =>
                            {
                                break;
                            }
                            ActiveInput::Event(Ok(None)) => {
                                let error = GatewayError::new(
                                    GatewayErrorKind::Internal,
                                    "gateway response was not finalized after its terminal event",
                                );
                                return send_gateway_error(socket, &error, &request_id).await;
                            }
                            ActiveInput::Event(Ok(Some(_))) => {
                                let error = GatewayError::new(
                                    GatewayErrorKind::Internal,
                                    "gateway response continued after its terminal event",
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
                                close_for_protocol_violation(socket, ACTIVE_RESPONSE_VIOLATION)
                                    .await;
                                return ForwardOutcome::Disconnect;
                            }
                        }
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

enum ActiveInput {
    Event(Result<Option<DeliveryEvent>, EngineError>),
    Control,
    Disconnect,
    ProtocolViolation,
}

async fn next_active_input<S>(
    socket: &mut WebSocket,
    execution: &mut PendingExecution<S>,
) -> ActiveInput
where
    S: ResponseExecutionSession,
{
    let Some(session) = execution.session_mut() else {
        return ActiveInput::Disconnect;
    };
    tokio::select! {
        event = session.next_delivery_event() => ActiveInput::Event(event),
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
    decode_request(&encoded).map_err(ResponseCreateFrameError::Request)
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

fn response_metadata_event(request_id: &str) -> String {
    json!({
        "type": "response.metadata",
        "headers": {"x-request-id": request_id},
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
