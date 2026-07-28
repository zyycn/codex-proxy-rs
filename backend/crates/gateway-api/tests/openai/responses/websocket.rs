use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, SystemTime};

use axum::{
    body::Body,
    http::{Request, StatusCode, header::AUTHORIZATION},
};
use futures::{SinkExt, StreamExt, future::BoxFuture};
use gateway_api::openai::responses::ResponseCreateFrameError;
use gateway_core::engine::execution::{
    AuthenticatedClient, ClientAuthenticationError, ExecutionService, ExecutionSession,
    StartExecution, StartedExecution,
};
use gateway_core::engine::{CommitRequirement, CoordinatedEvent, EngineError, UpstreamSendState};
use gateway_core::error::{GatewayError, ProviderError, ProviderErrorKind};
use gateway_core::event::{
    GatewayEvent, ProtocolWireEvent, ProviderEvent, ProviderResponseHeader, ResponseMeta,
};
use gateway_core::operation::Operation;
use gateway_core::routing::PublicModelId;
use serde_json::{Value, json};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as ClientMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tower::ServiceExt;

use super::decode_response_create;
use crate::openai::{api_router, authenticated_client, models::ModelsExecution};

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
    next_calls: AtomicUsize,
    committed: AtomicBool,
    finalized: AtomicBool,
}

struct AtomicFailureSession {
    trace: Arc<AtomicFailureTrace>,
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
        &[]
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
        Box::pin(async move {
            Ok(StartedExecution {
                request_id: gateway_core::engine::ModelRequestId::new("req_ws_atomic")
                    .expect("request id"),
                created_at: SystemTime::now(),
                stream: request.metadata.stream,
                session: Box::new(AtomicFailureSession {
                    trace: Arc::clone(&self.trace),
                }),
            })
        })
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
