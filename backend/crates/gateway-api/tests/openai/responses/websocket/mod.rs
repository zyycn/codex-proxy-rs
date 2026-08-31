mod connection;
mod protocol;

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, SystemTime};

use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, Request, StatusCode, header::AUTHORIZATION},
};
use bytes::Bytes;
use futures::{SinkExt, StreamExt, future::BoxFuture};
use gateway_api::openai::responses::{
    DecodedResponsesRequest, OpenAiRequestHeaders, ResponseCreateFrameError,
    decode_response_create_with_context,
};
use gateway_core::engine::execution::{
    AuthenticatedClient, ClientAuthenticationError, ExecutionService, ExecutionSession,
    StartExecution, StartProviderExecution, StartedExecution,
};
use gateway_core::engine::{CommitRequirement, CoordinatedEvent, EngineError, UpstreamSendState};
use gateway_core::error::{GatewayError, ProviderError, ProviderErrorKind};
use gateway_core::event::{
    GatewayEvent, ProtocolWireEvent, ProviderEvent, ProviderResponseHeader, ResponseMeta,
};
use gateway_core::operation::Operation;
use gateway_core::routing::PublicModelId;
use gateway_protocol::openai::codex_responses_request_semantics;
use serde_json::{Value, json};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as ClientMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tower::ServiceExt;

use super::decode_response_create;
use crate::openai::{api_router, authenticated_client, models::ModelsExecution};

fn decode_response_create_with_turn_header(
    payload: Value,
    opening_turn_metadata: &'static str,
) -> DecodedResponsesRequest {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-codex-turn-metadata",
        HeaderValue::from_static(opening_turn_metadata),
    );
    decode_response_create_with_context(
        &payload.to_string(),
        &OpenAiRequestHeaders::from_headers(&headers),
    )
    .expect("response.create should decode")
}

#[test]
fn response_create_should_default_to_the_websocket_streaming_contract() {
    let decoded = decode_response_create(
        &json!({
            "type": "response.create",
            "model": "smart-code",
            "input": "hello",
            "store": false
        })
        .to_string(),
    )
    .expect("decode response.create");

    assert!(decoded.metadata().stream());
    assert!(!decoded.metadata().store());
    let Operation::Generate(request) = decoded.operation() else {
        panic!("Responses must map to Generate");
    };
    assert!(request.protocol_payload().body().get("stream").is_none());
}

#[test]
fn response_create_should_preserve_provider_options_as_opaque_wire_body() {
    let decoded = decode_response_create(
        &json!({
            "type": "response.create",
            "model": "smart-code",
            "input": "hello",
            "stream": true,
            "provider_options": {
                "version": "v1",
                "providers": {
                    "openai": {"schema_version": 1, "transport": "websocket"}
                }
            }
        })
        .to_string(),
    )
    .expect("decode opaque provider options");
    let Operation::Generate(request) = decoded.operation() else {
        panic!("Responses must map to Generate");
    };
    let payload = request.protocol_payload();

    assert_eq!(
        payload.body().get("provider_options"),
        Some(&json!({
            "version": "v1",
            "providers": {
                "openai": {"schema_version": 1, "transport": "websocket"}
            }
        }))
    );
    assert!(payload.context().get("provider_options").is_none());
}

#[test]
fn response_create_should_preserve_compaction_trigger_for_openai() {
    let decoded = decode_response_create(
        &json!({
            "type": "response.create",
            "model": "smart-code",
            "input": [
                {"type": "message", "role": "user", "content": "history"},
                {"type": "compaction_trigger"}
            ]
        })
        .to_string(),
    )
    .expect("decode OpenAI response.create");
    let Operation::Generate(request) = decoded.operation() else {
        panic!("OpenAI response.create must remain Generate");
    };

    assert_eq!(
        request
            .protocol_payload()
            .body()
            .get("input")
            .and_then(|input| input.pointer("/1/type")),
        Some(&json!("compaction_trigger"))
    );
}

#[test]
fn response_create_should_prefer_frame_turn_metadata_over_opening_headers() {
    let decoded = decode_response_create_with_turn_header(
        json!({
            "type": "response.create",
            "model": "smart-code",
            "input": "hello",
            "client_metadata": {
                "x-codex-turn-metadata": r#"{"request_kind":"turn"}"#
            }
        }),
        r#"{"request_kind":"compaction"}"#,
    );
    let Operation::Generate(request) = decoded.operation() else {
        panic!("Responses must map to Generate");
    };

    let semantics = codex_responses_request_semantics(
        request.protocol_payload().body(),
        request.protocol_payload().context(),
    );

    assert_eq!(
        (semantics.request_kind.as_deref(), semantics.compact),
        (Some("turn"), false)
    );
}

