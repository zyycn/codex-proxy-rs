use std::collections::VecDeque;
use std::future::pending;
use std::net::{IpAddr, SocketAddr};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use async_trait::async_trait;
use axum::{
    body::{Bytes, to_bytes},
    extract::{Extension, State, connect_info::ConnectInfo},
    http::{HeaderMap, StatusCode, header::AUTHORIZATION},
};
use gateway_core::engine::{CommitRequirement, EngineError, UpstreamSendState};
use gateway_core::error::{GatewayError, GatewayErrorKind, ProviderError, ProviderErrorKind};
use gateway_core::event::{ContentItem, ContentKind, GatewayEvent, ResponseMeta, TextDelta};
use gateway_core::routing::PublicModelId;

use gateway_api::openai::responses::{
    DecodedResponsesRequest, collect_execution_response, responses, stream_execution_response,
};
use gateway_api::openai::{
    ConnectionTask, DeliveryEvent, OpenAiApiState, OpenAiClientService, ResponseExecutionSession,
    ResponsesTransport, StartedResponse, auth::ClientApiKeyAuthError,
};

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
    Event(DeliveryEvent),
    Error(EngineError),
    FinalizeCancelled,
    FinalizeSuccess,
    End,
}

struct FakeSession {
    trace: Arc<Trace>,
    next: VecDeque<NextStep>,
    collected: Option<Vec<GatewayEvent>>,
    collect_error: Option<EngineError>,
    collect_pending: bool,
    finalize_on_commit: bool,
    fail_commit: bool,
    finalized: bool,
}

#[derive(Clone)]
struct ContextCaptureState {
    observed: Arc<Mutex<Option<CapturedClientContext>>>,
}

type CapturedClientContext = (Option<IpAddr>, Option<String>);

#[async_trait]
impl OpenAiClientService for ContextCaptureState {
    type Client = ();
    type Session = FakeSession;

    fn authenticate(&self, plaintext: &str) -> Result<Self::Client, ClientApiKeyAuthError> {
        (plaintext == "sk_context_test")
            .then_some(())
            .ok_or(ClientApiKeyAuthError::InvalidKey)
    }

    fn public_models(&self, _client: &Self::Client) -> Vec<String> {
        Vec::new()
    }

    fn contains_public_model(&self, _client: &Self::Client, _model: &PublicModelId) -> bool {
        true
    }

    async fn start_response(
        &self,
        _client: Self::Client,
        request: DecodedResponsesRequest,
        _transport: ResponsesTransport,
    ) -> Result<StartedResponse<Self::Session>, GatewayError> {
        let metadata = request.metadata();
        *self.observed.lock().expect("context capture lock") = Some((
            metadata.client_ip(),
            metadata.user_agent().map(str::to_owned),
        ));
        Err(GatewayError::new(
            GatewayErrorKind::Internal,
            "context capture completed",
        ))
    }

    fn is_shutting_down(&self) -> bool {
        false
    }

    fn spawn_connection(&self, task: ConnectionTask) {
        drop(task);
    }

    fn next_connection_id(&self) -> String {
        "ws_context_test".to_owned()
    }

    fn next_request_id(&self) -> String {
        "req_context_test".to_owned()
    }
}

impl OpenAiApiState for ContextCaptureState {
    type Service = Self;

