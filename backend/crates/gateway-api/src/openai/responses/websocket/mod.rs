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
use gateway_core::{
    engine::execution::{AuthenticatedClient, ClientTransport},
    lifecycle::{ConnectionGuard, ConnectionLifecycle},
};

use crate::{
    ApiState,
    openai::{
        auth::{authenticate_client, client_access_error_response},
        error::runtime_unavailable_response,
        service::OpenAiService,
    },
};

use super::{http::request_client_context, request::OpenAiRequestHeaders};
use connection::{ConnectionEvent, FramePhase, ResponsesWebSocketConnection, WriteContext};
use forward::{
    ConnectionReplaySnapshot, ForwardOutcome, forward_execution, send_gateway_error,
    send_protocol_error,
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
    let client = match authenticate_client(&service, &headers) {
        Ok(client) => client,
        Err(error) => return client_access_error_response(error),
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
            Ok(decoded) => decoded.with_client_context(client_ip, user_agent.clone()),
            Err(error) => {
                tracing::info!(
                    websocket_connection_id = connection.id(),
                    request_id = %correlation_id,
                    error = %error,
                    "Responses WebSocket request rejected"
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
        let decoded = replay.prepare(decoded);
        // deadline 与 Text 可能同时就绪；在任何上游执行开始前再次封住该竞争窗口。
        if connection.is_expired() {
            expire_connection(&mut connection).await;
            break;
        }
        let started = match service
            .start_response(
                client.clone(),
                decoded,
                ClientTransport::WebSocket,
                "/v1/responses",
            )
            .await
        {
            Ok(started) => started,
            Err(error) => {
                if send_gateway_error(&mut connection, &error, &correlation_id).await
                    == ForwardOutcome::Disconnect
                {
                    break;
                }
                continue;
            }
        };

        if forward_execution(&mut connection, started, &mut replay).await
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
