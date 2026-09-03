use std::collections::VecDeque;
use std::net::{IpAddr, SocketAddr};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use axum::{
    body::{Body, to_bytes},
    extract::connect_info::ConnectInfo,
    http::{
        HeaderMap, HeaderValue, Request, StatusCode,
        header::{AUTHORIZATION, CONTENT_TYPE},
    },
};
use bytes::Bytes;
use futures::future::{BoxFuture, pending};
use gateway_core::engine::execution::{
    AuthenticatedClient, ClientAuthenticationError, ExecutionService, ExecutionSession,
    StartExecution, StartProviderExecution, StartedExecution,
};
use gateway_core::engine::{CommitRequirement, CoordinatedEvent, EngineError};
use gateway_core::error::{
    ClientVisibleUpstreamError, ClientVisibleUpstreamResponse, GatewayError, GatewayErrorKind,
    ProviderError, ProviderErrorKind,
};
use gateway_core::event::{
    ContentItem, ContentKind, GatewayEvent, ProtocolWireEvent, ProviderEvent,
    ProviderResponseHeader, ResponseMeta, TextDelta,
};
use gateway_core::operation::{Operation, OperationKind};
use gateway_core::routing::PublicModelId;
use gateway_core::upstream::UpstreamSendState;
use serde_json::{Value, json};

use gateway_api::openai::responses::{collect_execution_response, stream_execution_response};
use tower::ServiceExt;

use crate::openai::{api_router, authenticated_client_for_provider};

#[derive(Default)]
struct Trace {
    events: Mutex<Vec<&'static str>>,
    client_statuses: Mutex<Vec<u16>>,
    cancelled: AtomicBool,
}

impl Trace {
    fn push(&self, event: &'static str) {
        self.events.lock().expect("trace lock").push(event);
    }

    fn snapshot(&self) -> Vec<&'static str> {
        self.events.lock().expect("trace lock").clone()
    }

    fn record_client_status(&self, status: u16) {
        self.client_statuses
            .lock()
            .expect("client status lock")
            .push(status);
    }

    fn client_statuses(&self) -> Vec<u16> {
        self.client_statuses
            .lock()
            .expect("client status lock")
            .clone()
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

enum NextStep {
    Event(CoordinatedEvent),
    Error(EngineError),
    FinalizeCancelled,
    FinalizeSuccess,
    End,
}

struct FakeSession {
    trace: Arc<Trace>,
    next: VecDeque<NextStep>,
    collected: Option<Vec<ProviderEvent>>,
    collect_error: Option<EngineError>,
    collect_pending: bool,
    finalize_on_commit: bool,
    fail_commit: bool,
    finalized: bool,
    response_headers: Vec<ProviderResponseHeader>,
}

#[derive(Clone)]
struct ContextCaptureExecution {
    observed: Arc<Mutex<Option<CapturedClientContext>>>,
    client: AuthenticatedClient,
}

#[derive(Debug, Clone, PartialEq)]
struct CapturedClientContext {
    public_model: String,
    wire_model: Option<String>,
    client_ip: Option<IpAddr>,
    user_agent: Option<String>,
    endpoint: String,
    operation_kind: OperationKind,
    input: Option<Value>,
    client_metadata: Option<Value>,
    protocol_context: Option<Value>,
    prompt_cache_key: Option<String>,
    previous_response_id: Option<String>,
}

impl ExecutionService for ContextCaptureExecution {
    fn authenticate(
        &self,
        plaintext: &str,
    ) -> Result<AuthenticatedClient, ClientAuthenticationError> {
        if plaintext == "sk_context_test" {
            Ok(self.client.clone())
        } else {
            Err(ClientAuthenticationError::InvalidKey)
        }
    }

    fn public_models(&self, _: &AuthenticatedClient) -> Vec<PublicModelId> {
        ["model-a", "model-b"]
            .into_iter()
            .map(|model| PublicModelId::new(model).expect("catalog model"))
            .collect()
    }

    fn contains_public_model(&self, _: &AuthenticatedClient, model: &PublicModelId) -> bool {
        matches!(model.as_str(), "model-a" | "model-b")
    }

    fn start(
        &self,
        request: StartExecution,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async move {
            let public_model = request.public_model.as_str().to_owned();
            let operation_kind = request.operation.kind();
            let generation = match &request.operation {
                Operation::Generate(generation) => Some(generation),
                _ => None,
            };
            let wire_model = generation
                .and_then(|generation| generation.protocol_payload().body().get("model"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let (client_metadata, protocol_context, prompt_cache_key, input) =
                generation.map_or((None, None, None, None), |generation| {
                    let payload = generation.protocol_payload();
                    (
                        payload.body().get("client_metadata").cloned(),
                        (!payload.context().is_empty())
                            .then(|| Value::Object(payload.context().clone())),
                        generation.prompt_cache_key().map(ToOwned::to_owned),
                        payload.body().get("input").cloned(),
                    )
                });
            let previous_response_id = request
                .metadata
                .previous_response_id
                .as_ref()
                .map(|value| value.as_str().to_owned());
            *self.observed.lock().expect("context capture lock") = Some(CapturedClientContext {
                public_model,
                wire_model,
                client_ip: request.metadata.client_ip,
                user_agent: request.metadata.user_agent,
                endpoint: request.metadata.endpoint,
                operation_kind,
                input,
                client_metadata,
                protocol_context,
                prompt_cache_key,
                previous_response_id,
            });
            Err(GatewayError::new(
                GatewayErrorKind::Internal,
                "context capture completed",
            ))
        })
    }

    fn start_provider_endpoint(
        &self,
        _: StartProviderExecution,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async {
            Err(GatewayError::new(
                GatewayErrorKind::Internal,
                "responses context test must not start a provider endpoint",
            ))
        })
    }
}

impl FakeSession {
    fn streaming(trace: Arc<Trace>, next: Vec<NextStep>) -> Self {
        Self {
            trace,
            next: VecDeque::from(next),
            collected: None,
            collect_error: None,
            collect_pending: false,
            finalize_on_commit: false,
            fail_commit: false,
            finalized: false,
            response_headers: Vec::new(),
        }
    }

    fn buffered(trace: Arc<Trace>, events: Vec<GatewayEvent>) -> Self {
        Self::buffered_provider(
            trace,
            events.into_iter().map(provider_event_for_fact).collect(),
        )
    }

    fn buffered_provider(trace: Arc<Trace>, events: Vec<ProviderEvent>) -> Self {
        Self {
            trace,
            next: VecDeque::from([NextStep::FinalizeCancelled]),
            collected: Some(events),
            collect_error: None,
            collect_pending: false,
            finalize_on_commit: true,
            fail_commit: false,
            finalized: false,
            response_headers: Vec::new(),
        }
    }

    fn pending_buffered(trace: Arc<Trace>) -> Self {
        Self {
            trace,
            next: VecDeque::from([NextStep::FinalizeCancelled]),
            collected: None,
            collect_error: None,
            collect_pending: true,
            finalize_on_commit: true,
            fail_commit: false,
            finalized: false,
            response_headers: Vec::new(),
        }
    }

    fn with_commit_failure(mut self) -> Self {
        self.fail_commit = true;
        self.finalize_on_commit = false;
        self
    }

    fn with_collect_error(mut self, error: EngineError) -> Self {
        self.collect_error = Some(error);
        self
    }

    fn with_response_headers(mut self, response_headers: Vec<ProviderResponseHeader>) -> Self {
        self.response_headers = response_headers;
        self
    }
}

impl ExecutionSession for FakeSession {
    fn next_event(&mut self) -> BoxFuture<'_, Result<Option<CoordinatedEvent>, EngineError>> {
        Box::pin(async move {
            match self.next.pop_front().unwrap_or(NextStep::End) {
                NextStep::Event(event) => {
                    self.trace.push("next_event");
                    Ok(Some(event))
                }
                NextStep::Error(error) => {
                    self.trace.push("next_error");
                    self.finalized = true;
                    Err(error)
                }
                NextStep::FinalizeCancelled => {
                    self.trace.push("cancel_finalize");
                    self.finalized = true;
                    Err(EngineError::Cancelled)
                }
                NextStep::FinalizeSuccess => {
                    self.trace.push("next_end");
                    self.finalized = true;
                    Ok(None)
                }
                NextStep::End => {
                    self.trace.push("next_end");
                    Ok(None)
                }
            }
        })
    }

    fn collect_uncommitted(&mut self) -> BoxFuture<'_, Result<Vec<ProviderEvent>, EngineError>> {
        Box::pin(async move {
            self.trace.push("collect");
            if self.collect_pending {
                pending::<()>().await;
            }
            if let Some(error) = self.collect_error.take() {
                self.finalized = true;
                return Err(error);
            }
            Ok(self.collected.take().unwrap_or_default())
        })
    }

    fn response_headers(&self) -> &[ProviderResponseHeader] {
        &self.response_headers
    }

    fn commit_downstream(
        &mut self,
        client_status_code: Option<u16>,
    ) -> BoxFuture<'_, Result<(), EngineError>> {
        Box::pin(async move {
            self.trace.push("commit");
            if self.fail_commit {
                return Err(EngineError::ProviderMetadataMismatch);
            }
            if let Some(status) = client_status_code {
                self.trace.record_client_status(status);
            }
            self.finalized = self.finalize_on_commit;
            Ok(())
        })
    }

    fn record_client_status(
        &mut self,
        client_status_code: u16,
    ) -> BoxFuture<'_, Result<(), EngineError>> {
        Box::pin(async move {
            self.trace.record_client_status(client_status_code);
            Ok(())
        })
    }

    fn is_finalized(&self) -> bool {
        self.finalized
    }

    fn cancel(&self) {
        self.trace.cancel();
    }

    fn detach_finalize(mut self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            if !self.finalized {
                let _ = self.next_event().await;
            }
        })
    }
}

