use std::{
    fmt,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
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
        CommitRequirement, CoordinatedEvent, EngineError,
        execution::{
            AuthenticatedClient, ClientAuthenticationError, ExecutionService, ExecutionSession,
            StartExecution, StartProviderExecution, StartedExecution,
        },
    },
    error::GatewayError,
    event::{GatewayEvent, ProtocolWireEvent, ProviderEvent, ProviderResponseHeader, ResponseMeta},
    lifecycle::CancellationToken,
    operation::Operation,
    routing::PublicModelId,
};
use serde_json::{Value, json};
use tokio::sync::mpsc::{
    UnboundedReceiver, UnboundedSender, error::TryRecvError, unbounded_channel,
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{
        Message as ClientMessage, client::IntoClientRequest, protocol::frame::coding::CloseCode,
    },
};

use super::{AtomicFailureExecution, AtomicFailureTrace};
use crate::openai::{api_router, authenticated_client};

#[derive(Default)]
struct CrossingLimitTrace {
    release_terminal: tokio::sync::Notify,
    starts: AtomicUsize,
    inputs: Mutex<Vec<Value>>,
    cancelled: AtomicBool,
    cancellation_observed: tokio::sync::Notify,
    finalized: AtomicBool,
}

struct CrossingLimitSession {
    trace: Arc<CrossingLimitTrace>,
    started_emitted: bool,
    terminal_emitted: bool,
    finalized: bool,
}

