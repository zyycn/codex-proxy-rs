//! Chat HTTP 夹具和共享执行、鉴权、插件及终结边界

use std::{
    collections::VecDeque,
    io::Write as _,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    response::Response,
};
use bytes::Bytes;
use futures::future::BoxFuture;
use gateway_core::{
    engine::{
        CommitRequirement, CoordinatedEvent, EngineError, ModelRequestId,
        execution::{
            AuthenticatedClient, ClientAuthenticationError, ExecutionService, ExecutionSession,
            PreparedRootExecution, ResponseCommit, StartExecution, StartProviderExecution,
            StartedExecution,
        },
        middleware::{
            FrozenMiddlewarePlan, MiddlewareContext, MiddlewareError, MiddlewareNext,
            MiddlewarePlan, MiddlewareRequest, MiddlewareResponse,
        },
    },
    error::GatewayError,
    event::{ProviderEvent, ProviderResponseHeader},
    operation::Operation,
    routing::{
        PublicModelId,
        extensions::{ExtensionSetId, ExtensionSetLease, ExtensionSetReference},
    },
};
use serde_json::{Value, json};
use tower::ServiceExt as _;

use super::{chat_request, completed, created, message};
use crate::openai::{
    api_router, authenticated_client, authenticated_client_for_provider_with_limit,
    middleware::{RequestMiddleware, ResponseFrameAction},
};

pub(super) const PATH: &str = "/v1/chat/completions";
const KEY: &str = "sk_chat_fixture";

#[derive(Default)]
pub(super) struct Trace {
    pub commits: AtomicUsize,
    pub discarded: AtomicUsize,
    pub cancelled: AtomicBool,
    pub finalized: AtomicBool,
    pub detached: AtomicUsize,
    response_commit: ResponseCommit,
}

#[derive(Default)]
pub(super) enum Finish {
    #[default]
    Success,
    Error,
    Pending,
}

pub(super) struct Session {
    events: VecDeque<Vec<ProviderEvent>>,
    pub trace: Arc<Trace>,
    pub finish: Finish,
    pub delay: Option<Duration>,
    pub fail_commit: bool,
    pub response_headers: Vec<ProviderResponseHeader>,
    committed: bool,
    buffered: bool,
}

impl Session {
    pub fn new(events: Vec<ProviderEvent>) -> Self {
        Self {
            events: events.into_iter().map(|event| vec![event]).collect(),
            trace: Arc::default(),
            finish: Finish::Success,
            delay: None,
            fail_commit: false,
            response_headers: Vec::new(),
            committed: false,
            buffered: false,
        }
    }

    pub fn with_batch(mut self, events: Vec<ProviderEvent>) -> Self {
        self.events.push_back(events);
        self
    }
}

impl ExecutionSession for Session {
    fn response_commit(&self) -> ResponseCommit {
        self.trace.response_commit.clone()
    }

    fn next_event(&mut self) -> BoxFuture<'_, Result<Option<CoordinatedEvent>, EngineError>> {
        Box::pin(async move {
            if let Some(delay) = self.delay.take() {
                tokio::time::sleep(delay).await;
            }
            if let Some(event) = self.events.pop_front() {
                let requirement = if self.committed {
                    CommitRequirement::AlreadyCommitted
                } else {
                    CommitRequirement::CommitBeforeDelivery
                };
                return CoordinatedEvent::try_batch(event, requirement).map(Some);
            }
            match self.finish {
                Finish::Success => {
                    self.trace.finalized.store(true, Ordering::SeqCst);
                    Ok(None)
                }
                Finish::Error => {
                    self.trace.finalized.store(true, Ordering::SeqCst);
                    Err(EngineError::ProviderMetadataMismatch)
                }
                Finish::Pending => futures::future::pending().await,
            }
        })
    }

    fn collect_uncommitted(&mut self) -> BoxFuture<'_, Result<Vec<ProviderEvent>, EngineError>> {
        self.buffered = true;
        Box::pin(async move { Ok(self.events.drain(..).flatten().collect()) })
    }

    fn response_headers(&self) -> &[ProviderResponseHeader] {
        &self.response_headers
    }

    fn discard_pending_delivery(&mut self) -> Result<(), EngineError> {
        self.trace.discarded.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn commit_downstream(&mut self, status: Option<u16>) -> BoxFuture<'_, Result<(), EngineError>> {
        Box::pin(async move {
            self.trace.commits.fetch_add(1, Ordering::SeqCst);
            if self.fail_commit {
                return Err(EngineError::ProviderMetadataMismatch);
            }
            self.trace.response_commit.record(status);
            self.committed = true;
            if self.buffered {
                self.trace.finalized.store(true, Ordering::SeqCst);
            }
            Ok(())
        })
    }

    fn record_client_status(&mut self, status: u16) -> BoxFuture<'_, Result<(), EngineError>> {
        Box::pin(async move {
            self.trace.response_commit.record(Some(status));
            Ok(())
        })
    }

    fn is_finalized(&self) -> bool {
        self.trace.finalized.load(Ordering::SeqCst)
    }
    fn cancel(&self) {
        self.trace.cancelled.store(true, Ordering::SeqCst);
    }
    fn detach_finalize(self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            self.trace.detached.fetch_add(1, Ordering::SeqCst);
            self.trace.finalized.store(true, Ordering::SeqCst);
        })
    }
}