fn started() -> GatewayEvent {
    GatewayEvent::Started(ResponseMeta::new("resp_test", "public-model"))
}

fn completed() -> GatewayEvent {
    GatewayEvent::Completed(ResponseMeta::new("resp_test", "public-model"))
}

fn delivery(event: GatewayEvent, commit_requirement: CommitRequirement) -> CoordinatedEvent {
    delivery_provider(provider_event_for_fact(event), commit_requirement)
}

fn delivery_provider(
    event: ProviderEvent,
    commit_requirement: CommitRequirement,
) -> CoordinatedEvent {
    CoordinatedEvent::single(event, commit_requirement)
}

fn provider_event_for_fact(event: GatewayEvent) -> ProviderEvent {
    let (GatewayEvent::Started(meta) | GatewayEvent::Completed(meta)) = &event else {
        return ProviderEvent::canonical(event);
    };
    let event_type = if matches!(&event, GatewayEvent::Started(_)) {
        "response.created"
    } else {
        "response.completed"
    };
    let response = json!({
        "id": meta.response_id(),
        "model": meta.model(),
        "status": if event_type == "response.created" { "in_progress" } else { "completed" },
        "output": []
    });
    ProviderEvent::canonical_with_wire(
        vec![event],
        ProtocolWireEvent::json(
            "openai",
            Some(event_type.to_owned()),
            json!({"type": event_type, "response": response}),
        )
        .expect("fixture OpenAI wire event"),
    )
}

fn mismatched_terminal_event() -> ProviderEvent {
    let meta = ResponseMeta::new("resp_other", "public-model");
    ProviderEvent::canonical_with_wire(
        vec![GatewayEvent::Completed(meta)],
        ProtocolWireEvent::json(
            "openai",
            Some("response.completed".to_owned()),
            json!({
                "type": "response.completed",
                "response": {"id": "resp_other", "status": "completed", "output": []}
            }),
        )
        .expect("fixture mismatched terminal"),
    )
}

