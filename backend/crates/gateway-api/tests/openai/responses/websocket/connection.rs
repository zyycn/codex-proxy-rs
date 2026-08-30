use std::{
    fmt,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, SystemTime},
};

use axum::{
    extract::ws::Message,
    http::{StatusCode, header::AUTHORIZATION},
};
use futures::{Sink, SinkExt, Stream, StreamExt, future::BoxFuture};
use gateway_core::{
    engine::{
        CancellationToken, CommitRequirement, CoordinatedEvent, EngineError,
        execution::{
            AuthenticatedClient, ClientAuthenticationError, ExecutionService, ExecutionSession,
            StartExecution, StartProviderExecution, StartedExecution,
        },
    },
    error::GatewayError,
    event::{GatewayEvent, ProtocolWireEvent, ProviderEvent, ProviderResponseHeader, ResponseMeta},
    routing::PublicModelId,
};
use serde_json::{Value, json};
use tokio::sync::mpsc::{
    UnboundedReceiver, UnboundedSender, error::TryRecvError, unbounded_channel,
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        Message as ClientMessage, client::IntoClientRequest, protocol::frame::coding::CloseCode,
    },
};

use super::{AtomicFailureExecution, AtomicFailureTrace};
use crate::openai::{api_router, authenticated_client};

#[derive(Default)]
struct CrossingLimitTrace {
    release_terminal: tokio::sync::Notify,
    started_emitted: AtomicBool,
    terminal_emitted: AtomicBool,
    finalized: AtomicBool,
}

struct CrossingLimitSession {
    trace: Arc<CrossingLimitTrace>,
}

impl ExecutionSession for CrossingLimitSession {
    fn next_event(&mut self) -> BoxFuture<'_, Result<Option<CoordinatedEvent>, EngineError>> {
        Box::pin(async move {
            if !self.trace.started_emitted.swap(true, Ordering::AcqRel) {
                return Ok(Some(crossing_limit_started_batch()));
            }
            if !self.trace.terminal_emitted.load(Ordering::Acquire) {
                self.trace.release_terminal.notified().await;
                if !self.trace.terminal_emitted.swap(true, Ordering::AcqRel) {
                    return Ok(Some(crossing_limit_terminal_batch()));
                }
            }
            self.trace.finalized.store(true, Ordering::Release);
            Ok(None)
        })
    }

    fn collect_uncommitted(&mut self) -> BoxFuture<'_, Result<Vec<ProviderEvent>, EngineError>> {
        Box::pin(async { Err(EngineError::InvalidDeliveryState) })
    }

    fn response_headers(&self) -> &[ProviderResponseHeader] {
        &[]
    }

    fn commit_downstream(&mut self, _: Option<u16>) -> BoxFuture<'_, Result<(), EngineError>> {
        Box::pin(async { Ok(()) })
    }

    fn record_client_status(&mut self, _: u16) -> BoxFuture<'_, Result<(), EngineError>> {
        Box::pin(async { Ok(()) })
    }

    fn is_finalized(&self) -> bool {
        self.trace.finalized.load(Ordering::Acquire)
    }

    fn cancel(&self) {}

    fn detach_finalize(self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            self.trace.finalized.store(true, Ordering::Release);
        })
    }
}

struct CrossingLimitExecution {
    client: AuthenticatedClient,
    trace: Arc<CrossingLimitTrace>,
}

impl ExecutionService for CrossingLimitExecution {
    fn authenticate(
        &self,
        plaintext: &str,
    ) -> Result<AuthenticatedClient, ClientAuthenticationError> {
        (plaintext == "sk_ws_crossing_limit")
            .then(|| self.client.clone())
            .ok_or(ClientAuthenticationError::InvalidKey)
    }

    fn public_models(&self, _: &AuthenticatedClient) -> Vec<PublicModelId> {
        vec![PublicModelId::new("model-a").expect("public model")]
    }

    fn contains_public_model(&self, _: &AuthenticatedClient, model: &PublicModelId) -> bool {
        model.as_str() == "model-a"
    }

    fn start(
        &self,
        request: StartExecution,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async move {
            Ok(StartedExecution {
                request_id: gateway_core::engine::ModelRequestId::new("req_ws_crossing_limit")
                    .expect("request id"),
                created_at: SystemTime::now(),
                stream: request.metadata.stream,
                session: Box::new(CrossingLimitSession {
                    trace: Arc::clone(&self.trace),
                }),
            })
        })
    }

    fn start_provider_endpoint(
        &self,
        _: StartProviderExecution,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async { unreachable!("WebSocket test does not execute provider endpoints") })
    }
}