#[derive(Clone, Debug)]
pub(super) struct Captured {
    pub body: Value,
    pub context: Value,
    pub endpoint: String,
}

pub(super) struct Execution {
    pub captured: Mutex<Option<Captured>>,
    pub client: AuthenticatedClient,
    pub middleware: Option<FrozenMiddlewarePlan>,
    session: Mutex<Option<Session>>,
}

impl Execution {
    pub fn new(session: Session) -> Self {
        Self {
            captured: Mutex::default(),
            client: authenticated_client(KEY),
            middleware: None,
            session: Mutex::new(Some(session)),
        }
    }
}

impl ExecutionService for Execution {
    fn authenticate(&self, key: &str) -> Result<AuthenticatedClient, ClientAuthenticationError> {
        if key == KEY {
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
    fn middleware_plan(&self, _: &PreparedRootExecution) -> Option<FrozenMiddlewarePlan> {
        self.middleware.clone()
    }
    fn start(
        &self,
        request: StartExecution,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async move {
            let Operation::Generate(generation) = request.operation else {
                panic!("Chat must generate")
            };
            *self.captured.lock().unwrap() = Some(Captured {
                body: Value::Object(generation.protocol_payload().body().clone()),
                context: Value::Object(generation.protocol_payload().context().clone()),
                endpoint: request.metadata.endpoint,
            });
            Ok(StartedExecution {
                request_id: ModelRequestId::new("req_chat_fixture").unwrap(),
                created_at: std::time::SystemTime::now(),
                stream: request.metadata.stream,
                session: Box::new(self.session.lock().unwrap().take().expect("one request")),
            })
        })
    }
    fn start_provider_endpoint(
        &self,
        _: StartProviderExecution,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async { panic!("Chat must use model execution") })
    }
}

pub(super) struct Fixture {
    pub router: axum::Router,
    pub execution: Arc<Execution>,
    pub trace: Arc<Trace>,
}

impl Fixture {
    pub async fn new(session: Session) -> Self {
        Self::with_execution(Execution::new(session)).await
    }
    pub async fn with_execution(execution: Execution) -> Self {
        let trace = execution
            .session
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .trace
            .clone();
        let execution = Arc::new(execution);
        Self {
            router: api_router(execution.clone()).await,
            execution,
            trace,
        }
    }
    pub async fn post(&self, body: Value) -> Response {
        self.router.clone().oneshot(request(body)).await.unwrap()
    }
    pub fn captured(&self) -> Captured {
        self.execution
            .captured
            .lock()
            .unwrap()
            .clone()
            .expect("started request")
    }
}