async fn captured_client_context(
    headers: HeaderMap,
    peer_address: SocketAddr,
) -> CapturedClientContext {
    captured_http_request(
        "openai",
        json!({"model": "smart-code", "input": "hello"}),
        headers,
        Some(peer_address),
    )
    .await
}

async fn captured_http_request(
    provider_name: &str,
    body: Value,
    mut headers: HeaderMap,
    peer_address: Option<SocketAddr>,
) -> CapturedClientContext {
    headers.insert(
        AUTHORIZATION,
        "Bearer sk_context_test".parse().expect("authorization"),
    );
    let observed = Arc::new(Mutex::new(None));
    let execution = Arc::new(ContextCaptureExecution {
        observed: Arc::clone(&observed),
        client: authenticated_client_for_provider("sk_context_test", provider_name),
    });
    let mut request = Request::post("/v1/responses")
        .body(Body::from(body.to_string()))
        .expect("context request");
    *request.headers_mut() = headers;
    if let Some(peer_address) = peer_address {
        request.extensions_mut().insert(ConnectInfo(peer_address));
    }
    let response = api_router(execution)
        .await
        .oneshot(request)
        .await
        .expect("context response");
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    observed
        .lock()
        .expect("context capture lock")
        .clone()
        .expect("captured request context")
}

async fn captured_http_compaction(provider_name: &str) -> CapturedClientContext {
    captured_http_request(
        provider_name,
        json!({
            "model": "smart-code",
            "input": [
                {"type": "message", "role": "user", "content": "history"},
                {"type": "compaction_trigger"}
            ],
            "stream": true
        }),
        HeaderMap::new(),
        None,
    )
    .await
}

#[tokio::test]
async fn openai_http_should_preserve_compaction_trigger_as_generate() {
    let captured = captured_http_compaction("openai").await;

    assert_eq!(
        (captured.operation_kind, captured.input),
        (
            OperationKind::Generate,
            Some(json!([
                {"type": "message", "role": "user", "content": "history"},
                {"type": "compaction_trigger"}
            ])),
        )
    );
}

#[tokio::test]
async fn xai_http_should_leave_compaction_trigger_for_the_xai_provider_adapter() {
    let captured = captured_http_compaction("xai").await;

    assert_eq!(
        (captured.operation_kind, captured.input),
        (
            OperationKind::Generate,
            Some(json!([
                {"type": "message", "role": "user", "content": "history"},
                {"type": "compaction_trigger"}
            ])),
        )
    );
}

#[tokio::test]
async fn request_context_should_resolve_forwarded_precedence_and_peer_fallback() {
    let peer = "192.0.2.10:443".parse().expect("peer address");

    let mut headers = HeaderMap::new();
    headers.insert("cf-connecting-ip", "198.51.100.1".parse().expect("CF IP"));
    headers.insert("x-real-ip", "198.51.100.2".parse().expect("real IP"));
    headers.insert(
        "x-forwarded-for",
        "10.0.0.2, 203.0.113.3".parse().expect("forwarded IPs"),
    );
    headers.insert("user-agent", " Codex-CLI/1.0 ".parse().expect("user agent"));
    assert_eq!(
        captured_client_context(headers, peer).await,
        CapturedClientContext {
            public_model: "smart-code".to_owned(),
            wire_model: Some("smart-code".to_owned()),
            client_ip: Some("198.51.100.1".parse().expect("expected IP")),
            user_agent: Some("Codex-CLI/1.0".to_owned()),
            endpoint: "/v1/responses".to_owned(),
            operation_kind: OperationKind::Generate,
            input: Some(json!("hello")),
            client_metadata: None,
            // 客户端 User-Agent 仅用于本地展示，不再透传给上游指纹上下文。
            protocol_context: None,
            prompt_cache_key: None,
            previous_response_id: None,
        }
    );

    let mut headers = HeaderMap::new();
    headers.insert(
        "x-forwarded-for",
        "10.0.0.2, 203.0.113.3".parse().expect("forwarded IPs"),
    );
    assert_eq!(
        captured_client_context(headers, peer).await.client_ip,
        Some("203.0.113.3".parse().expect("expected IP"))
    );

    assert_eq!(
        captured_client_context(HeaderMap::new(), peer)
            .await
            .client_ip,
        Some("192.0.2.10".parse().expect("expected peer IP"))
    );
}

#[tokio::test]
async fn stale_model_catalog_should_not_block_a_new_model_request() {
    let model = "  gpt-future-codex  ";
    let captured = captured_http_request(
        "openai",
        json!({"model": model, "input": "hello"}),
        HeaderMap::new(),
        None,
    )
    .await;

    assert_eq!(captured.public_model, model);
    assert_eq!(captured.wire_model.as_deref(), Some(model));
}

#[tokio::test]
async fn http_request_should_pass_opaque_previous_response_id_to_core() {
    for response_id in [
        format!("resp_{}", "x".repeat(257)),
        "resp_control\0opaque".to_owned(),
        String::new(),
    ] {
        let captured = captured_http_request(
            "openai",
            json!({
                "model": "smart-code",
                "input": "continue",
                "previous_response_id": response_id.clone()
            }),
            HeaderMap::new(),
            None,
        )
        .await;

        assert_eq!(
            captured.previous_response_id.as_deref(),
            Some(response_id.as_str())
        );
    }
}

