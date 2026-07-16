//! 官方 Responses WebSocket 入站协议。

use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use futures::{SinkExt, StreamExt};
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use serde_json::{Map, Value, json};
use thiserror::Error;
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Error as WebSocketError, Message,
        handshake::server::create_response_with_body,
        protocol::{CloseFrame, Role, WebSocketConfig, frame::coding::CloseCode},
    },
};

use crate::{
    api::{AppState, middleware::request_id::RequestId},
    dispatch::{
        service::{ConnectionTranscriptFacts, ResponseDispatchStream},
        transport::canonical::CanonicalResponseEvent,
    },
    upstream::openai::protocol::websocket::is_terminal_websocket_event,
};

use super::{PreparedResponsesRequest, ResponsesRequestValidationError, prepare_responses_request};
use crate::api::client::{
    auth::authorized_client_api_key_id,
    errors::{
        missing_client_api_key_response, openai_error_response,
        responses_websocket_dispatch_error_event, responses_websocket_error_event,
    },
    router::MAX_CLIENT_REQUEST_BODY_BYTES,
};

const RESPONSES_ROUTE: &str = "/v1/responses";
const INVALID_REQUEST_MESSAGE: &str = "Invalid responses request";
const MODEL_NOT_FOUND_MESSAGE: &str = "Model not found";
const INVALID_STREAM_MESSAGE: &str = "Responses WebSocket requests require stream=true";
const INVALID_UPSTREAM_EVENT_MESSAGE: &str = "Invalid upstream Responses event";
const STREAM_DISCONNECTED_MESSAGE: &str = "Upstream stream closed before response.completed";

type ResponsesWebSocket = WebSocketStream<TokioIo<Upgraded>>;

/// 将已认证的 `GET /v1/responses` 请求升级为官方 Responses WebSocket 协议。
pub async fn responses_websocket(State(state): State<AppState>, mut request: Request) -> Response {
    let headers = request.headers().clone();
    let Some(client_api_key_id) = authorized_client_api_key_id(&state, &headers).await else {
        return missing_client_api_key_response().into_response();
    };
    let connection_request_id = request
        .extensions()
        .get::<RequestId>()
        .cloned()
        .unwrap_or_else(RequestId::generate);
    let client_ip = request
        .extensions()
        .get::<crate::api::middleware::request_id::ClientIp>()
        .map(|client_ip| client_ip.as_str().to_string());
    let connection_id = connection_request_id.as_str().to_string();
    let response = match create_response_with_body(&request, Body::empty) {
        Ok(response) => response,
        Err(error) => {
            tracing::info!(
                websocket_connection_id = %connection_id,
                error = %error,
                "Invalid Responses WebSocket upgrade request"
            );
            return openai_error_response(
                StatusCode::BAD_REQUEST,
                "Invalid Responses WebSocket upgrade request",
                "invalid_request_error",
                "invalid_websocket_upgrade",
            )
            .into_response();
        }
    };
    let upgrade = hyper::upgrade::on(&mut request);
    let connection_drain = state.services.connection_drain.clone();
    connection_drain.spawn(async move {
        match upgrade.await {
            Ok(upgraded) => {
                let socket = WebSocketStream::from_raw_socket(
                    TokioIo::new(upgraded),
                    Role::Server,
                    Some(downstream_websocket_config()),
                )
                .await;
                serve_responses_websocket(
                    socket,
                    ResponsesWebSocketSession {
                        state,
                        headers,
                        client_ip,
                        client_api_key_id,
                        connection_id,
                    },
                )
                .await;
            }
            Err(error) => {
                tracing::warn!(
                    websocket_connection_id = %connection_id,
                    error = %error,
                    "Responses WebSocket upgrade failed"
                );
            }
        }
    });
    response
}

fn downstream_websocket_config() -> WebSocketConfig {
    let mut config = WebSocketConfig::default();
    config.max_message_size = Some(MAX_CLIENT_REQUEST_BODY_BYTES);
    config.max_frame_size = Some(MAX_CLIENT_REQUEST_BODY_BYTES);
    config
}

struct ResponsesWebSocketSession {
    state: AppState,
    headers: HeaderMap,
    client_ip: Option<String>,
    client_api_key_id: String,
    connection_id: String,
}