#[test]
fn response_create_should_accept_frame_compaction_after_a_turn_opening_header() {
    let decoded = decode_response_create_with_turn_header(
        json!({
            "type": "response.create",
            "model": "smart-code",
            "input": "hello",
            "client_metadata": {
                "x-codex-turn-metadata": r#"{"request_kind":"compaction"}"#
            }
        }),
        r#"{"request_kind":"turn"}"#,
    );
    let Operation::Generate(request) = decoded.operation() else {
        panic!("Responses must map to Generate");
    };

    let semantics = codex_responses_request_semantics(
        request.protocol_payload().body(),
        request.protocol_payload().context(),
    );

    assert_eq!(
        (semantics.request_kind.as_deref(), semantics.compact),
        (Some("compaction"), true)
    );
}

#[test]
fn response_create_should_fall_back_to_opening_turn_metadata_when_the_frame_omits_it() {
    let decoded = decode_response_create_with_turn_header(
        json!({
            "type": "response.create",
            "model": "smart-code",
            "input": "hello"
        }),
        r#"{"request_kind":"compaction"}"#,
    );
    let Operation::Generate(request) = decoded.operation() else {
        panic!("Responses must map to Generate");
    };

    let semantics = codex_responses_request_semantics(
        request.protocol_payload().body(),
        request.protocol_payload().context(),
    );

    assert_eq!(
        (semantics.request_kind.as_deref(), semantics.compact),
        (Some("compaction"), true)
    );
}

#[test]
fn response_create_should_reject_explicit_non_streaming_requests() {
    let error = decode_response_create(
        &json!({
            "type": "response.create",
            "model": "smart-code",
            "input": "hello",
            "stream": false
        })
        .to_string(),
    )
    .expect_err("WebSocket requests must stream");

    assert_eq!(error, ResponseCreateFrameError::StreamingRequired);
}

#[test]
fn response_create_should_reject_invalid_frame_shapes() {
    for (payload, expected) in [
        ("not-json", ResponseCreateFrameError::InvalidJson),
        ("[]", ResponseCreateFrameError::ExpectedObject),
        (
            r#"{"type":"future.message","model":"smart-code","input":"hello"}"#,
            ResponseCreateFrameError::UnsupportedType,
        ),
    ] {
        assert_eq!(
            decode_response_create(payload).expect_err("invalid frame"),
            expected
        );
    }
}

#[test]
fn response_create_should_reject_non_boolean_stream_without_disclosing_body_values() {
    let prompt = "private-websocket-prompt-marker";
    let opaque_stream_value = "private-websocket-option-marker";
    let error = decode_response_create(
        &json!({
            "type": "response.create",
            "model": "smart-code",
            "input": prompt,
            "stream": opaque_stream_value
        })
        .to_string(),
    )
    .expect_err("WebSocket response.create must explicitly enable streaming");
    let rendered = format!("{error:?}\n{error}");

    assert_eq!(error, ResponseCreateFrameError::StreamingRequired);
    assert!(!rendered.contains(prompt));
    assert!(!rendered.contains(opaque_stream_value));
}

#[derive(Default)]
struct AtomicFailureTrace {
    starts: AtomicUsize,
    next_calls: AtomicUsize,
    committed: AtomicBool,
    finalized: AtomicBool,
}

struct AtomicFailureSession {
    trace: Arc<AtomicFailureTrace>,
    response_headers: Vec<ProviderResponseHeader>,
}

impl ExecutionSession for AtomicFailureSession {
    fn next_event(&mut self) -> BoxFuture<'_, Result<Option<CoordinatedEvent>, EngineError>> {
        Box::pin(async move {
            match self.trace.next_calls.fetch_add(1, Ordering::AcqRel) {
                0 => Ok(Some(atomic_failure_batch())),
                1 => {
                    self.trace.finalized.store(true, Ordering::Release);
                    Err(EngineError::Provider(ProviderError::new(
                        ProviderErrorKind::RateLimited,
                        UpstreamSendState::Sent,
                    )))
                }
                _ => Ok(None),
            }
        })
    }

    fn collect_uncommitted(&mut self) -> BoxFuture<'_, Result<Vec<ProviderEvent>, EngineError>> {
        Box::pin(async { Err(EngineError::InvalidDeliveryState) })
    }

    fn response_headers(&self) -> &[ProviderResponseHeader] {
        &self.response_headers
    }

    fn commit_downstream(&mut self, _: Option<u16>) -> BoxFuture<'_, Result<(), EngineError>> {
        Box::pin(async move {
            self.trace.committed.store(true, Ordering::Release);
            Ok(())
        })
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

struct AtomicFailureExecution {
    client: AuthenticatedClient,
    trace: Arc<AtomicFailureTrace>,
    response_headers: Vec<ProviderResponseHeader>,
}

