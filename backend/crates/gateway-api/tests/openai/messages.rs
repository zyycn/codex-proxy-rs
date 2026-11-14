//! Messages 原生交付、错误格式、计数与公共生命周期的接口合同

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use bytes::Bytes;
use futures::{StreamExt as _, future::BoxFuture};
use gateway_core::{
    engine::ModelRequestId,
    engine::execution::{
        AuthenticatedClient, ClientAuthenticationError, ExecutionService, StartExecution,
        StartProviderExecution, StartedExecution,
    },
    error::GatewayError,
    event::{ProtocolWireEvent, ProviderEvent},
    operation::Operation,
    routing::PublicModelId,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, atomic::Ordering};
use tower::ServiceExt as _;

use super::{api_router, authenticated_client};
use crate::support::generation::{Finish, Session};

struct Execution {
    session: Mutex<Option<Session>>,
    captured: Mutex<Option<Operation>>,
}

impl ExecutionService for Execution {
    fn authenticate(&self, key: &str) -> Result<AuthenticatedClient, ClientAuthenticationError> {
        if key == "sk_messages_test" {
            Ok(authenticated_client(key))
        } else {
            Err(ClientAuthenticationError::InvalidKey)
        }
    }
    fn public_models(&self, _: &AuthenticatedClient) -> Vec<PublicModelId> {
        vec![]
    }
    fn contains_public_model(&self, _: &AuthenticatedClient, _: &PublicModelId) -> bool {
        true
    }
    fn start(
        &self,
        request: StartExecution,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async move {
            assert_eq!(request.metadata.protocol, "anthropic");
            assert!(request.metadata.endpoint.starts_with("/v1/messages"));
            *self.captured.lock().unwrap() = Some(request.operation);
            Ok(StartedExecution {
                request_id: ModelRequestId::new("req_messages").unwrap(),
                created_at: std::time::SystemTime::now(),
                stream: request.metadata.stream,
                session: Box::new(self.session.lock().unwrap().take().unwrap()),
            })
        })
    }
    fn start_provider_endpoint(
        &self,
        _: StartProviderExecution,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async { panic!("Messages uses model execution") })
    }
}

fn wire(value: Value) -> ProviderEvent {
    ProviderEvent::wire(
        ProtocolWireEvent::json(
            "anthropic",
            value.get("type").and_then(Value::as_str).map(str::to_owned),
            value,
        )
        .unwrap(),
    )
}

fn message() -> Value {
    json!({"id":"msg_native","type":"message","role":"assistant","model":"native-model","content":[{"type":"text","text":"你好"}],"stop_reason":"end_turn","usage":{"input_tokens":3,"output_tokens":2},"future_extension":{"preserved":true}})
}

fn stream_events() -> Vec<ProviderEvent> {
    vec![
        wire(
            json!({"type":"message_start","message":{"id":"msg_native","type":"message","role":"assistant","model":"native-model","content":[],"stop_reason":null,"usage":{"input_tokens":3,"output_tokens":0}}}),
        ),
        wire(
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        ),
        wire(
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"你好"}}),
        ),
        wire(json!({"type":"content_block_stop","index":0})),
        wire(
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":2}}),
        ),
        wire(json!({"type":"message_stop"})),
    ]
}

fn request(path: &str, stream: bool) -> Request<Body> {
    Request::post(path).header("x-api-key", "sk_messages_test").header("anthropic-version", "2023-06-01").header("content-type", "application/json")
        .body(Body::from(json!({"model":"model-a","max_tokens":0,"stream":stream,"messages":[{"role":"user","content":"你好"}],"future_extension":{"preserved":true}}).to_string())).unwrap()
}

async fn fixture(session: Session) -> (axum::Router, Arc<Execution>) {
    let execution = Arc::new(Execution {
        session: Mutex::new(Some(session)),
        captured: Mutex::default(),
    });
    (api_router(execution.clone()).await, execution)
}

#[tokio::test]
async fn native_json_preserves_extensions_and_uses_client_key_authentication() {
    let raw = Bytes::from(format!("  {}\n", message()));
    let session = Session::new(vec![ProviderEvent::wire(
        ProtocolWireEvent::raw_json("anthropic", raw.clone()).unwrap(),
    )]);
    let trace = session.trace.clone();
    let (router, execution) = fixture(session).await;
    let mut incoming = request("/v1/messages", false);
    incoming.headers_mut().insert(
        "x-claude-code-session-id",
        "claude-session".parse().unwrap(),
    );
    let response = router.oneshot(incoming).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(bytes, raw);
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body, message());
    let captured = execution.captured.lock().unwrap();
    let Operation::Generate(generation) = captured.as_ref().unwrap() else {
        panic!("generate")
    };
    assert_eq!(generation.protocol_payload().protocol(), "anthropic");
    assert!(
        generation.protocol_payload().context()["opaque_request_headers"]
            .as_array()
            .unwrap()
            .contains(&json!([
                "x-claude-code-session-id",
                STANDARD.encode("claude-session")
            ]))
    );
    assert_eq!(
        generation.protocol_payload().body()["future_extension"]["preserved"],
        true
    );
    assert_eq!(generation.max_output_tokens(), Some(0));
    assert_eq!(trace.commits.load(Ordering::SeqCst), 1);
    assert!(trace.finalized.load(Ordering::SeqCst));
}