#[tokio::test]
async fn http_request_should_forward_codex_headers_without_projecting_xai_headers() {
    let peer = "192.0.2.10:443".parse().expect("peer address");
    let mut headers = HeaderMap::new();
    headers.insert("x-codex-turn-state", "turn-state".parse().expect("header"));
    headers.insert("conversation-id", "conversation-1".parse().expect("header"));
    headers.insert("session-id", "session-1".parse().expect("header"));
    headers.insert("x-grok-turn-idx", "7".parse().expect("header"));
    headers.insert(
        "x-openai-subagent",
        "future_codex_mode".parse().expect("header"),
    );
    headers.insert(
        "x-openai-internal-codex-responses-lite",
        "true".parse().expect("header"),
    );

    let captured = captured_client_context(headers, peer).await;
    let context = captured
        .protocol_context
        .as_ref()
        .and_then(Value::as_object)
        .expect("OpenAI protocol context");
    assert_eq!(context.get("turn_state"), Some(&json!("turn-state")));
    assert_eq!(
        context.get("conversation_id"),
        Some(&json!("conversation-1"))
    );
    assert_eq!(context.get("session_id"), Some(&json!("session-1")));
    assert_eq!(context.get("responses_lite"), Some(&json!("true")));
    assert!(captured.prompt_cache_key.is_none());
    assert!(!context.contains_key("authorization"));
    assert_eq!(
        captured
            .client_metadata
            .as_ref()
            .and_then(|metadata| metadata.get("x-openai-subagent")),
        Some(&json!("future_codex_mode"))
    );
}

#[tokio::test]
async fn subagent_header_should_not_replace_non_object_client_metadata() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-openai-subagent",
        "future_codex_mode".parse().expect("header"),
    );

    let captured = captured_http_request(
        "openai",
        json!({
            "model": "smart-code",
            "input": "preserve metadata",
            "client_metadata": "opaque-client-value"
        }),
        headers,
        None,
    )
    .await;

    assert_eq!(captured.client_metadata, Some(json!("opaque-client-value")));
}

#[tokio::test]
async fn xai_private_headers_should_not_enter_openai_request_facts() {
    let peer = "192.0.2.10:443".parse().expect("peer address");
    let mut headers = HeaderMap::new();
    headers.insert("x-grok-turn-idx", "7".parse().expect("header"));
    headers.insert("x-grok-conv-id", "private-session".parse().expect("header"));

    let captured = captured_client_context(headers, peer).await;

    assert!(captured.protocol_context.is_none());
    assert!(captured.prompt_cache_key.is_none());
}

#[tokio::test]
async fn streaming_encodes_first_frame_before_commit_and_http_delivery() {
    let trace = Arc::new(Trace::default());
    let session = FakeSession::streaming(
        Arc::clone(&trace),
        vec![NextStep::Event(delivery(
            started(),
            CommitRequirement::CommitBeforeDelivery,
        ))],
    );

    let response = stream_execution_response(Box::new(session), None).await;

    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert_eq!(trace.client_statuses(), vec![200]);
    assert_eq!(trace.snapshot(), vec!["next_event", "commit"]);
    assert!(!trace.is_cancelled());
    std::mem::forget(response);
}

#[tokio::test]
async fn streaming_response_should_append_ordinary_headers_and_skip_unrepresentable_or_sensitive_ones()
 {
    let trace = Arc::new(Trace::default());
    let headers = vec![
        ProviderResponseHeader::new("x-models-etag", Bytes::from_static(b"models-v2")),
        ProviderResponseHeader::new(
            "x-codex-turn-state",
            Bytes::from_static(b"turn-state-from-upstream"),
        ),
        ProviderResponseHeader::new("x-future-multi", Bytes::from_static(b"first")),
        ProviderResponseHeader::new("x-future-multi", Bytes::from_static(b"second")),
        ProviderResponseHeader::new("x-future-bytes", Bytes::from_static(b"\xffopaque")),
        ProviderResponseHeader::new(
            "authorization",
            Bytes::from_static(b"should-not-cross-boundary"),
        ),
        ProviderResponseHeader::new("connection", Bytes::from_static(b"x-hop-secret")),
        ProviderResponseHeader::new("x-hop-secret", Bytes::from_static(b"hop-secret")),
        ProviderResponseHeader::new("content-type", Bytes::from_static(b"application/private")),
        ProviderResponseHeader::new("bad\0name", Bytes::from_static(b"unrepresentable")),
    ];
    let session = FakeSession::streaming(
        Arc::clone(&trace),
        vec![NextStep::Event(delivery(
            started(),
            CommitRequirement::CommitBeforeDelivery,
        ))],
    )
    .with_response_headers(headers);

    let response = stream_execution_response(Box::new(session), None).await;

    assert_eq!(
        response
            .headers()
            .get("x-models-etag")
            .and_then(|value| value.to_str().ok()),
        Some("models-v2")
    );
    assert_eq!(
        response
            .headers()
            .get_all("x-future-multi")
            .iter()
            .map(HeaderValue::as_bytes)
            .collect::<Vec<_>>(),
        vec![b"first".as_slice(), b"second".as_slice()]
    );
    assert_eq!(
        response
            .headers()
            .get("x-future-bytes")
            .map(HeaderValue::as_bytes),
        Some(b"\xffopaque".as_slice())
    );
    assert_eq!(
        response
            .headers()
            .get("x-codex-turn-state")
            .and_then(|value| value.to_str().ok()),
        Some("turn-state-from-upstream")
    );
    assert!(response.headers().get("authorization").is_none());
    assert!(response.headers().get("connection").is_none());
    assert!(response.headers().get("x-hop-secret").is_none());
    assert_eq!(
        response.headers().get(CONTENT_TYPE),
        Some(&HeaderValue::from_static("text/event-stream"))
    );
    std::mem::forget(response);
}

