//! OpenAI Responses WebSocket 的升级、连接与串行 session 编排。

pub mod connection;
mod forward;
mod protocol;

use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use axum::{
    extract::{
        Extension, State,
        connect_info::ConnectInfo,
        ws::{WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use gateway_core::{
    diagnostics::{TraceContext, diagnostic_json},
    engine::{
        execution::{AuthenticatedClient, ClientTransport},
        middleware::MiddlewareError,
    },
    lifecycle::{ConnectionGuard, ConnectionLifecycle},
    operation::OperationKind,
};

use crate::{
    ApiState,
    openai::middleware::{
        HttpMiddlewareInput, invoke_http_middleware, request_headers as middleware_request_headers,
        request_parts,
    },
    openai::{
        auth::{authenticate_client, client_access_error_response},
        error::runtime_unavailable_response,
        service::OpenAiService,
    },
};

use super::{
    http::request_client_context, request::OpenAiRequestHeaders,
    validation::ResponseValidationFacts,
};
use connection::{ConnectionEvent, FramePhase, ResponsesWebSocketConnection, WriteContext};
use forward::{
    ConnectionReplaySnapshot, ForwardOutcome, execution_response, forward_response,
    new_replay_capture, send_gateway_error, send_middleware_error, send_protocol_error,
};
use protocol::connection_limit_event;
pub use protocol::{ResponseCreateFrameError, decode_response_create_with_context};

const TEXT_FRAMES_ONLY: &str = "Responses WebSocket accepts text frames only";
const CONNECTION_LIMIT_CLOSE_REASON: &str = "Responses websocket connection limit reached";

/// 将已认证的 `GET /v1/responses` 升级为 Responses WebSocket。
pub(crate) async fn responses_websocket(
    State(state): State<ApiState>,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    websocket: WebSocketUpgrade,
) -> Response {
    let service = state.openai().clone();
    let client = match authenticate_client(&service, &headers).await {
        Ok(client) => client,
        Err(error) => return client_access_error_response(error),
    };
    let (client_ip, user_agent) = request_client_context(
        &headers,
        connect_info.map(|Extension(ConnectInfo(address))| address),
    );
    let request_headers = OpenAiRequestHeaders::from_headers(&headers);
    ResponsesWebSocketAdapter::new(service).upgrade_with_client_context(
        websocket,
        client,
        client_ip,
        user_agent,
        request_headers,
        headers,
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
        raw_headers: HeaderMap,
    ) -> Response {
        let connection_guard = match self.service.try_register_connection() {
            Ok(guard) => guard,
            Err(_) => return runtime_unavailable_response().into_response(),
        };
        let connection_id = self.service.next_request_id().replacen("req_", "ws_", 1);
        TraceContext::new(&connection_id).headers(
            "client.connection.headers",
            serde_json::json!({"transport": "websocket", "method": "GET", "path": crate::openai::router::RESPONSES_PATH}),
            raw_headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_bytes())),
        );
        let session = ResponsesWebSocketSession {
            service: self.service.clone(),
            client,
            connection_id,
            client_ip,
            user_agent,
            request_headers,
            raw_headers,
            lifecycle: self.service.lifecycle(),
            connection_guard,
        };
        websocket
            // 覆盖 axum/tungstenite 的私有 64 MiB message 与 16 MiB frame 默认值。
            // Responses JSON 的协议可接受性由上游决定，代理不另设 wire 长度上限。
            .max_message_size(usize::MAX)
            .max_frame_size(usize::MAX)
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
    raw_headers: HeaderMap,
    lifecycle: Arc<dyn ConnectionLifecycle>,
    connection_guard: Box<dyn ConnectionGuard>,
}

async fn serve_responses_websocket(socket: WebSocket, session: ResponsesWebSocketSession) {
    let ResponsesWebSocketSession {
        service,
        client,
        connection_id,
        client_ip,
        user_agent,
        request_headers,
        raw_headers,
        lifecycle,
        connection_guard,
    } = session;
    tracing::info!(
        websocket_connection_id = %connection_id,
        "Responses WebSocket connected"
    );
    let _connection_guard = connection_guard;
    let cancellation = lifecycle.cancellation();
    let request_headers =
        request_headers.with_downstream_websocket_connection_id(connection_id.clone());
    let mut connection = ResponsesWebSocketConnection::new(socket, connection_id, cancellation);
    let mut request_count = 0_u64;
    let mut replay = ConnectionReplaySnapshot::default();

    loop {
        let Some(event) = connection.next_event().await else {
            break;
        };
        let payload = match event {
            ConnectionEvent::Text(payload) => payload,
            ConnectionEvent::Binary => {
                connection.close_policy(TEXT_FRAMES_ONLY, None).await;
                break;
            }
            ConnectionEvent::Expired => {
                expire_connection(&mut connection).await;
                break;
            }
            ConnectionEvent::Exited(_) => break,
        };
        if connection.is_expired() {
            expire_connection(&mut connection).await;
            break;
        }
        request_count = request_count.saturating_add(1);
        let correlation_id = Arc::<str>::from(service.next_request_id());
        let decoded = match decode_response_create_with_context(&payload, &request_headers) {
            Ok(decoded) => decoded,
            Err(error) => {
                trace_rejected_request(
                    &correlation_id,
                    connection.id(),
                    &payload,
                    "decode",
                    &error.to_string(),
                );
                if send_protocol_error(
                    &mut connection,
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
        // deadline 与 Text 可能同时就绪；在任何上游执行开始前再次封住该竞争窗口。
        if connection.is_expired() {
            trace_rejected_request(
                &correlation_id,
                connection.id(),
                &payload,
                "connection_expired",
                "",
            );
            expire_connection(&mut connection).await;
            break;
        }
        let execution = service.execution();
        let prepared = match execution.prepare_execution(client.clone()).await {
            Ok(prepared) => prepared,
            Err(error) => {
                trace_rejected_request(
                    &correlation_id,
                    connection.id(),
                    &payload,
                    "start",
                    &error.to_string(),
                );
                if send_gateway_error(&mut connection, &error, &correlation_id).await
                    == ForwardOutcome::Disconnect
                {
                    break;
                }
                continue;
            }
        };
        let request_id = Arc::<str>::from(prepared.request_id().to_string());
        let capture = new_replay_capture();
        let validation = ResponseValidationFacts::default();
        let input = HttpMiddlewareInput {
            endpoint: crate::openai::router::RESPONSES_PATH.to_owned(),
            protocol: "openai".to_owned(),
            operation: Some(OperationKind::Generate),
            transport: ClientTransport::WebSocket,
            model_hint: Some(decoded.metadata().requested_model().to_owned()),
            headers: middleware_request_headers(&raw_headers),
            body: Bytes::from(payload.clone()),
        };
        let service_for_terminal = service.clone();
        let replay_for_terminal = replay.clone();
        let connection_id_for_terminal = connection.id().to_owned();
        let user_agent_for_terminal = user_agent.clone();
        let request_id_for_terminal = Arc::clone(&request_id);
        let capture_for_terminal = Arc::clone(&capture);
        let validation_for_terminal = validation.clone();
        let invoke = invoke_http_middleware(
            execution,
            prepared,
            input,
            Box::new(move |prepared, request| {
                Box::pin(async move {
                    let (protocol, headers, body) = request_parts(request.clone())?;
                    if protocol != "openai" {
                        return Err(MiddlewareError::Rejected);
                    }
                    let payload =
                        std::str::from_utf8(&body).map_err(|_| MiddlewareError::Rejected)?;
                    let request_headers = OpenAiRequestHeaders::from_headers(&headers)
                        .with_downstream_websocket_connection_id(
                            connection_id_for_terminal.clone(),
                        );
                    let decoded = decode_response_create_with_context(payload, &request_headers)
                        .map_err(|_| MiddlewareError::Rejected)?
                        .with_client_context(client_ip, user_agent_for_terminal);
                    let decoded = replay_for_terminal
                        .prepare(decoded)
                        .with_middleware_capabilities(&request)?;
                    let started = service_for_terminal
                        .start_prepared_response(
                            prepared,
                            decoded,
                            ClientTransport::WebSocket,
                            crate::openai::router::RESPONSES_PATH,
                        )
                        .await
                        .map_err(MiddlewareError::Gateway)?;
                    started.session.trace().record(
                        "client.connection",
                        serde_json::json!({
                            "transport": "websocket",
                            "connectionId": connection_id_for_terminal,
                            "correlationId": request_id_for_terminal.as_ref(),
                            "requestIndex": request_count,
                        }),
                    );
                    started
                        .session
                        .trace()
                        .capture("client.request.body", &body);
                    execution_response(started, capture_for_terminal, validation_for_terminal).await
                })
            }),
        );
        let response = tokio::select! {
            biased;
            _ = connection.wait_for_exit() => break,
            response = invoke => response,
        };
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                if send_middleware_error(&mut connection, error, &request_id).await
                    == ForwardOutcome::Disconnect
                {
                    break;
                }
                continue;
            }
        };
        if forward_response(
            &mut connection,
            response,
            request_id,
            &mut replay,
            capture,
            validation,
        )
        .await
            == ForwardOutcome::Disconnect
        {
            break;
        }
        if connection.is_expired() {
            expire_connection(&mut connection).await;
            break;
        }
    }

    connection.log_summary(request_count);
}

// 尚未建立模型请求的拒绝也保留原文入口，用返回给客户端的 correlation ID 检索。
fn trace_rejected_request(
    correlation_id: &str,
    connection_id: &str,
    payload: &str,
    phase: &'static str,
    error: &str,
) {
    let trace = TraceContext::new(correlation_id);
    trace.record(
        "client.connection",
        serde_json::json!({"connectionId": connection_id}),
    );
    trace.capture("client.request.body", payload.as_bytes());
    trace.record(
        "client.request.rejected",
        serde_json::json!({
            "phase": phase, "detail": diagnostic_json(&serde_json::json!({"message": error})),
        }),
    );
}

async fn expire_connection(connection: &mut ResponsesWebSocketConnection) {
    if connection
        .send_text(
            connection_limit_event(),
            WriteContext::connection(FramePhase::ConnectionLimit),
        )
        .await
        .is_ok()
    {
        connection
            .close_for_connection_limit(CONNECTION_LIMIT_CLOSE_REASON)
            .await;
    }
}
