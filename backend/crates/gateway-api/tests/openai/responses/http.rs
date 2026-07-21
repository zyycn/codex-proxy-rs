use std::collections::VecDeque;
use std::net::{IpAddr, SocketAddr};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use axum::{
    body::{Body, to_bytes},
    extract::connect_info::ConnectInfo,
    http::{HeaderMap, Request, StatusCode, header::AUTHORIZATION},
};
use futures::future::{BoxFuture, pending};
use gateway_core::engine::execution::{
    AuthenticatedClient, ClientAuthenticationError, ExecutionService, ExecutionSession,
    StartExecution, StartedExecution,
};
use gateway_core::engine::{CommitRequirement, CoordinatedEvent, EngineError, UpstreamSendState};
use gateway_core::error::{
    GatewayError, GatewayErrorKind, ProviderError, ProviderErrorKind, SafeUpstreamValue,
};
use gateway_core::event::{
    ContentItem, ContentKind, GatewayEvent, ProtocolWireEvent, ProviderEvent,
    ProviderResponseHeader, ResponseMeta, TextDelta,
};
use gateway_core::operation::Operation;
use gateway_core::routing::PublicModelId;
use serde_json::{Value, json};

use gateway_api::openai::responses::{collect_execution_response, stream_execution_response};
use tower::ServiceExt;