#[tokio::test]
async fn native_stream_ends_at_message_stop_without_openai_done() {
    let session = Session::new(stream_events());
    let trace = session.trace.clone();
    let (router, _) = fixture(session).await;
    let response = router.oneshot(request("/v1/messages", true)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(text.contains("你好"));
    assert!(text.ends_with("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"));
    assert!(!text.contains("[DONE]"));
    assert_eq!(trace.commits.load(Ordering::SeqCst), 1);
    assert!(trace.finalized.load(Ordering::SeqCst));
}

#[tokio::test]
async fn buffered_client_aggregates_native_events_and_cumulative_usage() {
    let (router, _) = fixture(Session::new(stream_events())).await;
    let response = router
        .oneshot(request("/v1/messages", false))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["content"][0]["text"], "你好");
    assert_eq!(body["usage"], json!({"input_tokens":3,"output_tokens":2}));
    assert_eq!(body["stop_reason"], "end_turn");
}

#[tokio::test]
async fn count_tokens_uses_its_own_operation_without_generation_budget() {
    let (router, execution) = fixture(Session::new(vec![wire(json!({"input_tokens":9}))])).await;
    let request = Request::post("/v1/messages/count_tokens")
        .header("x-api-key", "sk_messages_test")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"model":"model-a","messages":[{"role":"user","content":"hello"}]}).to_string(),
        ))
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body, json!({"input_tokens":9}));
    assert!(matches!(
        execution.captured.lock().unwrap().as_ref(),
        Some(Operation::CountTokens(_))
    ));
}

#[tokio::test]
async fn authentication_errors_use_messages_envelope() {
    let (router, _) = fixture(Session::new(vec![])).await;
    let response = router
        .oneshot(Request::post("/v1/messages").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "authentication_error");
}

#[tokio::test]
async fn stream_failure_and_disconnect_finalize_the_same_execution() {
    let mut session = Session::new(vec![stream_events().remove(0)]);
    session.finish = Finish::Error;
    let trace = session.trace.clone();
    let (router, _) = fixture(session).await;
    let response = router.oneshot(request("/v1/messages", true)).await.unwrap();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(text.contains("event: error\n"));
    assert!(!text.contains("[DONE]"));
    assert!(trace.finalized.load(Ordering::SeqCst));

    let mut session = Session::new(vec![stream_events().remove(0)]);
    session.finish = Finish::Pending;
    let trace = session.trace.clone();
    let (router, _) = fixture(session).await;
    let response = router.oneshot(request("/v1/messages", true)).await.unwrap();
    let mut stream = response.into_body().into_data_stream();
    assert!(
        stream
            .next()
            .await
            .unwrap()
            .unwrap()
            .starts_with(b"event: message_start")
    );
    drop(stream);
    tokio::task::yield_now().await;
    assert!(trace.cancelled.load(Ordering::SeqCst));
    assert!(trace.finalized.load(Ordering::SeqCst));
}

#[tokio::test]
async fn sse_preserves_raw_frames() {
    let raw = Bytes::from_static(b"event: message_start\nid: frame-1\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_native\"},\"future\":true}\n\n");
    let event = ProtocolWireEvent::json_with_raw_sse_metadata(
        "anthropic",
        Some("message_start".to_owned()),
        json!({"type":"message_start","message":{"id":"msg_native"},"future":true}),
        raw.clone(),
        Some("frame-1".to_owned()),
        None,
    )
    .unwrap();
    let (router, _) = fixture(Session::new(vec![
        ProviderEvent::wire(event),
        wire(json!({"type":"message_stop"})),
    ]))
    .await;
    let response = router.oneshot(request("/v1/messages", true)).await.unwrap();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert!(bytes.starts_with(&raw));
}

#[tokio::test]
async fn streaming_client_can_receive_a_complete_native_json_response() {
    let event = ProviderEvent::wire(
        ProtocolWireEvent::raw_json("anthropic", message().to_string().into()).unwrap(),
    );
    let (router, _) = fixture(Session::new(vec![event])).await;
    let response = router.oneshot(request("/v1/messages", true)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(text.contains("event: message_start\n"));
    assert!(text.contains("future_extension"));
    assert!(text.contains("你好"));
    assert!(text.contains("event: message_stop\n"));
}

#[tokio::test]
async fn native_future_non_json_events_remain_opaque_but_missing_stop_is_an_error() {
    let raw = Bytes::from_static(b"event: future_extension\ndata: opaque future payload\n\n");
    let mut events = stream_events();
    events.insert(
        1,
        ProviderEvent::wire(ProtocolWireEvent::raw_sse("anthropic", raw.clone()).unwrap()),
    );
    let (router, _) = fixture(Session::new(events)).await;
    let response = router.oneshot(request("/v1/messages", true)).await.unwrap();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert!(bytes.windows(raw.len()).any(|window| window == raw));
    let (router, _) = fixture(Session::new(vec![stream_events().remove(0)])).await;
    let response = router.oneshot(request("/v1/messages", true)).await.unwrap();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert!(
        std::str::from_utf8(&bytes)
            .unwrap()
            .contains("event: error\n")
    );
}