#[tokio::test]
async fn streaming_commit_batch_preserves_pre_identity_wire_before_committing() {
    let trace = Arc::new(Trace::default());
    let metadata = ResponseMeta::new("resp_upstream", "public-model");
    let raw_future = Bytes::from_static(
        b"id: evt_before_identity\r\nevent: response.future_metadata\r\nretry: 3000\r\ndata: { \"type\": \"response.future_metadata\", \"opaque\": true }\r\n\r\n",
    );
    let events = vec![
        ProviderEvent::wire(
            ProtocolWireEvent::json_with_raw_sse_metadata(
                "openai",
                Some("response.future_metadata".to_owned()),
                json!({"type":"response.future_metadata","opaque":true}),
                raw_future.clone(),
                Some("evt_before_identity".to_owned()),
                Some(3_000),
            )
            .expect("future wire event"),
        ),
        ProviderEvent::canonical_with_wire(
            vec![GatewayEvent::Started(metadata.clone())],
            ProtocolWireEvent::json(
                "openai",
                Some("response.created".to_owned()),
                json!({
                    "type":"response.created",
                    "response":{"id":"resp_upstream","model":"public-model","status":"in_progress","output":[]}
                }),
            )
            .expect("created wire event"),
        ),
        ProviderEvent::canonical_with_wire(
            vec![GatewayEvent::Completed(metadata)],
            ProtocolWireEvent::json(
                "openai",
                Some("response.completed".to_owned()),
                json!({
                    "type":"response.completed",
                    "response":{"id":"resp_upstream","model":"public-model","status":"completed","output":[]}
                }),
            )
            .expect("completed wire event"),
        ),
    ];
    let batch = CoordinatedEvent::try_batch(events, CommitRequirement::CommitBeforeDelivery)
        .expect("non-empty commit batch");
    let session = FakeSession::streaming(
        Arc::clone(&trace),
        vec![NextStep::Event(batch), NextStep::FinalizeSuccess],
    );

    let response = stream_execution_response(Box::new(session), None).await;
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read SSE body");
    let body = String::from_utf8(body.to_vec()).expect("SSE is UTF-8");

    let future = body.find("response.future_metadata").expect("future event");
    let created = body.find("response.created").expect("created event");
    let completed = body.find("response.completed").expect("completed event");
    assert!(future < created && created < completed);
    assert!(body.as_bytes().starts_with(raw_future.as_ref()));
    assert!(body.contains("resp_upstream"));
    assert!(body.ends_with("data: [DONE]\n\n"));
    assert_eq!(trace.client_statuses(), vec![200]);
    assert_eq!(trace.snapshot(), vec!["next_event", "commit", "next_end"]);
    assert!(!trace.is_cancelled());
}

#[tokio::test]
async fn streaming_first_frame_encode_failure_cancels_before_commit() {
    let trace = Arc::new(Trace::default());
    let session = FakeSession::streaming(
        Arc::clone(&trace),
        vec![
            NextStep::Event(delivery_provider(
                ProviderEvent::canonical(completed()),
                CommitRequirement::CommitBeforeDelivery,
            )),
            NextStep::FinalizeCancelled,
        ],
    );

    let response = stream_execution_response(Box::new(session), None).await;

    assert_eq!(
        response.status(),
        axum::http::StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(trace.client_statuses(), vec![500]);
    assert!(trace.is_cancelled());
    assert_eq!(trace.snapshot(), vec!["next_event", "cancel_finalize"]);
}

#[tokio::test]
async fn streaming_rate_limit_before_first_frame_should_persist_the_returned_429_status() {
    let trace = Arc::new(Trace::default());
    let session = FakeSession::streaming(
        Arc::clone(&trace),
        vec![NextStep::Error(EngineError::Provider(
            ProviderError::new(ProviderErrorKind::RateLimited, UpstreamSendState::NotSent)
                .with_status(429),
        ))],
    );

    let response = stream_execution_response(Box::new(session), None).await;

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(trace.client_statuses(), vec![429]);
}

#[tokio::test]
async fn streaming_upstream_http_failure_before_first_frame_should_preserve_raw_response() {
    let trace = Arc::new(Trace::default());
    let raw_body = Bytes::from_static(
        b"{\"error\":{\"message\":\"rate limited\",\"future_field\":{\"kept\":true}},\"top_level\":17}\x00",
    );
    let error = ProviderError::new(ProviderErrorKind::RateLimited, UpstreamSendState::Sent)
        .with_client_visible_upstream_response(
            ClientVisibleUpstreamResponse::new(
                429,
                Some(b"application/problem+json; charset=utf-8".to_vec()),
                raw_body.clone(),
            )
            .with_headers(vec![
                ProviderResponseHeader::new("retry-after", Bytes::from_static(b"17")),
                ProviderResponseHeader::new("x-request-id", Bytes::from_static(b"req-failure")),
                ProviderResponseHeader::new("x-future-error", Bytes::from_static(b"first")),
                ProviderResponseHeader::new("x-future-error", Bytes::from_static(b"second")),
                ProviderResponseHeader::new("x-future-bytes", Bytes::from_static(b"\xffopaque")),
            ]),
        );
    let session = FakeSession::streaming(
        Arc::clone(&trace),
        vec![NextStep::Error(EngineError::Provider(error))],
    );

    let response = stream_execution_response(Box::new(session), None).await;

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        response
            .headers()
            .get(CONTENT_TYPE)
            .map(|value| value.as_bytes()),
        Some(b"application/problem+json; charset=utf-8".as_slice())
    );
    assert_eq!(
        response
            .headers()
            .get("retry-after")
            .map(|value| value.as_bytes()),
        Some(b"17".as_slice())
    );
    assert_eq!(
        response
            .headers()
            .get("x-request-id")
            .map(|value| value.as_bytes()),
        Some(b"req-failure".as_slice())
    );
    assert_eq!(
        response
            .headers()
            .get_all("x-future-error")
            .iter()
            .map(|value| value.as_bytes())
            .collect::<Vec<_>>(),
        vec![b"first".as_slice(), b"second".as_slice()]
    );
    assert_eq!(
        response
            .headers()
            .get("x-future-bytes")
            .map(|value| value.as_bytes()),
        Some(b"\xffopaque".as_slice())
    );
    assert_eq!(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read raw error body"),
        raw_body
    );
    assert_eq!(trace.client_statuses(), vec![429]);
}

#[tokio::test]
async fn buffered_upstream_http_failure_should_preserve_raw_response() {
    let trace = Arc::new(Trace::default());
    let raw_body = Bytes::from_static(b"not-json\xffstill-upstream");
    let error = ProviderError::new(ProviderErrorKind::Unavailable, UpstreamSendState::Sent)
        .with_client_visible_upstream_response(ClientVisibleUpstreamResponse::new(
            503,
            Some(b"application/octet-stream".to_vec()),
            raw_body.clone(),
        ));
    let session = FakeSession::buffered(Arc::clone(&trace), vec![started(), completed()])
        .with_collect_error(EngineError::Provider(error));

    let response = collect_execution_response(Box::new(session)).await;

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read raw error body"),
        raw_body
    );
    assert_eq!(trace.client_statuses(), vec![503]);
}