impl ExecutionSession for CrossingLimitSession {
    fn next_event(&mut self) -> BoxFuture<'_, Result<Option<CoordinatedEvent>, EngineError>> {
        Box::pin(async move {
            if !self.started_emitted {
                self.started_emitted = true;
                return Ok(Some(crossing_limit_started_batch()));
            }
            if !self.terminal_emitted {
                self.trace.release_terminal.notified().await;
                self.terminal_emitted = true;
                return Ok(Some(crossing_limit_terminal_batch()));
            }
            self.finalized = true;
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
        self.finalized
    }

    fn cancel(&self) {
        self.trace.cancelled.store(true, Ordering::Release);
        self.trace.cancellation_observed.notify_one();
    }

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
            self.trace.starts.fetch_add(1, Ordering::AcqRel);
            if let Operation::Generate(generate) = &request.operation {
                self.trace
                    .inputs
                    .lock()
                    .expect("input trace lock")
                    .push(generate.protocol_payload().body()["input"].clone());
            }
            Ok(StartedExecution {
                request_id: gateway_core::engine::ModelRequestId::new("req_ws_crossing_limit")
                    .expect("request id"),
                created_at: SystemTime::now(),
                stream: request.metadata.stream,
                session: Box::new(CrossingLimitSession {
                    trace: Arc::clone(&self.trace),
                    started_emitted: false,
                    terminal_emitted: false,
                    finalized: false,
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

use gateway_api::openai::responses::websocket::connection::{
    ConnectionConfig, ConnectionEvent, ConnectionWriteError, FramePhase, PumpExitReason,
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
    cancellation: CancellationToken,
}

fn test_connection(stall_writes: bool) -> PumpHarness {
    let (incoming_tx, incoming_rx) = unbounded_channel();
    let (written_tx, written_rx) = unbounded_channel();
    let dropped = Arc::new(AtomicBool::new(false));
    let cancellation = CancellationToken::new();
    let socket = TestSocket {
        incoming: incoming_rx,
        written: written_tx,
        stall_writes,
        dropped: Arc::clone(&dropped),
    };
    let connection = spawn_connection(
        socket,
        Arc::from("ws_test"),
        cancellation.clone(),
        ConnectionConfig::PRODUCTION,
    );
    PumpHarness {
        connection,
        incoming: incoming_tx,
        written: written_rx,
        dropped,
        cancellation,
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
async fn waiting_for_exit_should_preserve_queued_business_frames() {
    let PumpHarness {
        mut connection,
        incoming,
        ..
    } = test_connection(false);
    for payload in ["first", "second"] {
        incoming
            .send(Ok(Message::Text(payload.into())))
            .expect("queue text");
    }

    assert!(
        tokio::time::timeout(Duration::from_secs(1), connection.wait_for_exit())
            .await
            .is_err()
    );
    for expected in ["first", "second"] {
        assert!(
            matches!(connection.next_event().await, Some(ConnectionEvent::Text(payload)) if payload == expected)
        );
    }
}

#[tokio::test]
async fn client_close_should_take_priority_over_queued_business_frames() {
    let PumpHarness {
        mut connection,
        incoming,
        ..
    } = test_connection(false);
    incoming
        .send(Ok(Message::Text("queued".into())))
        .expect("queue text");
    incoming
        .send(Ok(Message::Close(None)))
        .expect("close socket");

    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), connection.wait_for_exit())
            .await
            .expect("observe close"),
        PumpExitReason::ClientClose
    );
    assert!(matches!(
        connection.next_event().await,
        Some(ConnectionEvent::Exited(PumpExitReason::ClientClose))
    ));
}

#[tokio::test]
async fn full_business_queue_should_not_block_lifecycle_shutdown() {
    let PumpHarness {
        mut connection,
        incoming,
        mut written,
        cancellation,
        ..
    } = test_connection(false);
    for _ in 0..32 {
        incoming
            .send(Ok(Message::Text("queued".into())))
            .expect("queue text");
    }
    incoming
        .send(Ok(Message::Ping(vec![1].into())))
        .expect("send Ping after queue fills");
    assert!(matches!(written.recv().await, Some(Message::Pong(_))));
    cancellation.cancel();

    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), connection.wait_for_exit())
            .await
            .expect("observe shutdown"),
        PumpExitReason::LifecycleShutdown
    );
}

#[tokio::test]
async fn inbound_overload_should_close_without_executing_queued_frames() {
    let PumpHarness {
        mut connection,
        incoming,
        ..
    } = test_connection(false);
    for _ in 0..33 {
        incoming
            .send(Ok(Message::Text("queued".into())))
            .expect("queue text");
    }

    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), connection.wait_for_exit())
            .await
            .expect("observe overload"),
        PumpExitReason::InboundOverload
    );
    assert!(matches!(
        connection.next_event().await,
        Some(ConnectionEvent::Exited(PumpExitReason::InboundOverload))
    ));
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
        ..
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
        ..
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
        fail_before_first_event: false,
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
    let text = loop {
        let message = tokio::time::timeout(Duration::from_secs(1), socket.next())
            .await
            .expect("connection limit response timeout")
            .expect("connection remains available for the limit error")
            .expect("valid WebSocket frame");
        match message {
            ClientMessage::Text(text) => break text,
            ClientMessage::Ping(_) => {}
            other => panic!("unexpected frame before connection limit: {other:?}"),
        }
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

    // 服务端已因生命周期到期关闭；不再向关闭中的连接写入。
    drop(socket);
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

    for _ in 0..32 {
        socket
            .send(ClientMessage::Text(
                json!({"type": "response.create", "model": "model-a", "input": "queued after limit"})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("fill request queue before expiry");
    }
    socket
        .send(ClientMessage::Ping(vec![1].into()))
        .await
        .expect("send Ping after queued request");
    let pong = tokio::time::timeout(Duration::from_secs(1), socket.next())
        .await
        .expect("Pong timeout")
        .expect("connection remains open")
        .expect("valid frame");
    assert!(matches!(pong, ClientMessage::Pong(_)));

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
    assert_eq!(trace.starts.load(Ordering::Acquire), 1);
    server.abort();
}

type TestClientSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

async fn start_active_response() -> (
    Arc<CrossingLimitTrace>,
    TestClientSocket,
    tokio::task::JoinHandle<()>,
) {
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
        .send(response_create)
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

    (trace, socket, server)
}

#[tokio::test]
async fn requests_received_during_an_active_response_should_execute_in_order() {
    let (trace, mut socket, server) = start_active_response().await;
    for input in ["second", "third"] {
        socket
            .send(ClientMessage::Text(
                json!({"type": "response.create", "model": "model-a", "input": input})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("send queued response.create");
    }
    socket
        .send(ClientMessage::Ping(vec![1, 2, 3].into()))
        .await
        .expect("send Ping after queued requests");
    let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .expect("Pong timeout while response is active")
        .expect("WebSocket remains open")
        .expect("valid WebSocket frame");
    assert!(matches!(message, ClientMessage::Pong(_)));
    assert_eq!(trace.starts.load(Ordering::Acquire), 1);

    for completed_count in 1..=3 {
        trace.release_terminal.notify_one();
        let expected_types = if completed_count < 3 {
            vec![
                "response.completed",
                "response.metadata",
                "response.created",
            ]
        } else {
            vec!["response.completed"]
        };
        let mut event_types = Vec::new();
        for _ in &expected_types {
            let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
                .await
                .expect("serial response timeout")
                .expect("WebSocket remains open")
                .expect("valid WebSocket frame");
            let ClientMessage::Text(text) = message else {
                panic!("unexpected frame during serial execution: {message:?}");
            };
            let event: Value = serde_json::from_str(&text).expect("response event JSON");
            event_types.push(event["type"].as_str().expect("event type").to_owned());
        }
        assert_eq!(event_types, expected_types);
        assert_eq!(
            trace.starts.load(Ordering::Acquire),
            (completed_count + 1).min(3)
        );
    }
    assert_eq!(
        *trace.inputs.lock().expect("input trace lock"),
        vec![json!("hello"), json!("second"), json!("third")]
    );
    assert!(!trace.cancelled.load(Ordering::Acquire));
    socket.close(None).await.expect("close WebSocket");
    server.abort();
}

#[tokio::test]
async fn queued_unknown_message_should_be_rejected_before_the_next_valid_request() {
    let (trace, mut socket, server) = start_active_response().await;
    for payload in [
        json!({"type": "response.future"}),
        json!({"type": "response.create", "model": "model-a", "input": "next"}),
    ] {
        socket
            .send(ClientMessage::Text(payload.to_string().into()))
            .await
            .expect("queue frame");
    }
    trace.release_terminal.notify_one();

    let mut event_types = Vec::new();
    for _ in 0..4 {
        let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("response timeout")
            .expect("connection remains open")
            .expect("valid frame");
        let ClientMessage::Text(text) = message else {
            panic!("unexpected frame: {message:?}");
        };
        let event: Value = serde_json::from_str(&text).expect("event JSON");
        event_types.push(event["type"].as_str().expect("event type").to_owned());
    }
    assert_eq!(
        event_types,
        [
            "response.completed",
            "error",
            "response.metadata",
            "response.created"
        ]
    );
    assert_eq!(trace.starts.load(Ordering::Acquire), 2);
    assert_eq!(
        *trace.inputs.lock().expect("input trace lock"),
        vec![json!("hello"), json!("next")]
    );
    socket.close(None).await.expect("close WebSocket");
    server.abort();
}

#[tokio::test]
async fn queued_binary_frame_should_be_rejected_after_the_active_response_finishes() {
    let (trace, mut socket, server) = start_active_response().await;
    socket
        .send(ClientMessage::Binary(b"{}".to_vec().into()))
        .await
        .expect("queue binary frame");
    trace.release_terminal.notify_one();

    let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .expect("terminal timeout")
        .expect("connection remains open")
        .expect("valid frame");
    let ClientMessage::Text(text) = message else {
        panic!("active response did not finish before binary rejection: {message:?}");
    };
    let event: Value = serde_json::from_str(&text).expect("event JSON");
    assert_eq!(event["type"], "response.completed");
    let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .expect("policy close timeout")
        .expect("connection closes")
        .expect("valid frame");
    assert!(
        matches!(message, ClientMessage::Close(Some(frame)) if frame.code == CloseCode::Policy)
    );
    assert_eq!(trace.starts.load(Ordering::Acquire), 1);
    server.abort();
}

#[tokio::test]
async fn requests_deferred_during_active_response_share_the_inbound_queue_limit() {
    let (trace, mut socket, server) = start_active_response().await;
    for _ in 0..33 {
        if socket
            .send(ClientMessage::Text(
                json!({"type":"response.create","model":"model-a","input":"queued"})
                    .to_string()
                    .into(),
            ))
            .await
            .is_err()
        {
            break;
        }
    }
    tokio::time::timeout(
        Duration::from_secs(5),
        trace.cancellation_observed.notified(),
    )
    .await
    .expect("queue overload cancels the active execution");
    assert_eq!(trace.starts.load(Ordering::Acquire), 1);
    assert!(trace.cancelled.load(Ordering::Acquire));
    server.abort();
}

#[tokio::test]
async fn client_close_should_cancel_active_execution_without_starting_queued_requests() {
    let (trace, mut socket, server) = start_active_response().await;
    socket
        .send(ClientMessage::Text(
            json!({"type": "response.create", "model": "model-a", "input": "queued"})
                .to_string()
                .into(),
        ))
        .await
        .expect("queue request");
    socket.close(None).await.expect("close WebSocket");

    tokio::time::timeout(
        Duration::from_secs(2),
        trace.cancellation_observed.notified(),
    )
    .await
    .expect("cancel active execution promptly");
    assert_eq!(trace.starts.load(Ordering::Acquire), 1);
    assert!(trace.cancelled.load(Ordering::Acquire));
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn idle_pump_sends_heartbeat_without_bursting_after_a_delay() {
    let PumpHarness {
        connection,
        incoming: _incoming,
        mut written,
        ..
    } = test_connection(false);
    tokio::task::yield_now().await;
    assert!(matches!(written.try_recv(), Err(TryRecvError::Empty)));

    tokio::time::advance(Duration::from_secs(25)).await;
    let first = written.recv().await.expect("first heartbeat");
    assert!(matches!(first, Message::Ping(_)));

    tokio::time::advance(Duration::from_secs(120)).await;
    let second = written.recv().await.expect("heartbeat after delay");
    assert!(matches!(&second, Message::Ping(_)));
    assert_ne!(first, second);
    tokio::task::yield_now().await;
    assert!(matches!(written.try_recv(), Err(TryRecvError::Empty)));
    drop(connection);
}

#[tokio::test(start_paused = true)]
async fn missing_pongs_do_not_cancel_the_connection_or_block_business_frames() {
    let PumpHarness {
        mut connection,
        incoming,
        mut written,
        ..
    } = test_connection(false);
    for _ in 0..13 {
        tokio::time::advance(Duration::from_secs(25)).await;
        assert!(matches!(written.recv().await, Some(Message::Ping(_))));
    }

    incoming
        .send(Ok(Message::Text("still connected".into())))
        .expect("send business frame after 325 seconds without Pong");
    assert!(matches!(
        connection.next_event().await,
        Some(ConnectionEvent::Text(text)) if text == "still connected"
    ));
    incoming.send(Ok(Message::Close(None))).expect("close peer");
    assert_eq!(
        connection.wait_for_exit().await,
        PumpExitReason::ClientClose
    );
    tokio::time::advance(Duration::from_secs(25)).await;
    assert!(written.recv().await.is_none());
}

#[tokio::test(start_paused = true)]
async fn stalled_heartbeat_can_be_cancelled_without_waiting_for_write_timeout() {
    let PumpHarness {
        mut connection,
        incoming: _incoming,
        written: _written,
        dropped,
        cancellation,
    } = test_connection(true);
    tokio::time::advance(Duration::from_secs(25)).await;
    tokio::task::yield_now().await;
    cancellation.cancel();
    let reason = tokio::time::timeout(Duration::from_secs(1), connection.wait_for_exit())
        .await
        .expect("cancel a blocked heartbeat promptly");
    assert_eq!(reason, PumpExitReason::LifecycleShutdown);
    assert!(dropped.load(Ordering::Acquire));
}

#[tokio::test(start_paused = true)]
async fn stalled_heartbeat_obeys_the_existing_write_timeout() {
    let PumpHarness {
        mut connection,
        incoming: _incoming,
        written: _written,
        dropped,
        ..
    } = test_connection(true);
    let started = tokio::time::Instant::now();
    assert_eq!(
        connection.wait_for_exit().await,
        PumpExitReason::WriteTimeout
    );
    assert_eq!(started.elapsed(), Duration::from_secs(25 + 300));
    assert!(dropped.load(Ordering::Acquire));
}

#[tokio::test]
async fn active_response_receives_heartbeats_during_two_hundred_seconds_of_silence() {
    let (trace, mut socket, server) = start_active_response().await;
    for _ in 0..8 {
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(25)).await;
        tokio::task::yield_now().await;
        tokio::time::resume();
        let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("heartbeat while the upstream response is pending")
            .expect("connection remains open")
            .expect("valid heartbeat frame");
        let ClientMessage::Ping(payload) = message else {
            panic!("expected heartbeat while business stream is silent: {message:?}");
        };
        // 与官方 WsStream 一样，在业务响应未完成时回复同 payload 的 Pong。
        socket
            .send(ClientMessage::Pong(payload))
            .await
            .expect("reply to heartbeat");
    }
    assert_eq!(trace.starts.load(Ordering::Acquire), 1);
    assert!(!trace.cancelled.load(Ordering::Acquire));

    trace.release_terminal.notify_one();
    let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .expect("terminal response after heartbeats")
        .expect("connection remains open")
        .expect("valid terminal frame");
    let ClientMessage::Text(text) = message else {
        panic!("expected completed response: {message:?}");
    };
    let value: Value = serde_json::from_str(&text).expect("terminal JSON");
    assert_eq!(value["type"], "response.completed");
    assert_eq!(trace.starts.load(Ordering::Acquire), 1);
    socket.close(None).await.expect("close WebSocket");
    server.abort();
}