pub(super) fn request(body: Value) -> Request<Body> {
    Request::post(PATH)
        .header("authorization", format!("Bearer {KEY}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

pub(super) async fn json_body(response: Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
}

#[derive(Debug, Default)]
pub(super) struct InspectMiddleware {
    pub inner: RequestMiddleware,
    pub requests: Mutex<Vec<Value>>,
    pub update: Option<fn(&mut Value)>,
}

impl InspectMiddleware {
    pub fn frozen(self: &Arc<Self>) -> FrozenMiddlewarePlan {
        FrozenMiddlewarePlan::new(
            self.clone(),
            ExtensionSetReference::new(
                ExtensionSetId::new("chat-inspector".to_owned()).unwrap(),
                Arc::new(ReadyLease),
            ),
        )
    }
}

struct ReadyLease;
impl ExtensionSetLease for ReadyLease {
    fn is_ready(&self) -> bool {
        true
    }
}

impl MiddlewarePlan for InspectMiddleware {
    fn handle(
        &self,
        context: MiddlewareContext,
        mut request: MiddlewareRequest,
        next: MiddlewareNext,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        let mut body: Value = serde_json::from_slice(request.body()).unwrap();
        self.requests.lock().unwrap().push(body.clone());
        if let Some(update) = self.update {
            update(&mut body);
            let headers = request.headers().to_vec();
            request = request
                .replace_parts(
                    "openai".to_owned(),
                    headers,
                    Bytes::from(body.to_string()),
                    None,
                )
                .unwrap();
        }
        self.inner.handle(context, request, next)
    }
}

#[tokio::test]
async fn rejects_missing_authentication_before_decoding_or_execution() {
    let fixture = Fixture::new(Session::new(Vec::new())).await;
    for authorization in [None, Some("Bearer sk_invalid")] {
        let mut request = Request::post(PATH);
        if let Some(authorization) = authorization {
            request = request.header("authorization", authorization);
        }
        let response = fixture
            .router
            .clone()
            .oneshot(request.body(Body::from("broken json")).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    assert!(fixture.execution.captured.lock().unwrap().is_none());
}

#[tokio::test]
async fn request_middleware_sees_responses_and_preserves_the_chat_endpoint() {
    let middleware = Arc::new(InspectMiddleware {
        inner: RequestMiddleware {
            expected_operation: Some(gateway_core::operation::OperationKind::Generate),
            ..RequestMiddleware::default()
        },
        update: Some(|body| {
            body["input"] = json!("rewritten by request middleware");
        }),
        ..InspectMiddleware::default()
    });
    let mut execution = Execution::new(Session::new(vec![completed(json!([message("ok")]))]));
    execution.middleware = Some(middleware.frozen());
    let fixture = Fixture::with_execution(execution).await;
    let response = fixture.post(chat_request(false)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let downstream = json_body(response).await;
    assert_eq!(downstream["object"], "chat.completion");
    let observed = middleware.requests.lock().unwrap();
    assert!(observed[0].get("messages").is_none());
    assert_eq!(observed[0]["input"][0]["role"], "user");
    assert_eq!(
        middleware.inner.endpoints.lock().unwrap().as_slice(),
        [PATH]
    );
    let captured = fixture.captured();
    assert_eq!(captured.endpoint, PATH);
    assert_eq!(captured.body["input"], "rewritten by request middleware");
    assert_eq!(fixture.trace.commits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn dropping_chat_body_cancels_and_finalizes_the_original_execution() {
    let mut session = Session::new(vec![created()]);
    session.finish = Finish::Pending;
    let fixture = Fixture::new(session).await;
    let response = fixture.post(chat_request(true)).await;
    assert_eq!(response.status(), StatusCode::OK);
    drop(response);
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    assert!(fixture.trace.cancelled.load(Ordering::SeqCst));
    assert_eq!(fixture.trace.detached.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn gzip_and_headers_reuse_the_responses_ingress_contract() {
    let mut session = Session::new(vec![completed(json!([message("ok")]))]);
    session.response_headers = vec![
        ProviderResponseHeader::new("x-future-multi", Bytes::from_static(b"first")),
        ProviderResponseHeader::new("x-future-multi", Bytes::from_static(b"second")),
        ProviderResponseHeader::new("connection", Bytes::from_static(b"x-hop-only")),
        ProviderResponseHeader::new("x-hop-only", Bytes::from_static(b"removed")),
    ];
    let fixture = Fixture::new(session).await;
    let body = chat_request(false).to_string();
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(body.as_bytes()).unwrap();
    let compressed = encoder.finish().unwrap();
    let request = Request::post(PATH)
        .header("authorization", format!("Bearer {KEY}"))
        .header("content-type", "application/json")
        .header("content-encoding", "gzip")
        .header("content-length", compressed.len())
        .header("session_id", "chat-session")
        .header("x-future-input", "first")
        .header("x-future-input", "second")
        .body(Body::from(compressed))
        .unwrap();
    let response = fixture.router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get_all("x-future-multi")
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert!(!response.headers().contains_key("x-hop-only"));
    assert!(!response.headers().contains_key("content-encoding"));
    assert_eq!(
        json_body(response).await["choices"][0]["message"]["content"],
        "ok"
    );
    let captured = fixture.captured();
    assert_eq!(captured.context["session_id"], "chat-session");
    let headers = captured.context["opaque_request_headers"]
        .as_array()
        .unwrap();
    assert_eq!(
        headers
            .iter()
            .filter(|header| header[0] == "x-future-input")
            .count(),
        2
    );
    assert!(!headers.iter().any(|header| matches!(
        header[0].as_str(),
        Some("content-encoding" | "content-length" | "authorization")
    )));
}

#[tokio::test]
async fn corrupt_or_oversized_compressed_chat_never_starts_execution() {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder
        .write_all(chat_request(false).to_string().as_bytes())
        .unwrap();
    let valid_compressed = encoder.finish().unwrap();
    for bytes in [b"broken gzip".to_vec(), valid_compressed] {
        let mut execution = Execution::new(Session::new(Vec::new()));
        execution.client = authenticated_client_for_provider_with_limit(KEY, "openai", 16);
        let fixture = Fixture::with_execution(execution).await;
        let request = Request::post(PATH)
            .header("authorization", format!("Bearer {KEY}"))
            .header("content-encoding", "gzip")
            .body(Body::from(bytes))
            .unwrap();
        let response = fixture.router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(fixture.execution.captured.lock().unwrap().is_none());
    }
}

#[tokio::test]
async fn response_middleware_is_validated_as_responses_before_chat_projection() {
    for matching_identity in [true, false] {
        let replacement = json!({"id":if matching_identity {"resp_chat_test"} else {"resp_other"},
            "status":"completed","output":[message("plugin text")]});
        let middleware = Arc::new(
            RequestMiddleware {
                expected_operation: Some(gateway_core::operation::OperationKind::Generate),
                ..RequestMiddleware::default()
            }
            .with_response_actions(vec![ResponseFrameAction::Replace(Bytes::from(
                replacement.to_string(),
            ))]),
        );
        let mut execution =
            Execution::new(Session::new(vec![completed(json!([message("original")]))]));
        execution.middleware = Some(middleware.frozen());
        let fixture = Fixture::with_execution(execution).await;
        let response = fixture.post(chat_request(false)).await;
        if matching_identity {
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                json_body(response).await["choices"][0]["message"]["content"],
                "plugin text"
            );
        } else {
            assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
            assert_eq!(
                json_body(response).await["error"]["code"],
                "middleware_failed"
            );
            assert_eq!(fixture.trace.commits.load(Ordering::SeqCst), 0);
        }
    }
}

#[tokio::test]
async fn commit_failure_does_not_deliver_buffered_or_first_terminal_chat_success() {
    for streaming in [false, true] {
        let mut session = Session::new(vec![completed(json!([message("not delivered")]))]);
        session.fail_commit = true;
        let fixture = Fixture::new(session).await;
        let response = fixture.post(chat_request(streaming)).await;
        assert!(response.status().is_server_error());
        let body = json_body(response).await;
        assert!(body.get("error").is_some());
        assert!(body.get("choices").is_none());
        assert!(fixture.trace.cancelled.load(Ordering::SeqCst));
    }
}

#[tokio::test]
async fn the_same_execution_fixture_finishes_native_responses_with_one_commit() {
    for already_started in [false, true] {
        let mut events = Vec::new();
        if already_started {
            events.push(created());
        }
        events.push(completed(json!([message("native baseline")])));
        let fixture = Fixture::new(Session::new(events)).await;
        let mut request = request(json!({"model":"model-a","input":"hello","stream":true}));
        *request.uri_mut() = "/v1/responses".parse().unwrap();
        let response = fixture.router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(text.contains("event: response.completed"), "{text}");
        assert!(text.contains("native baseline"), "{text}");
        assert!(!text.contains("response.failed"), "{text}");
        assert_eq!(text.matches("data: [DONE]").count(), 1, "{text}");
        assert!(text.ends_with("data: [DONE]\n\n"), "{text}");
        assert!(fixture.trace.finalized.load(Ordering::SeqCst));
        assert_eq!(fixture.trace.commits.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.trace.response_commit.status_code(), Some(200));
    }
}

#[tokio::test]
async fn finalization_failure_never_leaks_success_finish_or_usage() {
    for already_started in [false, true] {
        let mut events = Vec::new();
        if already_started {
            events.push(created());
        }
        events.push(completed(json!([])));
        let mut session = Session::new(events);
        session.finish = Finish::Error;
        let fixture = Fixture::new(session).await;
        let mut body = chat_request(true);
        body["stream_options"] = json!({"include_usage":true});
        let response = fixture.post(body).await;
        // 原会话已经接受 200 提交，后续终结错误只能作为流内错误交付
        assert_eq!(response.status(), StatusCode::OK);
        let (_, chunks) = super::response::sse_body(response).await;
        assert!(chunks.iter().any(|chunk| chunk.get("error").is_some()));
        assert!(
            !chunks
                .iter()
                .any(|chunk| chunk["choices"][0]["finish_reason"].is_string())
        );
        assert!(!chunks.iter().any(|chunk| chunk["choices"] == json!([])));
        assert!(fixture.trace.finalized.load(Ordering::SeqCst));
        assert_eq!(fixture.trace.commits.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.trace.response_commit.status_code(), Some(200));
    }
}

#[tokio::test(start_paused = true)]
async fn error_after_admission_keepalive_uses_chat_error_encoding() {
    let mut session = Session::new(Vec::new());
    session.delay = Some(Duration::from_secs(31));
    session.finish = Finish::Error;
    let fixture = Fixture::new(session).await;
    let response = fixture.post(chat_request(true)).await;
    let (body, chunks) = super::response::sse_body(response).await;
    assert!(body.starts_with(": keep-alive\n\n"));
    assert!(chunks.iter().any(|chunk| chunk.get("error").is_some()));
    assert!(!body.contains("response.failed"));
    assert_eq!(fixture.trace.commits.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.trace.response_commit.status_code(), Some(200));
}