fn crossing_limit_started_batch() -> CoordinatedEvent {
    let started = ProviderEvent::canonical_with_wire(
        vec![GatewayEvent::Started(ResponseMeta::new(
            "resp_ws_crossing_limit",
            "model-a",
        ))],
        ProtocolWireEvent::json(
            "openai",
            Some("response.created".to_owned()),
            json!({
                "type": "response.created",
                "response": {
                    "id": "resp_ws_crossing_limit",
                    "model": "model-a",
                    "status": "in_progress"
                }
            }),
        )
        .expect("created wire"),
    );
    CoordinatedEvent::try_batch(vec![started], CommitRequirement::CommitBeforeDelivery)
        .expect("started WebSocket batch")
}

fn crossing_limit_terminal_batch() -> CoordinatedEvent {
    let completed = ProviderEvent::wire(
        ProtocolWireEvent::json(
            "openai",
            Some("response.completed".to_owned()),
            json!({
                "type": "response.completed",
                "response": {
                    "id": "resp_ws_crossing_limit",
                    "model": "model-a",
                    "status": "completed"
                }
            }),
        )
        .expect("completed wire"),
    );
    CoordinatedEvent::try_batch(vec![completed], CommitRequirement::AlreadyCommitted)
        .expect("terminal WebSocket batch")
}

// 连接 runtime 没有正式公共 API；镜像测试直接编译该私有模块，以测试 socket pump，
// 不把测试钩子暴露给 gateway-api 的消费者。
#[expect(
    dead_code,
    reason = "the mirrored test only exercises pump-facing seams"
)]
#[path = "../../../../src/openai/responses/websocket/connection.rs"]
mod connection_under_test;

use connection_under_test::{
    ConnectionConfig, ConnectionEvent, ConnectionWriteError, FramePhase,
    ResponsesWebSocketConnection, WriteContext, spawn_connection,
};

#[derive(Debug)]
struct TestSocketError;

impl fmt::Display for TestSocketError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("test socket error")
    }
}

struct TestSocket {
    incoming: UnboundedReceiver<Result<Message, TestSocketError>>,
    written: UnboundedSender<Message>,
    stall_writes: bool,
    dropped: Arc<AtomicBool>,
}

impl Stream for TestSocket {
    type Item = Result<Message, TestSocketError>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.incoming.poll_recv(context)
    }
}

impl Sink<Message> for TestSocket {
    type Error = TestSocketError;

    fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if self.stall_writes {
            Poll::Pending
        } else {
            Poll::Ready(Ok(()))
        }
    }

    fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        self.written.send(item).map_err(|_| TestSocketError)
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

impl Drop for TestSocket {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::Release);
    }
}

struct PumpHarness {
    connection: ResponsesWebSocketConnection,
    incoming: UnboundedSender<Result<Message, TestSocketError>>,
    written: UnboundedReceiver<Message>,
    dropped: Arc<AtomicBool>,
}

fn test_connection(stall_writes: bool) -> PumpHarness {
    let (incoming_tx, incoming_rx) = unbounded_channel();
    let (written_tx, written_rx) = unbounded_channel();
    let dropped = Arc::new(AtomicBool::new(false));
    let socket = TestSocket {
        incoming: incoming_rx,
        written: written_tx,
        stall_writes,
        dropped: Arc::clone(&dropped),
    };
    let connection = spawn_connection(
        socket,
        Arc::from("ws_test"),
        CancellationToken::new(),
        ConnectionConfig::PRODUCTION,
    );
    PumpHarness {
        connection,
        incoming: incoming_tx,
        written: written_rx,
        dropped,
    }
}

#[tokio::test]
async fn pump_replies_to_ping_with_the_same_pong_payload() {
    let PumpHarness {
        connection,
        incoming,
        mut written,
        ..
    } = test_connection(false);
    incoming
        .send(Ok(Message::Ping(vec![1, 2, 3].into())))
        .expect("send Ping to test socket");

    let message = written.recv().await.expect("pump must write Pong");
    let Message::Pong(payload) = message else {
        panic!("pump wrote a non-Pong control frame");
    };
    assert_eq!(payload.as_ref(), &[1, 2, 3]);
    drop(connection);
}

#[tokio::test(start_paused = true)]
async fn pump_consumes_pong_without_forwarding_a_business_event() {
    let PumpHarness {
        mut connection,
        incoming,
        ..
    } = test_connection(false);
    incoming
        .send(Ok(Message::Pong(vec![4, 5, 6].into())))
        .expect("send Pong to test socket");

    let result = tokio::time::timeout(Duration::from_secs(1), connection.next_event()).await;
    assert!(result.is_err(), "Pong unexpectedly reached the coordinator");
}