use crate::openai::{api_router, authenticated_client};

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
    client_ip: Option<IpAddr>,
    user_agent: Option<String>,
    endpoint: String,
    client_metadata: Option<Value>,
    openai_options: Option<Value>,
    xai_options: Option<Value>,
    prompt_cache_key: Option<String>,
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
        Vec::new()
    }

    fn contains_public_model(&self, _: &AuthenticatedClient, _: &PublicModelId) -> bool {
        true
    }

    fn start(
        &self,
        request: StartExecution,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async move {
            let (client_metadata, openai_options, xai_options, prompt_cache_key) =
                match &request.operation {
                    Operation::Generate(generate) => (
                        generate
                            .protocol_payload()
                            .and_then(|payload| payload.body().get("client_metadata"))
                            .cloned(),
                        generate
                            .provider_options()
                            .get("openai")
                            .cloned()
                            .map(Value::Object),
                        generate
                            .provider_options()
                            .get("xai")
                            .cloned()
                            .map(Value::Object),
                        generate.prompt_cache_key().map(ToOwned::to_owned),
                    ),
                    _ => (None, None, None, None),
                };
            *self.observed.lock().expect("context capture lock") = Some(CapturedClientContext {
                client_ip: request.metadata.client_ip,
                user_agent: request.metadata.user_agent,
                endpoint: request.metadata.endpoint,
                client_metadata,
                openai_options,
                xai_options,
                prompt_cache_key,
            });
            Err(GatewayError::new(
                GatewayErrorKind::Internal,
                "context capture completed",
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
        Self {
            trace,
            next: VecDeque::from([NextStep::FinalizeCancelled]),
            collected: Some(events.into_iter().map(ProviderEvent::canonical).collect()),
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
    CoordinatedEvent::single(ProviderEvent::canonical(event), commit_requirement)
}

async fn captured_client_context(
    mut headers: HeaderMap,
    peer_address: SocketAddr,
) -> CapturedClientContext {
    headers.insert(
        AUTHORIZATION,
        "Bearer sk_context_test".parse().expect("authorization"),
    );
    let observed = Arc::new(Mutex::new(None));
    let execution = Arc::new(ContextCaptureExecution {
        observed: Arc::clone(&observed),
        client: authenticated_client("sk_context_test"),
    });
    let mut request = Request::post("/v1/responses")
        .body(Body::from(r#"{"model":"smart-code","input":"hello"}"#))
        .expect("context request");
    *request.headers_mut() = headers;
    request.extensions_mut().insert(ConnectInfo(peer_address));
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
            client_ip: Some("198.51.100.1".parse().expect("expected IP")),
            user_agent: Some("Codex-CLI/1.0".to_owned()),
            endpoint: "/v1/responses".to_owned(),
            client_metadata: None,
            openai_options: None,
            xai_options: None,
            prompt_cache_key: None,
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
async fn http_request_should_forward_safe_codex_headers_without_projecting_xai_headers() {
    let peer = "192.0.2.10:443".parse().expect("peer address");
    let mut headers = HeaderMap::new();
    headers.insert("x-codex-turn-state", "turn-state".parse().expect("header"));
    headers.insert("conversation-id", "conversation-1".parse().expect("header"));
    headers.insert("session-id", "session-1".parse().expect("header"));
    headers.insert("x-grok-turn-idx", "7".parse().expect("header"));
    headers.insert("x-openai-subagent", "compact".parse().expect("header"));
    headers.insert(
        "x-openai-internal-codex-responses-lite",
        "true".parse().expect("header"),
    );

    let captured = captured_client_context(headers, peer).await;
    let options = captured
        .openai_options
        .as_ref()
        .and_then(Value::as_object)
        .expect("OpenAI options");
    assert_eq!(options.get("turn_state"), Some(&json!("turn-state")));
    assert_eq!(
        options.get("conversation_id"),
        Some(&json!("conversation-1"))
    );
    assert_eq!(options.get("session_id"), Some(&json!("session-1")));
    assert_eq!(options.get("responses_lite"), Some(&json!("true")));
    assert_eq!(captured.prompt_cache_key.as_deref(), Some("session-1"));
    assert!(captured.xai_options.is_none());
    assert!(!options.contains_key("authorization"));
    assert_eq!(
        captured
            .client_metadata
            .as_ref()
            .and_then(|metadata| metadata.get("x-openai-subagent")),
        Some(&json!("compact"))
    );
}

#[tokio::test]
async fn xai_private_headers_should_not_enter_openai_request_facts() {
    let peer = "192.0.2.10:443".parse().expect("peer address");
    let mut headers = HeaderMap::new();
    headers.insert("x-grok-turn-idx", "7".parse().expect("header"));
    headers.insert("x-grok-conv-id", "private-session".parse().expect("header"));

    let captured = captured_client_context(headers, peer).await;

    assert!(captured.openai_options.is_none());
    assert!(captured.xai_options.is_none());
    assert!(captured.prompt_cache_key.is_none());
}

#[tokio::test]
async fn review_route_should_inject_review_subagent_and_record_its_endpoint() {
    let observed = Arc::new(Mutex::new(None));
    let execution = Arc::new(ContextCaptureExecution {
        observed: Arc::clone(&observed),
        client: authenticated_client("sk_context_test"),
    });
    let request = Request::post("/v1/responses/review")
        .header(AUTHORIZATION, "Bearer sk_context_test")
        .header("x-openai-subagent", "compact")
        .body(Body::from(
            json!({
                "model": "smart-code",
                "input": "review this",
                "client_metadata": {"existing": "preserved"}
            })
            .to_string(),
        ))
        .expect("review request");

    let response = api_router(execution)
        .await
        .oneshot(request)
        .await
        .expect("review response");
    let captured = observed
        .lock()
        .expect("review capture lock")
        .clone()
        .expect("captured review request");

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(captured.endpoint, "/v1/responses/review");
    assert_eq!(
        captured
            .client_metadata
            .as_ref()
            .and_then(|metadata| metadata.get("x-openai-subagent")),
        Some(&json!("review"))
    );
    assert_eq!(
        captured
            .client_metadata
            .as_ref()
            .and_then(|metadata| metadata.get("existing")),
        Some(&json!("preserved"))
    );
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

    let response = stream_execution_response(Box::new(session), 1, None).await;

    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert_eq!(trace.client_statuses(), vec![200]);
    assert_eq!(trace.snapshot(), vec!["next_event", "commit"]);
    assert!(!trace.is_cancelled());
    std::mem::forget(response);
}

#[tokio::test]
async fn streaming_response_forwards_only_allowlisted_provider_headers() {
    let trace = Arc::new(Trace::default());
    let headers = vec![
        ProviderResponseHeader::new(
            "x-models-etag",
            SafeUpstreamValue::new("models-v2").expect("etag"),
        )
        .expect("safe header"),
        ProviderResponseHeader::new(
            "authorization",
            SafeUpstreamValue::new("should-not-cross-boundary").expect("header value"),
        )
        .expect("syntactically safe header"),
    ];
    let session = FakeSession::streaming(
        Arc::clone(&trace),
        vec![NextStep::Event(delivery(
            started(),
            CommitRequirement::CommitBeforeDelivery,
        ))],
    )
    .with_response_headers(headers);

    let response = stream_execution_response(Box::new(session), 1, None).await;

    assert_eq!(
        response
            .headers()
            .get("x-models-etag")
            .and_then(|value| value.to_str().ok()),
        Some("models-v2")
    );
    assert!(response.headers().get("authorization").is_none());
    std::mem::forget(response);
}

#[tokio::test]
async fn streaming_commit_batch_encodes_pre_identity_wire_before_committing() {
    let trace = Arc::new(Trace::default());
    let upstream_id = SafeUpstreamValue::new("resp_upstream").expect("upstream response ID");
    let metadata =
        ResponseMeta::new("resp_gateway", "public-model").with_upstream_response_id(upstream_id);
    let events = vec![
        ProviderEvent::wire(
            ProtocolWireEvent::json_with_sse_metadata(
                "openai",
                Some("response.future_metadata".to_owned()),
                json!({"type":"response.future_metadata","opaque":true}),
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

    let response = stream_execution_response(Box::new(session), 1, None).await;
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read SSE body");
    let body = String::from_utf8(body.to_vec()).expect("SSE is UTF-8");

    let future = body.find("response.future_metadata").expect("future event");
    let created = body.find("response.created").expect("created event");
    let completed = body.find("response.completed").expect("completed event");
    assert!(future < created && created < completed);
    assert!(body.contains("id: evt_before_identity\n"));
    assert!(body.contains("retry: 3000\n"));
    assert!(body.contains("resp_gateway"));
    assert!(!body.contains("resp_upstream"));
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
            NextStep::Event(delivery(
                completed(),
                CommitRequirement::CommitBeforeDelivery,
            )),
            NextStep::FinalizeCancelled,
        ],
    );

    let response = stream_execution_response(Box::new(session), 1, None).await;

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

    let response = stream_execution_response(Box::new(session), 1, None).await;

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(trace.client_statuses(), vec![429]);
}

#[tokio::test]
async fn streaming_postcommit_encode_failure_cancels_without_second_commit() {
    let trace = Arc::new(Trace::default());
    let unsupported = GatewayEvent::ContentAdded(ContentItem::new(0, ContentKind::Image));
    let session = FakeSession::streaming(
        Arc::clone(&trace),
        vec![
            NextStep::Event(delivery(started(), CommitRequirement::CommitBeforeDelivery)),
            NextStep::Event(delivery(unsupported, CommitRequirement::AlreadyCommitted)),
            NextStep::FinalizeCancelled,
        ],
    );

    let response = stream_execution_response(Box::new(session), 1, None).await;
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read SSE body");

    assert!(String::from_utf8_lossy(&body).contains("response.failed"));
    assert_eq!(trace.client_statuses(), vec![200]);
    assert!(trace.is_cancelled());
    assert_eq!(
        trace.snapshot(),
        vec!["next_event", "commit", "next_event", "cancel_finalize"]
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

    let response = stream_execution_response(Box::new(session), 1, None).await;
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

    let response = stream_execution_response(Box::new(session), 1, None).await;
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

    let response = stream_execution_response(Box::new(session), 1, None).await;
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

    let response = stream_execution_response(Box::new(session), 1, None).await;
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

    let response = stream_execution_response(Box::new(session), 1, None).await;
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

    let response = collect_execution_response(Box::new(session), 1).await;
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
async fn buffered_response_forwards_allowlisted_provider_headers() {
    let trace = Arc::new(Trace::default());
    let header = ProviderResponseHeader::new(
        "openai-model",
        SafeUpstreamValue::new("gpt-5.5-effective").expect("model header"),
    )
    .expect("safe header");
    let session = FakeSession::buffered(Arc::clone(&trace), vec![started(), completed()])
        .with_response_headers(vec![header]);

    let response = collect_execution_response(Box::new(session), 1).await;

    assert_eq!(
        response
            .headers()
            .get("openai-model")
            .and_then(|value| value.to_str().ok()),
        Some("gpt-5.5-effective")
    );
}

#[tokio::test]
async fn buffered_encode_failure_cancels_without_commit() {
    let trace = Arc::new(Trace::default());
    let session = FakeSession::buffered(Arc::clone(&trace), vec![completed()]);

    let response = collect_execution_response(Box::new(session), 1).await;

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

    let response = collect_execution_response(Box::new(session), 1).await;
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
    let task = tokio::spawn(async move { collect_execution_response(Box::new(session), 1).await });
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

    let response = collect_execution_response(Box::new(session), 1).await;

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(trace.client_statuses(), vec![429]);
}

fn response_status_from_body(body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    value.pointer("/error/code")?.as_str().map(str::to_owned)
}
