mod http;
mod request;
mod response;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use futures::future::BoxFuture;
use gateway_core::engine::execution::{
    AuthenticatedClient, ClientAuthenticationError, ExecutionService, ExecutionSession,
    StartExecution, StartProviderExecution, StartedExecution,
};
use gateway_core::engine::{CommitRequirement, CoordinatedEvent, EngineError, ModelRequestId};
use gateway_core::error::GatewayError;
use gateway_core::event::{ProtocolWireEvent, ProviderEvent, ProviderResponseHeader};
use gateway_core::routing::PublicModelId;
use serde_json::{Value, json};
use tower::ServiceExt;

#[derive(Default)]
struct Trace(Mutex<Vec<String>>);

impl Trace {
    fn push(&self, value: impl Into<String>) {
        self.0.lock().unwrap().push(value.into());
    }
    fn events(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

enum Step {
    Batch(Vec<ProviderEvent>, CommitRequirement),
    Finish,
    Fail,
}

struct Session {
    trace: Arc<Trace>,
    steps: VecDeque<Step>,
    collected: Option<Vec<ProviderEvent>>,
    finalized: bool,
    committed: bool,
}

impl Session {
    fn json(events: Vec<ProviderEvent>) -> Self {
        Self {
            trace: Arc::default(),
            steps: VecDeque::from([Step::Finish]),
            collected: Some(events),
            finalized: false,
            committed: false,
        }
    }
    fn stream(steps: Vec<Step>) -> Self {
        Self {
            trace: Arc::default(),
            steps: steps.into(),
            collected: None,
            finalized: false,
            committed: false,
        }
    }
}

impl ExecutionSession for Session {
    fn next_event(&mut self) -> BoxFuture<'_, Result<Option<CoordinatedEvent>, EngineError>> {
        Box::pin(async move {
            self.trace.push("next");
            match self.steps.pop_front().unwrap_or(Step::Finish) {
                Step::Batch(events, requirement) => {
                    if requirement == CommitRequirement::AlreadyCommitted {
                        assert!(self.committed);
                    }
                    Ok(Some(
                        CoordinatedEvent::try_batch(events, requirement).unwrap(),
                    ))
                }
                Step::Finish => {
                    self.finalized = true;
                    self.trace.push("finalize");
                    Ok(None)
                }
                Step::Fail => {
                    self.finalized = true;
                    self.trace.push("finalize");
                    Err(EngineError::ProviderMetadataMismatch)
                }
            }
        })
    }
    fn collect_uncommitted(&mut self) -> BoxFuture<'_, Result<Vec<ProviderEvent>, EngineError>> {
        Box::pin(async move {
            self.trace.push("collect");
            Ok(self.collected.take().unwrap())
        })
    }
    fn response_headers(&self) -> &[ProviderResponseHeader] {
        &[]
    }
    fn commit_downstream(&mut self, status: Option<u16>) -> BoxFuture<'_, Result<(), EngineError>> {
        Box::pin(async move {
            self.trace.push(format!("commit:{status:?}"));
            self.committed = true;
            if self.trace.events().iter().any(|event| event == "collect") {
                self.finalized = true;
                self.trace.push("finalize");
            }
            Ok(())
        })
    }
    fn record_client_status(&mut self, status: u16) -> BoxFuture<'_, Result<(), EngineError>> {
        Box::pin(async move {
            self.trace.push(format!("status:{status}"));
            Ok(())
        })
    }
    fn is_finalized(&self) -> bool {
        self.finalized
    }
    fn cancel(&self) {
        self.trace.push("cancel");
    }
    fn detach_finalize(mut self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            if !self.finalized {
                let _ = self.next_event().await;
            }
        })
    }
}

struct Execution {
    session: Mutex<Option<Session>>,
}

impl ExecutionService for Execution {
    fn authenticate(
        &self,
        plaintext: &str,
    ) -> Result<AuthenticatedClient, ClientAuthenticationError> {
        if plaintext == "sk_chat_test" {
            Ok(crate::openai::authenticated_client_for_provider(
                plaintext, "openai",
            ))
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
            assert_eq!(request.metadata.endpoint, "/v1/chat/completions");
            Ok(StartedExecution {
                request_id: ModelRequestId::new("req_chat_test").unwrap(),
                created_at: UNIX_EPOCH + Duration::from_secs(1_700_000_000),
                stream: request.metadata.stream,
                session: Box::new(self.session.lock().unwrap().take().unwrap()),
            })
        })
    }
    fn start_provider_endpoint(
        &self,
        _: StartProviderExecution,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async { panic!("Chat must use shared model execution") })
    }
}

async fn call(session: Session, streaming: bool, include_usage: bool) -> axum::response::Response {
    let app = crate::openai::api_router(Arc::new(Execution {
        session: Mutex::new(Some(session)),
    }))
    .await;
    let mut body = json!({"model":"public-chat-model","messages":[{"role":"user","content":"hello"}],"stream":streaming});
    if streaming {
        body["stream_options"] = json!({"include_usage":include_usage});
    }
    app.oneshot(
        Request::post("/v1/chat/completions")
            .header("authorization", "Bearer sk_chat_test")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn json_response(events: Vec<ProviderEvent>) -> (StatusCode, Value) {
    let response = call(Session::json(events), false, false).await;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

fn wire(kind: &str, mut data: Value) -> ProviderEvent {
    data["type"] = json!(kind);
    ProviderEvent::wire(ProtocolWireEvent::json("openai", Some(kind.to_owned()), data).unwrap())
}

fn completed(output: Value) -> ProviderEvent {
    wire(
        "response.completed",
        json!({"response":{"id":"private-upstream-id","model":"private-model","status":"completed","output":output,"usage":{"input_tokens":10,"output_tokens":4,"total_tokens":14,"input_tokens_details":{"cached_tokens":3},"output_tokens_details":{"reasoning_tokens":2}}}}),
    )
}

fn message(text: &str) -> Value {
    json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]})
}

fn delta(text: &str) -> ProviderEvent {
    wire(
        "response.output_text.delta",
        json!({"output_index":0,"content_index":0,"delta":text}),
    )
}

fn stream_values(body: &str) -> Vec<Value> {
    body.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter(|data| *data != "[DONE]")
        .map(|data| serde_json::from_str(data).unwrap())
        .collect()
}