#[tokio::test(start_paused = true)]
async fn pump_does_not_emit_an_active_ping() {
    let PumpHarness {
        connection,
        incoming: _incoming,
        mut written,
        ..
    } = test_connection(false);
    tokio::time::advance(Duration::from_secs(5 * 60)).await;
    tokio::task::yield_now().await;

    assert!(matches!(written.try_recv(), Err(TryRecvError::Empty)));
    drop(connection);
}

#[tokio::test]
async fn outbound_commands_are_written_in_acknowledged_order() {
    let PumpHarness {
        mut connection,
        incoming: _incoming,
        mut written,
        ..
    } = test_connection(false);
    let request_id = Arc::<str>::from("req_test");

    connection
        .send_text(
            "first".to_owned(),
            WriteContext::request(&request_id, FramePhase::First),
        )
        .await
        .expect("first write acknowledgement");
    connection
        .send_text(
            "second".to_owned(),
            WriteContext::request(&request_id, FramePhase::Terminal),
        )
        .await
        .expect("second write acknowledgement");

    let first = written.recv().await.expect("first wire message");
    let second = written.recv().await.expect("second wire message");
    assert!(
        matches!(first, Message::Text(payload) if payload.as_str() == "first")
            && matches!(second, Message::Text(payload) if payload.as_str() == "second")
    );
}

#[tokio::test(start_paused = true)]
async fn stalled_write_times_out_at_three_hundred_seconds_and_aborts_the_pump() {
    let PumpHarness {
        mut connection,
        incoming: _incoming,
        written: _written,
        dropped,
    } = test_connection(true);
    let request_id = Arc::<str>::from("req_test");

    let error = connection
        .send_text(
            "payload".to_owned(),
            WriteContext::request(&request_id, FramePhase::First),
        )
        .await
        .expect_err("stalled write must time out");
    tokio::task::yield_now().await;

    assert!(
        matches!(error, ConnectionWriteError::Timeout { timeout } if timeout == Duration::from_secs(300))
            && dropped.load(Ordering::Acquire)
    );
}

#[tokio::test(start_paused = true)]
async fn pump_marks_the_connection_expired_at_sixty_minutes() {
    let PumpHarness {
        mut connection,
        incoming: _incoming,
        written: _written,
        ..
    } = test_connection(false);

    let event = connection
        .next_event()
        .await
        .expect("deadline must emit an event");

    assert!(matches!(event, ConnectionEvent::Expired) && connection.is_expired());
}

#[tokio::test]
async fn dropping_the_connection_aborts_and_drops_the_socket_owner() {
    let PumpHarness {
        connection,
        incoming: _incoming,
        written: _written,
        dropped,
    } = test_connection(false);

    drop(connection);
    tokio::task::yield_now().await;

    assert!(dropped.load(Ordering::Acquire));
}

#[tokio::test(start_paused = true)]
async fn idle_connection_reaches_the_official_limit_without_starting_an_execution() {
    let trace = Arc::new(AtomicFailureTrace::default());
    let execution = Arc::new(AtomicFailureExecution {
        client: authenticated_client("sk_ws_atomic"),
        trace: Arc::clone(&trace),
        response_headers: Vec::new(),
    });
    let app = api_router(execution).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind WebSocket test server");
    let address = listener.local_addr().expect("test server address");
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve WebSocket test router");
    });
    let mut request = format!("ws://{address}/v1/responses")
        .into_client_request()
        .expect("WebSocket request");
    request.headers_mut().insert(
        AUTHORIZATION,
        "Bearer sk_ws_atomic".parse().expect("authorization"),
    );
    let (mut socket, response) = connect_async(request).await.expect("upgrade WebSocket");
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);

    tokio::time::advance(Duration::from_secs(60 * 60)).await;
    tokio::task::yield_now().await;
    let message = tokio::time::timeout(Duration::from_secs(1), socket.next())
        .await
        .expect("connection limit response timeout")
        .expect("connection remains available for the limit error")
        .expect("valid WebSocket frame");
    let ClientMessage::Text(text) = message else {
        panic!("connection limit must be a text error event");
    };
    let value = serde_json::from_str::<Value>(&text).expect("connection limit JSON");

    assert_eq!(
        value,
        json!({
            "type": "error",
            "status": 400,
            "error": {
                "type": "invalid_request_error",
                "code": "websocket_connection_limit_reached",
                "message": "Responses websocket connection limit reached (60 minutes). Create a new websocket connection to continue."
            }
        })
    );
    assert_eq!(trace.starts.load(Ordering::Acquire), 0);

    socket.close(None).await.expect("close WebSocket");
    server.abort();
}