impl ExecutionService for AtomicFailureExecution {
    fn authenticate(
        &self,
        plaintext: &str,
    ) -> Result<AuthenticatedClient, ClientAuthenticationError> {
        (plaintext == "sk_ws_atomic")
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
        self.trace.starts.fetch_add(1, Ordering::AcqRel);
        Box::pin(async move {
            Ok(StartedExecution {
                request_id: gateway_core::engine::ModelRequestId::new("req_ws_atomic")
                    .expect("request id"),
                created_at: SystemTime::now(),
                stream: request.metadata.stream,
                session: Box::new(AtomicFailureSession {
                    trace: Arc::clone(&self.trace),
                    response_headers: self.response_headers.clone(),
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

fn atomic_failure_batch() -> CoordinatedEvent {
    let started = ProviderEvent::canonical_with_wire(
        vec![GatewayEvent::Started(ResponseMeta::new(
            "resp_ws_atomic",
            "model-a",
        ))],
        ProtocolWireEvent::json(
            "openai",
            Some("response.created".to_owned()),
            json!({
                "type": "response.created",
                "response": {
                    "id": "resp_ws_atomic",
                    "model": "model-a",
                    "status": "in_progress"
                }
            }),
        )
        .expect("created wire"),
    );
    let failed = ProviderEvent::wire(
        ProtocolWireEvent::json(
            "openai",
            Some("response.failed".to_owned()),
            json!({
                "type": "response.failed",
                "response": {
                    "id": "resp_ws_atomic",
                    "status": "failed",
                    "error": {
                        "code": "rate_limit_exceeded",
                        "message": "websocket atomic upstream marker"
                    }
                }
            }),
        )
        .expect("failed wire"),
    );
    CoordinatedEvent::try_batch(
        vec![started, failed],
        CommitRequirement::CommitBeforeDelivery,
    )
    .expect("atomic WebSocket batch")
}

#[tokio::test]
async fn websocket_atomic_upstream_failure_batch_should_be_forwarded_once() {
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

    let mut messages = Vec::<Value>::new();
    loop {
        let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("WebSocket response timeout")
            .expect("WebSocket remains open")
            .expect("valid WebSocket frame");
        let ClientMessage::Text(text) = message else {
            continue;
        };
        let value: Value = serde_json::from_str(&text).expect("JSON WebSocket event");
        let failed = value.get("type").and_then(Value::as_str) == Some("response.failed");
        messages.push(value);
        if failed {
            break;
        }
    }

    assert_eq!(
        messages
            .iter()
            .filter(|message| message.get("type").and_then(Value::as_str) == Some("response.failed"))
            .count(),
        1
    );
    let failed = messages
        .iter()
        .find(|message| message.get("type").and_then(Value::as_str) == Some("response.failed"))
        .expect("upstream failed event");
    assert_eq!(
        failed
            .pointer("/response/error/message")
            .and_then(Value::as_str),
        Some("websocket atomic upstream marker")
    );
    assert!(trace.committed.load(Ordering::Acquire));
    assert_eq!(trace.next_calls.load(Ordering::Acquire), 2);
    assert!(trace.finalized.load(Ordering::Acquire));

    socket.close(None).await.expect("close WebSocket");
    server.abort();
}

#[tokio::test]
async fn websocket_response_metadata_should_preserve_ordinary_headers_without_blocking_events() {
    let trace = Arc::new(AtomicFailureTrace::default());
    let execution = Arc::new(AtomicFailureExecution {
        client: authenticated_client("sk_ws_atomic"),
        trace,
        response_headers: vec![
            ProviderResponseHeader::new("x-future-header", Bytes::from_static(b"future-value")),
            ProviderResponseHeader::new("x-future-multi", Bytes::from_static(b"first")),
            ProviderResponseHeader::new("x-future-multi", Bytes::from_static(b"second")),
            ProviderResponseHeader::new(
                "x-codex-turn-state",
                Bytes::from_static(b"turn-state-from-upstream"),
            ),
            ProviderResponseHeader::new("x-future-bytes", Bytes::from_static(b"\xffopaque")),
            ProviderResponseHeader::new("bad\0name", Bytes::from_static(b"unrepresentable")),
            ProviderResponseHeader::new(
                "authorization",
                Bytes::from_static(b"should-not-cross-boundary"),
            ),
            ProviderResponseHeader::new("connection", Bytes::from_static(b"x-private-hop")),
            ProviderResponseHeader::new("x-private-hop", Bytes::from_static(b"private-hop")),
            ProviderResponseHeader::new("content-type", Bytes::from_static(b"application/private")),
            ProviderResponseHeader::new("x-request-id", Bytes::from_static(b"req_upstream")),
        ],
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

    let mut metadata = None;
    let mut saw_failed = false;
    while metadata.is_none() || !saw_failed {
        let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("WebSocket response timeout")
            .expect("WebSocket remains open")
            .expect("valid WebSocket frame");
        let ClientMessage::Text(text) = message else {
            continue;
        };
        let value: Value = serde_json::from_str(&text).expect("JSON WebSocket event");
        match value.get("type").and_then(Value::as_str) {
            Some("response.metadata") => metadata = Some(value),
            Some("response.failed") => saw_failed = true,
            _ => {}
        }
    }

    let headers = metadata
        .as_ref()
        .and_then(|value| value.get("headers"))
        .and_then(Value::as_object)
        .expect("response metadata headers");
    assert_eq!(headers.get("x-future-header"), Some(&json!("future-value")));
    // 官方 metadata 是 string map，同名多值只能保持既有的后值覆盖语义。
    assert_eq!(headers.get("x-future-multi"), Some(&json!("second")));
    assert_eq!(headers.get("x-request-id"), Some(&json!("req_upstream")));
    assert_eq!(
        headers.get("x-codex-turn-state"),
        Some(&json!("turn-state-from-upstream"))
    );
    for omitted in [
        "x-future-bytes",
        "bad\0name",
        "authorization",
        "connection",
        "x-private-hop",
        "content-type",
    ] {
        assert!(
            !headers.contains_key(omitted),
            "unexpected metadata header: {omitted:?}"
        );
    }
    assert!(saw_failed);

    socket.close(None).await.expect("close WebSocket");
    server.abort();
}

#[tokio::test]
async fn websocket_response_create_should_not_have_a_private_16_mib_frame_limit() {
    let trace = Arc::new(AtomicFailureTrace::default());
    let execution = Arc::new(AtomicFailureExecution {
        client: authenticated_client("sk_ws_atomic"),
        trace,
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
    let payload = json!({
        "type": "response.create",
        "model": "model-a",
        "input": "x".repeat(16 * 1024 * 1024 + 1),
    })
    .to_string();
    assert!(payload.len() > 16 * 1024 * 1024);

    socket
        .send(ClientMessage::Text(payload.into()))
        .await
        .expect("send response.create above the former frame limit");

    let mut saw_upstream_event = false;
    for _ in 0..3 {
        let message = tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await
            .expect("WebSocket response timeout")
            .expect("WebSocket remains open")
            .expect("valid WebSocket frame");
        let ClientMessage::Text(text) = message else {
            continue;
        };
        let value: Value = serde_json::from_str(&text).expect("JSON WebSocket event");
        if matches!(
            value.get("type").and_then(Value::as_str),
            Some("response.created" | "response.failed")
        ) {
            saw_upstream_event = true;
            break;
        }
    }
    assert!(saw_upstream_event);

    socket.close(None).await.expect("close WebSocket");
    server.abort();
}

const TEST_WEBSOCKET_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAA==";

fn upgrade_request(authorization: &str) -> Request<Body> {
    Request::get("/v1/responses")
        .header(AUTHORIZATION, authorization)
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", TEST_WEBSOCKET_KEY)
        .body(Body::empty())
        .expect("build WebSocket upgrade request")
}

#[tokio::test]
async fn get_responses_should_route_to_the_websocket_upgrade_boundary() {
    let response = api_router(ModelsExecution::new())
        .await
        .oneshot(upgrade_request("Bearer sk_models_test"))
        .await
        .expect("route WebSocket upgrade request");

    // oneshot 请求不携带升级状态;426 证明 GET /v1/responses 进入的是
    // WebSocketUpgrade 边界而不是普通 HTTP handler。
    assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED);
}

#[tokio::test]
async fn websocket_upgradability_should_be_checked_before_authentication() {
    let response = api_router(ModelsExecution::new())
        .await
        .oneshot(upgrade_request("Bearer sk_invalid"))
        .await
        .expect("route unauthenticated upgrade request");

    // 升级能力在 extractor 阶段先于 handler 内的 API Key 认证被校验,
    // 无效凭据得到的仍是升级失败而不是 401。
    assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED);
}

#[tokio::test]
async fn websocket_upgrade_should_reject_malformed_handshakes() {
    let missing_connection = Request::get("/v1/responses")
        .header(AUTHORIZATION, "Bearer sk_models_test")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", TEST_WEBSOCKET_KEY)
        .body(Body::empty())
        .expect("build request without connection header");
    let unsupported_version = Request::get("/v1/responses")
        .header(AUTHORIZATION, "Bearer sk_models_test")
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "12")
        .header("sec-websocket-key", TEST_WEBSOCKET_KEY)
        .body(Body::empty())
        .expect("build request with unsupported version");
    let missing_key = Request::get("/v1/responses")
        .header(AUTHORIZATION, "Bearer sk_models_test")
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .body(Body::empty())
        .expect("build request without websocket key");

    for request in [missing_connection, unsupported_version, missing_key] {
        let response = api_router(ModelsExecution::new())
            .await
            .oneshot(request)
            .await
            .expect("route malformed WebSocket handshake");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