#[tokio::test]
async fn streaming_canonical_identity_change_does_not_interrupt_wire_delivery() {
    let trace = Arc::new(Trace::default());
    let session = FakeSession::streaming(
        Arc::clone(&trace),
        vec![
            NextStep::Event(delivery(started(), CommitRequirement::CommitBeforeDelivery)),
            NextStep::Event(delivery_provider(
                mismatched_terminal_event(),
                CommitRequirement::AlreadyCommitted,
            )),
            NextStep::FinalizeSuccess,
        ],
    );

    let response = stream_execution_response(Box::new(session), None).await;
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read SSE body");

    let body = String::from_utf8_lossy(&body);
    assert!(body.contains("response.completed"));
    assert!(body.contains("resp_other"));
    assert!(!body.contains("response.failed"));
    assert!(body.ends_with("data: [DONE]\n\n"));
    assert_eq!(trace.client_statuses(), vec![200]);
    assert!(!trace.is_cancelled());
    assert_eq!(
        trace.snapshot(),
        vec!["next_event", "commit", "next_event", "next_end"]
    );
}

#[tokio::test]
async fn streaming_success_should_emit_terminal_event_and_done_marker() {
    let trace = Arc::new(Trace::default());
    let session = FakeSession::streaming(
        Arc::clone(&trace),
        vec![
            NextStep::Event(delivery(started(), CommitRequirement::CommitBeforeDelivery)),
            NextStep::Event(delivery(
                GatewayEvent::ContentAdded(ContentItem::new(0, ContentKind::Text)),
                CommitRequirement::AlreadyCommitted,
            )),
            NextStep::Event(delivery(
                GatewayEvent::TextDelta(TextDelta {
                    content_index: 0,
                    text: "hello".to_owned(),
                }),
                CommitRequirement::AlreadyCommitted,
            )),
            NextStep::Event(delivery(completed(), CommitRequirement::AlreadyCommitted)),
            NextStep::FinalizeSuccess,
        ],
    );

    let response = stream_execution_response(Box::new(session), None).await;
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read complete SSE body");
    let body = String::from_utf8(body.to_vec()).expect("SSE is UTF-8");

    assert!(body.contains("event: response.completed"));
    assert!(body.ends_with("data: [DONE]\n\n"));
    assert!(!trace.is_cancelled());
    assert_eq!(
        trace.snapshot(),
        vec![
            "next_event",
            "commit",
            "next_event",
            "next_event",
            "next_event",
            "next_end",
        ]
    );
}

#[tokio::test]
async fn streaming_finalized_unknown_wire_should_end_cleanly_without_synthetic_failure() {
    let trace = Arc::new(Trace::default());
    let raw = Bytes::from_static(
        b"event: response.future_terminal\ndata: {\"type\":\"response.future_terminal\",\"opaque\":true}\n\n",
    );
    let event = ProviderEvent::wire(
        ProtocolWireEvent::raw_sse("openai", raw.clone()).expect("raw future event"),
    );
    let session = FakeSession::streaming(
        Arc::clone(&trace),
        vec![
            NextStep::Event(delivery_provider(
                event,
                CommitRequirement::CommitBeforeDelivery,
            )),
            NextStep::FinalizeSuccess,
        ],
    );

    let response = stream_execution_response(Box::new(session), None).await;
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read transparent SSE body");

    assert!(body.as_ref().starts_with(raw.as_ref()));
    assert!(body.as_ref().ends_with(b"data: [DONE]\n\n"));
    assert!(!String::from_utf8_lossy(&body).contains("response.failed"));
    assert!(!trace.is_cancelled());
    assert_eq!(trace.snapshot(), vec!["next_event", "commit", "next_end"]);
}

#[tokio::test]
async fn streaming_completed_event_without_finalized_execution_should_fail_closed() {
    let trace = Arc::new(Trace::default());
    let session = FakeSession::streaming(
        Arc::clone(&trace),
        vec![
            NextStep::Event(delivery(started(), CommitRequirement::CommitBeforeDelivery)),
            NextStep::Event(delivery(completed(), CommitRequirement::AlreadyCommitted)),
            NextStep::End,
            NextStep::FinalizeCancelled,
        ],
    );

    let response = stream_execution_response(Box::new(session), None).await;
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read failed SSE body");
    tokio::task::yield_now().await;
    let body = String::from_utf8(body.to_vec()).expect("SSE is UTF-8");

    assert!(!body.contains("event: response.completed"));
    assert!(body.contains("response.failed"));
    assert!(body.ends_with("data: [DONE]\n\n"));
    assert!(trace.is_cancelled());
    assert_eq!(
        trace.snapshot(),
        vec![
            "next_event",
            "commit",
            "next_event",
            "next_end",
            "cancel_finalize"
        ]
    );
}

#[tokio::test]
async fn streaming_empty_terminal_should_emit_failure_done_and_cancel_execution() {
    let trace = Arc::new(Trace::default());
    let session = FakeSession::streaming(
        Arc::clone(&trace),
        vec![
            NextStep::Event(delivery(started(), CommitRequirement::CommitBeforeDelivery)),
            NextStep::End,
            NextStep::FinalizeCancelled,
        ],
    );

    let response = stream_execution_response(Box::new(session), None).await;
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read failed SSE body");
    tokio::task::yield_now().await;
    let body = String::from_utf8(body.to_vec()).expect("SSE is UTF-8");

    assert!(body.contains("\"code\":\"internal_error\""));
    assert!(body.ends_with("data: [DONE]\n\n"));
    assert!(trace.is_cancelled());
    assert_eq!(
        trace.snapshot(),
        vec!["next_event", "commit", "next_end", "cancel_finalize"]
    );
}