struct ConnectionTranscriptCollector {
    account_id: String,
    request_input: Vec<Value>,
    continued_from_previous_response: bool,
    completed_items: Vec<Value>,
    completed: Option<ConnectionTranscriptFacts>,
}

enum ForwardDispatchOutcome {
    Continue(Option<ConnectionTranscriptFacts>),
    Disconnect,
}

impl ConnectionTranscriptCollector {
    fn new(
        account_id: String,
        request_input: Vec<Value>,
        continued_from_previous_response: bool,
    ) -> Self {
        Self {
            account_id,
            request_input,
            continued_from_previous_response,
            completed_items: Vec::new(),
            completed: None,
        }
    }

    fn observe(&mut self, event: &CanonicalResponseEvent) {
        match event.event_type() {
            Some("response.output_item.done") => {
                if let Some(item) = event.data().get("item") {
                    self.completed_items.push(item.clone());
                }
            }
            Some("response.completed") => {
                let Some(response_id) = event
                    .data()
                    .pointer("/response/id")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
                else {
                    return;
                };
                let output = event
                    .data()
                    .pointer("/response/output")
                    .and_then(Value::as_array)
                    .filter(|output| !output.is_empty())
                    .cloned()
                    .unwrap_or_else(|| self.completed_items.clone());
                self.completed = Some(ConnectionTranscriptFacts::new(
                    self.account_id.clone(),
                    self.request_input.clone(),
                    self.continued_from_previous_response,
                    response_id,
                    output,
                ));
            }
            _ => {}
        }
    }
}