    fn openai_client_api(&self) -> Self::Service {
        self.clone()
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
        }
    }

    fn buffered(trace: Arc<Trace>, events: Vec<GatewayEvent>) -> Self {
        Self {
            trace,
            next: VecDeque::from([NextStep::FinalizeCancelled]),
            collected: Some(events),
            collect_error: None,
            collect_pending: false,
            finalize_on_commit: true,
            fail_commit: false,
            finalized: false,
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
}

#[async_trait]
impl ResponseExecutionSession for FakeSession {
    async fn next_delivery_event(&mut self) -> Result<Option<DeliveryEvent>, EngineError> {
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
    }

    async fn collect_uncommitted(&mut self) -> Result<Vec<GatewayEvent>, EngineError> {
        self.trace.push("collect");
        if self.collect_pending {
            pending::<()>().await;
        }
        if let Some(error) = self.collect_error.take() {
            self.finalized = true;
            return Err(error);
        }
        Ok(self.collected.take().unwrap_or_default())
    }

    async fn commit_downstream(
        &mut self,
        client_status_code: Option<u16>,
    ) -> Result<(), EngineError> {
        self.trace.push("commit");
        if self.fail_commit {
            return Err(EngineError::ProviderMetadataMismatch);
        }
        if let Some(status) = client_status_code {
            self.trace.record_client_status(status);
        }
        self.finalized = self.finalize_on_commit;
        Ok(())
    }

    async fn record_client_status(&mut self, client_status_code: u16) -> Result<(), EngineError> {
        self.trace.record_client_status(client_status_code);
        Ok(())
    }

    fn is_finalized(&self) -> bool {
        self.finalized
    }

    fn cancel(&self) {
        self.trace.cancel();
    }

    fn detach_finalize(mut self) {
        if self.finalized {
            return;
        }
        std::mem::drop(tokio::spawn(async move {
            let _ = self.next_delivery_event().await;
        }));
    }
}

fn started() -> GatewayEvent {
    GatewayEvent::Started(ResponseMeta::new("resp_test", "public-model"))
}

fn completed() -> GatewayEvent {
    GatewayEvent::Completed(ResponseMeta::new("resp_test", "public-model"))
}

fn delivery(event: GatewayEvent, commit_requirement: CommitRequirement) -> DeliveryEvent {
    DeliveryEvent::new(event, commit_requirement)
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
    let state = ContextCaptureState {
        observed: Arc::clone(&observed),
    };
    let response = responses::<ContextCaptureState>(
        State(state),
        Some(Extension(ConnectInfo(peer_address))),
        headers,
        Bytes::from_static(br#"{"model":"smart-code","input":"hello"}"#),
    )
    .await;
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
        (
            Some("198.51.100.1".parse().expect("expected IP")),
            Some("Codex-CLI/1.0".to_owned()),
        )
    );

    let mut headers = HeaderMap::new();
    headers.insert(
        "x-forwarded-for",
        "10.0.0.2, 203.0.113.3".parse().expect("forwarded IPs"),
    );
    assert_eq!(
        captured_client_context(headers, peer).await.0,
        Some("203.0.113.3".parse().expect("expected IP"))
    );

    assert_eq!(
        captured_client_context(HeaderMap::new(), peer).await.0,
        Some("192.0.2.10".parse().expect("expected peer IP"))
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

    let response = stream_execution_response(session, 1).await;

    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert_eq!(trace.client_statuses(), vec![200]);
    assert_eq!(trace.snapshot(), vec!["next_event", "commit"]);
    assert!(!trace.is_cancelled());
    std::mem::forget(response);
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

    let response = stream_execution_response(session, 1).await;

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

    let response = stream_execution_response(session, 1).await;

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

    let response = stream_execution_response(session, 1).await;
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

    let response = stream_execution_response(session, 1).await;
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

    let response = stream_execution_response(session, 1).await;
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

    let response = stream_execution_response(session, 1).await;
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

    let response = stream_execution_response(session, 1).await;
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

    let response = stream_execution_response(session, 1).await;
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

    let response = collect_execution_response(session, 1).await;
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
async fn buffered_encode_failure_cancels_without_commit() {
    let trace = Arc::new(Trace::default());
    let session = FakeSession::buffered(Arc::clone(&trace), vec![completed()]);

    let response = collect_execution_response(session, 1).await;

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

    let response = collect_execution_response(session, 1).await;
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
    let task = tokio::spawn(async move { collect_execution_response(session, 1).await });
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

    let response = collect_execution_response(session, 1).await;

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(trace.client_statuses(), vec![429]);
}

fn response_status_from_body(body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    value.pointer("/error/code")?.as_str().map(str::to_owned)
}