#[tokio::test]
async fn streaming_error_should_emit_client_visible_upstream_details() {
    let trace = Arc::new(Trace::default());
    let error = ProviderError::new(ProviderErrorKind::RateLimited, UpstreamSendState::Sent)
        .with_client_visible_upstream_error(ClientVisibleUpstreamError::new(
            "Your Codex quota is exhausted",
            Some("quota_exhausted".to_owned()),
            Some("rate_limit_error".to_owned()),
        ));
    let session = FakeSession::streaming(
        Arc::clone(&trace),
        vec![
            NextStep::Event(delivery(started(), CommitRequirement::CommitBeforeDelivery)),
            NextStep::Error(EngineError::Provider(error)),
        ],
    );

    let response = stream_execution_response(Box::new(session), None).await;
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read SSE body");
    let body = String::from_utf8(body.to_vec()).expect("SSE is UTF-8");

    assert!(body.contains("event: response.failed"));
    assert!(body.contains("Your Codex quota is exhausted"));
    assert!(body.contains("\"code\":\"quota_exhausted\""));
    assert!(body.contains("\"type\":\"rate_limit_error\""));
    assert!(body.ends_with("data: [DONE]\n\n"));
}

#[tokio::test]
async fn streaming_upstream_wire_failure_should_not_be_rewritten_as_a_gateway_error() {
    let trace = Arc::new(Trace::default());
    let raw_failure = Bytes::from_static(
        b"event: response.failed\r\ndata: { \"type\": \"response.failed\", \"response\": { \"id\": \"resp_test\", \"status\": \"failed\", \"error\": { \"code\": \"rate_limit_exceeded\", \"message\": \"upstream raw failure marker\" } } }\r\n\r\n",
    );
    let wire_failure = ProviderEvent::wire(
        ProtocolWireEvent::json_with_raw_sse_metadata(
            "openai",
            Some("response.failed".to_owned()),
            json!({
                "type": "response.failed",
                "response": {
                    "id": "resp_test",
                    "status": "failed",
                    "error": {
                        "code": "rate_limit_exceeded",
                        "message": "upstream raw failure marker"
                    }
                }
            }),
            raw_failure.clone(),
            None,
            None,
        )
        .expect("valid upstream failure wire"),
    );
    let started_wire = ProviderEvent::canonical_with_wire(
        vec![started()],
        ProtocolWireEvent::json(
            "openai",
            Some("response.created".to_owned()),
            json!({
                "type": "response.created",
                "response": {
                    "id": "resp_test",
                    "model": "public-model",
                    "status": "in_progress"
                }
            }),
        )
        .expect("valid upstream started wire"),
    );
    let session = FakeSession::streaming(
        Arc::clone(&trace),
        vec![
            NextStep::Event(CoordinatedEvent::single(
                started_wire,
                CommitRequirement::CommitBeforeDelivery,
            )),
            NextStep::Event(CoordinatedEvent::single(
                wire_failure,
                CommitRequirement::AlreadyCommitted,
            )),
            NextStep::Error(EngineError::Provider(ProviderError::new(
                ProviderErrorKind::RateLimited,
                UpstreamSendState::Sent,
            ))),
        ],
    );

    let response = stream_execution_response(Box::new(session), None).await;
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read SSE body");

    assert!(
        body.windows(raw_failure.len())
            .any(|frame| frame == raw_failure.as_ref())
    );
    assert_eq!(
        String::from_utf8_lossy(&body)
            .matches("upstream raw failure marker")
            .count(),
        1
    );
    assert!(!String::from_utf8_lossy(&body).contains("resp_proxy_"));
    assert!(String::from_utf8_lossy(&body).ends_with("data: [DONE]\n\n"));
    assert_eq!(
        trace.snapshot(),
        vec!["next_event", "commit", "next_event", "next_error"]
    );
}

#[tokio::test]
async fn atomic_uncommitted_upstream_failure_batch_should_be_forwarded_once() {
    let trace = Arc::new(Trace::default());
    let raw_created = Bytes::from_static(
        b"event: response.created\r\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_atomic_api\",\"model\":\"public-model\",\"status\":\"in_progress\"}}\r\n\r\n",
    );
    let raw_failure = Bytes::from_static(
        b"event: response.failed\r\ndata: {\"type\":\"response.failed\",\"response\":{\"id\":\"resp_atomic_api\",\"status\":\"failed\",\"error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"atomic upstream marker\"}}}\r\n\r\n",
    );
    let started_wire = ProviderEvent::canonical_with_wire(
        vec![started()],
        ProtocolWireEvent::json_with_raw_sse_metadata(
            "openai",
            Some("response.created".to_owned()),
            json!({
                "type": "response.created",
                "response": {
                    "id": "resp_atomic_api",
                    "model": "public-model",
                    "status": "in_progress"
                }
            }),
            raw_created.clone(),
            None,
            None,
        )
        .expect("valid upstream started wire"),
    );
    let failed_wire = ProviderEvent::wire(
        ProtocolWireEvent::json_with_raw_sse_metadata(
            "openai",
            Some("response.failed".to_owned()),
            json!({
                "type": "response.failed",
                "response": {
                    "id": "resp_atomic_api",
                    "status": "failed",
                    "error": {
                        "code": "rate_limit_exceeded",
                        "message": "atomic upstream marker"
                    }
                }
            }),
            raw_failure.clone(),
            None,
            None,
        )
        .expect("valid upstream failure wire"),
    );
    let batch = CoordinatedEvent::try_batch(
        vec![started_wire, failed_wire],
        CommitRequirement::CommitBeforeDelivery,
    )
    .expect("atomic failure delivery");
    let session = FakeSession::streaming(
        Arc::clone(&trace),
        vec![
            NextStep::Event(batch),
            NextStep::Error(EngineError::Provider(ProviderError::new(
                ProviderErrorKind::RateLimited,
                UpstreamSendState::Sent,
            ))),
        ],
    );

    let response = stream_execution_response(Box::new(session), None).await;
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read SSE body");
    let body = String::from_utf8(body.to_vec()).expect("SSE is UTF-8");

    assert_eq!(body.matches("atomic upstream marker").count(), 1);
    assert_eq!(body.matches("event: response.failed").count(), 1);
    assert!(body.contains(std::str::from_utf8(&raw_created).expect("created frame")));
    assert!(body.contains(std::str::from_utf8(&raw_failure).expect("failure frame")));
    assert!(!body.contains("upstream capacity is temporarily unavailable"));
    assert!(body.ends_with("data: [DONE]\n\n"));
    assert_eq!(trace.snapshot(), vec!["next_event", "commit", "next_error"]);
}