async fn serve_responses_websocket(
    mut socket: ResponsesWebSocket,
    session: ResponsesWebSocketSession,
) {
    tracing::info!(
        websocket_connection_id = %session.connection_id,
        "Responses WebSocket connected"
    );
    let mut request_count = 0u64;
    let mut replay_snapshot = session
        .state
        .services
        .responses
        .connection_replay_snapshot();

    while let Some(message) = socket.next().await {
        let payload = match message {
            Ok(Message::Text(payload)) => payload,
            Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => continue,
            Ok(Message::Close(_)) => break,
            Ok(Message::Binary(_)) => {
                close_for_protocol_violation(
                    &mut socket,
                    "Responses WebSocket accepts text frames only",
                )
                .await;
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
        let request_id = RequestId::generate();
        let body = match parse_response_create(payload.as_str()) {
            Ok(body) => body,
            Err(error) => {
                tracing::info!(
                    websocket_connection_id = %session.connection_id,
                    request_id = %request_id.as_str(),
                    error = %error,
                    "Responses WebSocket request rejected"
                );
                if !send_error(
                    &mut socket,
                    StatusCode::BAD_REQUEST,
                    "invalid_request_error",
                    "invalid_request",
                    &error.to_string(),
                    request_id.as_str(),
                )
                .await
                {
                    break;
                }
                continue;
            }
        };
        let mut prepared = match prepare_responses_request(
            &session.state,
            body,
            &session.headers,
            None,
            session.client_ip.clone(),
            session.client_api_key_id.clone(),
        )
        .await
        {
            Ok(prepared) => prepared,
            Err(error) => {
                let (status, code, message) = match error {
                    ResponsesRequestValidationError::InvalidRequest => (
                        StatusCode::BAD_REQUEST,
                        "invalid_request",
                        INVALID_REQUEST_MESSAGE,
                    ),
                    ResponsesRequestValidationError::ModelNotFound => (
                        StatusCode::NOT_FOUND,
                        "model_not_found",
                        MODEL_NOT_FOUND_MESSAGE,
                    ),
                };
                if !send_error(
                    &mut socket,
                    status,
                    "invalid_request_error",
                    code,
                    message,
                    request_id.as_str(),
                )
                .await
                {
                    break;
                }
                continue;
            }
        };
        let replay_plan = session
            .state
            .services
            .responses
            .prepare_connection_replay(&replay_snapshot, &mut prepared.request);

        match dispatch_response_create(&mut socket, &session, request_id, prepared).await {
            ForwardDispatchOutcome::Continue(Some(transcript)) => {
                session.state.services.responses.commit_connection_replay(
                    &mut replay_snapshot,
                    replay_plan,
                    transcript,
                );
            }
            ForwardDispatchOutcome::Continue(None) => {}
            ForwardDispatchOutcome::Disconnect => break,
        }
    }

    tracing::info!(
        websocket_connection_id = %session.connection_id,
        request_count,
        "Responses WebSocket disconnected"
    );
}

async fn dispatch_response_create(
    socket: &mut ResponsesWebSocket,
    session: &ResponsesWebSocketSession,
    request_id: RequestId,
    prepared: PreparedResponsesRequest,
) -> ForwardDispatchOutcome {
    let PreparedResponsesRequest {
        mut request,
        requested_model,
    } = prepared;
    if !request.force_http_sse {
        request.use_websocket = true;
    }
    let stream = session
        .state
        .services
        .responses
        .stream(
            request_id.as_str(),
            RESPONSES_ROUTE,
            request,
            &requested_model,
        )
        .await;
    let stream = match stream {
        Ok(stream) => stream,
        Err(error) => {
            let event = responses_websocket_dispatch_error_event(&error, request_id.as_str());
            return if send_text(socket, event).await {
                ForwardDispatchOutcome::Continue(None)
            } else {
                ForwardDispatchOutcome::Disconnect
            };
        }
    };
    forward_dispatch_stream(socket, stream, request_id.as_str()).await
}

async fn forward_dispatch_stream(
    socket: &mut ResponsesWebSocket,
    stream: ResponseDispatchStream,
    request_id: &str,
) -> ForwardDispatchOutcome {
    let ResponseDispatchStream {
        account_id,
        request_input,
        continued_from_previous_response,
        body,
        mut canonical_events,
        response_headers,
    } = stream;
    if !send_text(
        socket,
        response_metadata_event(request_id, &response_headers),
    )
    .await
    {
        return ForwardDispatchOutcome::Disconnect;
    }

    let mut body = body;
    let mut terminal_sent = false;
    let mut completed = ConnectionTranscriptCollector::new(
        account_id,
        request_input,
        continued_from_previous_response,
    );
    loop {
        let next = if terminal_sent {
            ActiveResponseInput::Upstream(body.next().await)
        } else {
            tokio::select! {
                next = body.next() => ActiveResponseInput::Upstream(next),
                message = socket.next() => ActiveResponseInput::Downstream(message),
            }
        };
        match next {
            ActiveResponseInput::Upstream(Some(Ok(chunk))) => {
                let Some(events) = canonical_events.recv().await else {
                    tracing::warn!(
                        request_id,
                        "Canonical Responses event stream closed before body stream"
                    );
                    return ForwardDispatchOutcome::Disconnect;
                };
                drop(chunk);
                if !forward_canonical_events(
                    socket,
                    events,
                    request_id,
                    &mut terminal_sent,
                    &mut completed,
                )
                .await
                {
                    return ForwardDispatchOutcome::Disconnect;
                }
            }
            ActiveResponseInput::Upstream(Some(Err(error))) => {
                tracing::warn!(request_id, error = %error, "Responses dispatch stream failed");
                if terminal_sent {
                    return ForwardDispatchOutcome::Continue(completed.completed);
                }
                return if send_error(
                    socket,
                    StatusCode::BAD_GATEWAY,
                    "server_error",
                    "upstream_error",
                    STREAM_DISCONNECTED_MESSAGE,
                    request_id,
                )
                .await
                {
                    ForwardDispatchOutcome::Continue(None)
                } else {
                    ForwardDispatchOutcome::Disconnect
                };
            }
            ActiveResponseInput::Upstream(None) => {
                if terminal_sent {
                    return ForwardDispatchOutcome::Continue(completed.completed);
                }
                return if send_error(
                    socket,
                    StatusCode::BAD_GATEWAY,
                    "server_error",
                    "stream_disconnected",
                    STREAM_DISCONNECTED_MESSAGE,
                    request_id,
                )
                .await
                {
                    ForwardDispatchOutcome::Continue(None)
                } else {
                    ForwardDispatchOutcome::Disconnect
                };
            }
            ActiveResponseInput::Downstream(Some(Ok(
                Message::Ping(_) | Message::Pong(_) | Message::Frame(_),
            ))) => {}
            ActiveResponseInput::Downstream(Some(Ok(Message::Close(_))) | None) => {
                return ForwardDispatchOutcome::Disconnect;
            }
            ActiveResponseInput::Downstream(Some(Ok(Message::Text(_) | Message::Binary(_)))) => {
                close_for_protocol_violation(
                    socket,
                    "Only one response.create may be active per connection",
                )
                .await;
                return ForwardDispatchOutcome::Disconnect;
            }
            ActiveResponseInput::Downstream(Some(Err(error))) => {
                tracing::info!(request_id, error = %error, "Responses WebSocket client disconnected during response");
                return ForwardDispatchOutcome::Disconnect;
            }
        }
    }
}

enum ActiveResponseInput {
    Upstream(Option<Result<bytes::Bytes, crate::dispatch::errors::ResponseDispatchStreamError>>),
    Downstream(Option<Result<Message, WebSocketError>>),
}

async fn forward_canonical_events(
    socket: &mut ResponsesWebSocket,
    events: Vec<CanonicalResponseEvent>,
    request_id: &str,
    terminal_sent: &mut bool,
    completed: &mut ConnectionTranscriptCollector,
) -> bool {
    for event in events {
        let Some(event_type) = event.event_type() else {
            tracing::warn!(
                request_id,
                "Responses SSE event does not contain a valid JSON type"
            );
            let _ = send_error(
                socket,
                StatusCode::BAD_GATEWAY,
                "server_error",
                "invalid_upstream_response",
                INVALID_UPSTREAM_EVENT_MESSAGE,
                request_id,
            )
            .await;
            return false;
        };
        completed.observe(&event);
        if !send_text(socket, event.data().to_string()).await {
            return false;
        }
        *terminal_sent |= is_terminal_websocket_event(event_type);
    }
    true
}

fn response_metadata_event(request_id: &str, response_headers: &[(String, String)]) -> String {
    let mut headers = response_headers
        .iter()
        .map(|(name, value)| (name.clone(), Value::String(value.clone())))
        .collect::<Map<String, Value>>();
    headers.insert(
        "x-request-id".to_string(),
        Value::String(request_id.to_string()),
    );
    json!({
        "type": "response.metadata",
        "headers": headers,
    })
    .to_string()
}

fn parse_response_create(payload: &str) -> Result<Map<String, Value>, ResponseCreateFrameError> {
    let Value::Object(mut body) = serde_json::from_str::<Value>(payload)? else {
        return Err(ResponseCreateFrameError::ExpectedObject);
    };
    match body.remove("type") {
        Some(Value::String(message_type)) if message_type == "response.create" => {}
        _ => return Err(ResponseCreateFrameError::UnsupportedType),
    }
    if matches!(body.get("stream"), Some(value) if value.as_bool() != Some(true)) {
        return Err(ResponseCreateFrameError::StreamingRequired);
    }
    Ok(body)
}

#[derive(Debug, Error)]
enum ResponseCreateFrameError {
    #[error("Invalid response.create JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("A response.create payload must be a JSON object")]
    ExpectedObject,
    #[error("Unsupported Responses WebSocket message type")]
    UnsupportedType,
    #[error("{INVALID_STREAM_MESSAGE}")]
    StreamingRequired,
}

async fn send_error(
    socket: &mut ResponsesWebSocket,
    status: StatusCode,
    error_type: &str,
    code: &str,
    message: &str,
    request_id: &str,
) -> bool {
    send_text(
        socket,
        responses_websocket_error_event(status, error_type, code, message, request_id),
    )
    .await
}

async fn send_text(socket: &mut ResponsesWebSocket, payload: String) -> bool {
    if let Err(error) = socket.send(Message::Text(payload.into())).await {
        tracing::info!(error = %error, "Responses WebSocket send failed");
        return false;
    }
    true
}

async fn close_for_protocol_violation(socket: &mut ResponsesWebSocket, reason: &'static str) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code: CloseCode::Policy,
            reason: reason.into(),
        })))
        .await;
}