#[tokio::test]
async fn active_response_crossing_the_limit_writes_terminal_before_the_limit_error() {
    let trace = Arc::new(CrossingLimitTrace::default());
    let execution = Arc::new(CrossingLimitExecution {
        client: authenticated_client("sk_ws_crossing_limit"),
        trace: Arc::clone(&trace),
    });
    let app = api_router(execution).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind WebSocket test server");
    let address = listener.local_addr().expect("test server address");
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve WebSocket test router");
    });
    let mut request = format!("ws://{address}/v1/responses")
        .into_client_request()
        .expect("WebSocket request");
    request.headers_mut().insert(
        AUTHORIZATION,
        "Bearer sk_ws_crossing_limit"
            .parse()
            .expect("authorization"),
    );
    let (mut socket, response) = connect_async(request).await.expect("upgrade WebSocket");
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    socket
        .send(ClientMessage::Text(
            json!({
                "type": "response.create",
                "model": "model-a",
                "input": "hello"
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("send response.create");

    let mut initial_types = Vec::new();
    loop {
        tokio::task::yield_now().await;
        let message = tokio::time::timeout(Duration::from_secs(1), socket.next())
            .await
            .unwrap_or_else(|_| panic!("initial response timeout after {initial_types:?}"))
            .expect("WebSocket remains open")
            .expect("valid WebSocket frame");
        let ClientMessage::Text(text) = message else {
            panic!("unexpected initial WebSocket frame after {initial_types:?}: {message:?}");
        };
        let value = serde_json::from_str::<Value>(&text).expect("initial response JSON");
        let event_type = value.get("type").and_then(Value::as_str);
        initial_types.extend(event_type.map(str::to_owned));
        if event_type == Some("response.created") {
            break;
        }
    }

    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(60 * 60)).await;
    tokio::task::yield_now().await;
    tokio::time::resume();
    trace.release_terminal.notify_one();
    let mut event_types = Vec::new();
    loop {
        tokio::task::yield_now().await;
        let message = tokio::time::timeout(Duration::from_secs(1), socket.next())
            .await
            .expect("post-limit response timeout")
            .expect("WebSocket remains open through the limit error")
            .expect("valid WebSocket frame");
        let ClientMessage::Text(text) = message else {
            continue;
        };
        let value = serde_json::from_str::<Value>(&text).expect("post-limit response JSON");
        let Some(event_type) = value.get("type").and_then(Value::as_str) else {
            continue;
        };
        event_types.push(event_type.to_owned());
        if event_type == "error" {
            break;
        }
    }

    assert_eq!(event_types, ["response.completed", "error"]);
    assert!(trace.finalized.load(Ordering::Acquire));
    server.abort();
}

#[tokio::test]
async fn second_text_frame_during_an_active_response_keeps_the_single_response_contract() {
    let trace = Arc::new(CrossingLimitTrace::default());
    let execution = Arc::new(CrossingLimitExecution {
        client: authenticated_client("sk_ws_crossing_limit"),
        trace,
    });
    let app = api_router(execution).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind WebSocket test server");
    let address = listener.local_addr().expect("test server address");
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve WebSocket test router");
    });
    let mut request = format!("ws://{address}/v1/responses")
        .into_client_request()
        .expect("WebSocket request");
    request.headers_mut().insert(
        AUTHORIZATION,
        "Bearer sk_ws_crossing_limit"
            .parse()
            .expect("authorization"),
    );
    let (mut socket, _) = connect_async(request).await.expect("upgrade WebSocket");
    let response_create = ClientMessage::Text(
        json!({
            "type": "response.create",
            "model": "model-a",
            "input": "hello"
        })
        .to_string()
        .into(),
    );
    socket
        .send(response_create.clone())
        .await
        .expect("send first response.create");
    loop {
        let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("initial response timeout")
            .expect("WebSocket remains open")
            .expect("valid WebSocket frame");
        let ClientMessage::Text(text) = message else {
            continue;
        };
        let value = serde_json::from_str::<Value>(&text).expect("initial response JSON");
        if value.get("type").and_then(Value::as_str) == Some("response.created") {
            break;
        }
    }

    socket
        .send(response_create)
        .await
        .expect("send overlapping response.create");
    let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .expect("policy close timeout")
        .expect("WebSocket returns a policy close")
        .expect("valid WebSocket frame");
    let ClientMessage::Close(Some(frame)) = message else {
        panic!("overlapping response.create must close the connection");
    };

    assert_eq!(frame.code, CloseCode::Policy);
    server.abort();
}