#[tokio::test]
async fn streaming_second_commit_request_should_fail_and_finalize_once() {
    let trace = Arc::new(Trace::default());
    let session = FakeSession::streaming(
        Arc::clone(&trace),
        vec![
            NextStep::Event(delivery(started(), CommitRequirement::CommitBeforeDelivery)),
            NextStep::Event(delivery(
                completed(),
                CommitRequirement::CommitBeforeDelivery,
            )),
            NextStep::FinalizeCancelled,
        ],
    );

    let response = stream_execution_response(Box::new(session), None).await;
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read failed SSE body");
    let body = String::from_utf8(body.to_vec()).expect("SSE is UTF-8");

    assert!(body.contains("response.failed"));
    assert!(body.ends_with("data: [DONE]\n\n"));
    assert!(trace.is_cancelled());
    assert_eq!(
        trace.snapshot(),
        vec!["next_event", "commit", "next_event", "cancel_finalize"]
    );
}

#[tokio::test]
async fn streaming_commit_failure_should_not_deliver_the_prepared_first_frame() {
    let trace = Arc::new(Trace::default());
    let session = FakeSession::streaming(
        Arc::clone(&trace),
        vec![
            NextStep::Event(delivery(started(), CommitRequirement::CommitBeforeDelivery)),
            NextStep::FinalizeCancelled,
        ],
    )
    .with_commit_failure();

    let response = stream_execution_response(Box::new(session), None).await;
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read error response");

    assert_eq!(
        response_status_from_body(&body).as_deref(),
        Some("internal_error")
    );
    assert_eq!(trace.client_statuses(), vec![500]);
    assert!(trace.is_cancelled());
    assert_eq!(
        trace.snapshot(),
        vec!["next_event", "commit", "cancel_finalize"]
    );
}

#[tokio::test]
async fn buffered_response_commits_only_after_complete_json_is_encoded() {
    let trace = Arc::new(Trace::default());
    let session = FakeSession::buffered(Arc::clone(&trace), vec![started(), completed()]);

    let response = collect_execution_response(Box::new(session)).await;
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read JSON body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("valid response JSON");

    assert_eq!(json["id"], "resp_test");
    assert_eq!(trace.client_statuses(), vec![200]);
    assert_eq!(trace.snapshot(), vec!["collect", "commit"]);
    assert!(!trace.is_cancelled());
}

#[tokio::test]
async fn buffered_response_forwards_long_ordinary_provider_header_values() {
    let trace = Arc::new(Trace::default());
    let model = format!("gpt-{}", "x".repeat(512));
    let header = ProviderResponseHeader::new("openai-model", Bytes::from(model.clone()));
    let session = FakeSession::buffered(Arc::clone(&trace), vec![started(), completed()])
        .with_response_headers(vec![header]);

    let response = collect_execution_response(Box::new(session)).await;

    assert_eq!(
        response
            .headers()
            .get("openai-model")
            .and_then(|value| value.to_str().ok()),
        Some(model.as_str())
    );
}

#[tokio::test]
async fn buffered_encode_failure_cancels_without_commit() {
    let trace = Arc::new(Trace::default());
    let session = FakeSession::buffered_provider(
        Arc::clone(&trace),
        vec![ProviderEvent::canonical(completed())],
    );

    let response = collect_execution_response(Box::new(session)).await;

    assert_eq!(
        response.status(),
        axum::http::StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(trace.client_statuses(), vec![500]);
    assert!(trace.is_cancelled());
    assert_eq!(trace.snapshot(), vec!["collect", "cancel_finalize"]);
}

#[tokio::test]
async fn buffered_commit_failure_should_cancel_after_encoding_without_returning_success() {
    let trace = Arc::new(Trace::default());
    let session = FakeSession::buffered(Arc::clone(&trace), vec![started(), completed()])
        .with_commit_failure();

    let response = collect_execution_response(Box::new(session)).await;
    let status = response.status();

    assert_eq!(status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(trace.client_statuses(), vec![500]);
    assert!(trace.is_cancelled());
    assert_eq!(
        trace.snapshot(),
        vec!["collect", "commit", "cancel_finalize"]
    );
}

#[tokio::test]
async fn dropping_buffered_handler_before_commit_cancels_execution() {
    let trace = Arc::new(Trace::default());
    let session = FakeSession::pending_buffered(Arc::clone(&trace));
    let task = tokio::spawn(async move { collect_execution_response(Box::new(session)).await });
    tokio::task::yield_now().await;

    task.abort();
    let _ = task.await;
    tokio::task::yield_now().await;

    assert!(trace.is_cancelled());
    assert!(!trace.snapshot().contains(&"commit"));
}

#[tokio::test]
async fn buffered_rate_limit_should_persist_the_returned_429_status() {
    let trace = Arc::new(Trace::default());
    let session = FakeSession::buffered(Arc::clone(&trace), Vec::new()).with_collect_error(
        EngineError::Provider(
            ProviderError::new(ProviderErrorKind::RateLimited, UpstreamSendState::NotSent)
                .with_status(429),
        ),
    );

    let response = collect_execution_response(Box::new(session)).await;

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(trace.client_statuses(), vec![429]);
}

fn response_status_from_body(body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    value.pointer("/error/code")?.as_str().map(str::to_owned)
}
