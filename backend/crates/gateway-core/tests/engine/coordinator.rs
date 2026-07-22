//! 单行 `model_requests`、账号重试与下游提交屏障测试。

use std::collections::{BTreeSet, VecDeque};
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use futures::executor::block_on;

use gateway_core::accounting::{CalculatedCost, CostSource, ProviderReportedCost, Usage};
use gateway_core::engine::continuation::{
    ContinuationBinding, NativeContinuationPin, PreviousResponseId,
};
use gateway_core::engine::credential::{
    AccountSelectionPolicy, CredentialRevision, ProviderAccountId, RotationStrategy,
};
use gateway_core::engine::provider::{
    Provider, ProviderCallMetadata, ProviderCatalogGeneration, ProviderModelCapabilities,
    ProviderRegistry, ProviderRequest, ProviderResource, ProviderStream, UpstreamTransport,
};
use gateway_core::engine::{
    AttemptContext, AttemptCoordinator, AttemptRecord, CancellationToken, CommitRequirement,
    ContinuationAttempt, ExecutionOutcome, ExecutionStore, GatewayEngine, IntermediateFailure,
    ModelRequestFinalization, ModelRequestId, NewModelRequest, ProviderAttemptOutcome,
    RecoveryReport, UpstreamSendState,
};
use gateway_core::error::{
    ContinuationFailure, ProviderError, ProviderErrorKind, SafeUpstreamValue, StoreError,
};
use gateway_core::event::{
    GatewayEvent, ProtocolWireEvent, ProviderEvent, ProviderResponseObservation,
    ProviderResponseTimings, ResponseMeta, UpstreamHttpVersion, WebSocketPoolKind,
};
use gateway_core::operation::{
    ContentPart, EmbedRequest, GenerateRequest, Message, MessageRole, Operation,
    ProviderSessionState, RetrySafety,
};
use gateway_core::policy::ClientApiKeyId;
use gateway_core::routing::{
    ConfigRevision, InstanceHealth, ModelCapabilities, ProviderInstance, ProviderInstanceId,
    ProviderKind, ProviderModel, PublicModelId, RoutingContext, RoutingPlan, RuntimeSnapshot,
    UpstreamModelId,
};
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
struct FinalState {
    outcome: ExecutionOutcome,
    send_state: UpstreamSendState,
    attempt_count: u32,
    committed: bool,
    client_status_code: Option<u16>,
    total_tokens: Option<u64>,
    image_input_tokens: Option<u64>,
    image_output_tokens: Option<u64>,
    image_generation_succeeded: Option<bool>,
    provider_error_code: Option<String>,
    retry_after_ms: Option<u64>,
    latency_ms: Option<u64>,
    client_response_id: Option<String>,
    upstream_response_id: Option<String>,
    upstream_transport: Option<String>,
    http_version: Option<String>,
    websocket_pool: Option<String>,
    upstream_request_id: Option<String>,
    upstream_status_code: Option<u16>,
    transport_decision_wait_ms: Option<u64>,
    connect_ms: Option<u64>,
    headers_ms: Option<u64>,
    first_event_ms: Option<u64>,
    cost_source: CostSource,
    cost_ticks: Option<u128>,
}

#[derive(Default)]
struct StoreState {
    created: usize,
    attempts: Vec<AttemptRecord>,
    send_states: Vec<UpstreamSendState>,
    commits: usize,
    committed_statuses: Vec<Option<u16>>,
    recorded_statuses: Vec<u16>,
    intermediate_failures: usize,
    intermediate_status_codes: Vec<Option<u16>>,
    intermediate_request_ids: Vec<Option<String>>,
    finalizations: Vec<FinalState>,
}

#[derive(Default)]
struct FakeStore {
    state: Mutex<StoreState>,
}

#[async_trait]
impl ExecutionStore for FakeStore {
    async fn create_model_request(&self, _request: NewModelRequest) -> Result<(), StoreError> {
        self.state.lock().expect("store lock").created += 1;
        Ok(())
    }

    async fn record_attempt(&self, attempt: AttemptRecord) -> Result<(), StoreError> {
        self.state
            .lock()
            .expect("store lock")
            .attempts
            .push(attempt);
        Ok(())
    }

    async fn mark_send_state(
        &self,
        _request_id: &ModelRequestId,
        state: UpstreamSendState,
    ) -> Result<(), StoreError> {
        self.state
            .lock()
            .expect("store lock")
            .send_states
            .push(state);
        Ok(())
    }

    async fn mark_downstream_committed(
        &self,
        _request_id: &ModelRequestId,
        _committed_at: SystemTime,
        client_status_code: Option<u16>,
    ) -> Result<(), StoreError> {
        let mut state = self.state.lock().expect("store lock");
        state.commits += 1;
        state.committed_statuses.push(client_status_code);
        Ok(())
    }

    async fn record_client_status(
        &self,
        _request_id: &ModelRequestId,
        client_status_code: u16,
    ) -> Result<(), StoreError> {
        self.state
            .lock()
            .expect("store lock")
            .recorded_statuses
            .push(client_status_code);
        Ok(())
    }

    async fn record_intermediate_failure(
        &self,
        failure: IntermediateFailure,
    ) -> Result<(), StoreError> {
        let mut state = self.state.lock().expect("store lock");
        state.intermediate_failures += 1;
        state
            .intermediate_status_codes
            .push(failure.upstream_status_code);
        state
            .intermediate_request_ids
            .push(failure.upstream_request_id);
        Ok(())
    }

    async fn finalize_model_request(
        &self,
        finalization: ModelRequestFinalization,
    ) -> Result<(), StoreError> {
        self.state
            .lock()
            .expect("store lock")
            .finalizations
            .push(FinalState {
                outcome: finalization.outcome,
                send_state: finalization.send_state,
                attempt_count: finalization.attempt_count,
                committed: finalization.downstream_committed_at.is_some(),
                client_status_code: finalization.client_status_code,
                total_tokens: finalization.usage.total_tokens,
                image_input_tokens: finalization.usage.image_input_tokens,
                image_output_tokens: finalization.usage.image_output_tokens,
                image_generation_succeeded: finalization.image_generation_succeeded,
                provider_error_code: finalization.provider_error_code,
                retry_after_ms: finalization.retry_after_ms,
                latency_ms: finalization.timings.latency_ms,
                client_response_id: finalization.client_response_id,
                upstream_response_id: finalization.upstream_response_id,
                upstream_transport: finalization.upstream_transport,
                http_version: finalization.http_version,
                websocket_pool: finalization.websocket_pool,
                upstream_request_id: finalization.upstream_request_id,
                upstream_status_code: finalization.upstream_status_code,
                transport_decision_wait_ms: finalization.timings.transport_decision_wait_ms,
                connect_ms: finalization.timings.connect_ms,
                headers_ms: finalization.timings.headers_ms,
                first_event_ms: finalization.timings.first_event_ms,
                cost_source: finalization.cost.source(),
                cost_ticks: finalization
                    .cost
                    .total()
                    .map(|total| total.amount().scaled()),
            });
        Ok(())
    }

    async fn recover_expired(&self, _now: SystemTime) -> Result<RecoveryReport, StoreError> {
        Ok(RecoveryReport::default())
    }
}

enum Script {
    Stream {
        account_id: &'static str,
        items: Vec<Result<GatewayEvent, ProviderError>>,
    },
    ObservedStream {
        account_id: &'static str,
        items: Vec<Result<ProviderEvent, ProviderError>>,
    },
    Error(ProviderError),
}

struct ScriptedProvider {
    scripts: Mutex<VecDeque<Script>>,
    contexts: Mutex<Vec<AttemptContext>>,
}

impl ScriptedProvider {
    fn new(scripts: Vec<Script>) -> Self {
        Self {
            scripts: Mutex::new(scripts.into()),
            contexts: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn name(&self) -> &'static str {
        "openai"
    }

    fn catalog_generation(&self) -> ProviderCatalogGeneration {
        ProviderCatalogGeneration::default()
    }

    async fn query_model_capabilities(
        &self,
        _instance: &ProviderInstance,
    ) -> Result<Vec<ProviderModelCapabilities>, ProviderError> {
        Ok(Vec::new())
    }

    async fn execute(
        &self,
        request: ProviderRequest,
        context: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        self.contexts.lock().expect("contexts lock").push(context);
        let script = self
            .scripts
            .lock()
            .expect("scripts lock")
            .pop_front()
            .expect("one script per provider call");
        match script {
            Script::Error(error) => Err(error),
            Script::Stream { account_id, items } => {
                let candidate = request.candidate();
                let metadata = ProviderCallMetadata::new(
                    candidate.provider().clone(),
                    candidate.instance().clone(),
                    candidate.upstream_model().clone(),
                    ProviderResource::Account {
                        id: ProviderAccountId::new(account_id).expect("account id"),
                        revision: CredentialRevision::new(1).expect("revision"),
                    },
                    UpstreamTransport::new("http_sse").expect("transport"),
                )
                .with_upstream_request_id(
                    SafeUpstreamValue::new("upstream-request").expect("safe request id"),
                );
                Ok(ProviderStream::new(
                    metadata,
                    Box::pin(futures::stream::iter(
                        items
                            .into_iter()
                            .map(|event| event.map(ProviderEvent::from)),
                    )),
                    (),
                ))
            }
            Script::ObservedStream { account_id, items } => {
                let candidate = request.candidate();
                let metadata = ProviderCallMetadata::new(
                    candidate.provider().clone(),
                    candidate.instance().clone(),
                    candidate.upstream_model().clone(),
                    ProviderResource::Account {
                        id: ProviderAccountId::new(account_id).expect("account id"),
                        revision: CredentialRevision::new(1).expect("revision"),
                    },
                    UpstreamTransport::new("websocket").expect("planned transport"),
                );
                Ok(ProviderStream::new(
                    metadata,
                    Box::pin(futures::stream::iter(items)),
                    (),
                ))
            }
        }
    }
}

fn operation(retry_safety: RetrySafety) -> Operation {
    if retry_safety == RetrySafety::Idempotent {
        return Operation::Embed(
            EmbedRequest::new(vec!["hello".to_owned()]).expect("embedding request"),
        );
    }
    generate_operation()
}

fn generate_operation() -> Operation {
    let message = Message::new(
        MessageRole::User,
        vec![ContentPart::Text("hello".to_owned())],
    )
    .expect("message");
    Operation::Generate(GenerateRequest::new(vec![message]).expect("generate request"))
}

fn image_generate_operation() -> Operation {
    let Operation::Generate(request) = generate_operation() else {
        unreachable!("generate_operation must return Generate")
    };
    Operation::Generate(request.with_image_generation_requested(true))
}

fn complete_stream(total_tokens: Option<u64>) -> Vec<Result<GatewayEvent, ProviderError>> {
    let mut events = vec![Ok(GatewayEvent::Started(ResponseMeta::new(
        "response-1",
        "gpt-5",
    )))];
    if total_tokens.is_some() {
        events.push(Ok(GatewayEvent::Usage(Usage {
            total_tokens,
            ..Usage::new()
        })));
    }
    events.push(Ok(GatewayEvent::Completed(
        ResponseMeta::new("response-1", "gpt-5").with_upstream_response_id(
            SafeUpstreamValue::new("upstream-response").expect("safe response id"),
        ),
    )));
    events
}

fn image_stream(image_output_tokens: Option<u64>) -> Vec<Result<GatewayEvent, ProviderError>> {
    let mut usage = Usage::new();
    usage.input_tokens = Some(12);
    usage.output_tokens = Some(5);
    usage.image_input_tokens = Some(31);
    usage.image_output_tokens = image_output_tokens;
    usage.total_tokens = Some(17);
    vec![
        Ok(GatewayEvent::Started(ResponseMeta::new(
            "response-image",
            "gpt-5",
        ))),
        Ok(GatewayEvent::Usage(usage)),
        Ok(GatewayEvent::Completed(ResponseMeta::new(
            "response-image",
            "gpt-5",
        ))),
    ]
}

fn plan(operation: &Operation, instance_count: u32) -> RoutingPlan {
    let provider = ProviderKind::new("openai").expect("provider");
    let public_model = PublicModelId::new("gpt-5").expect("public model");
    let capabilities =
        ModelCapabilities::new(BTreeSet::from([operation.kind()]), 128_000, Some(32_000));
    let mut instances = Vec::new();
    let mut provider_models = Vec::new();
    for index in 1..=instance_count {
        let instance_id =
            ProviderInstanceId::new(format!("inst_openai_{index}")).expect("instance id");
        instances.push(ProviderInstance::new(
            instance_id.clone(),
            provider.clone(),
            format!("https://openai-{index}.example"),
            true,
            InstanceHealth::Healthy,
        ));
        provider_models.push(ProviderModel::new(
            instance_id,
            UpstreamModelId::new("gpt-5").expect("upstream model"),
            capabilities.clone(),
        ));
    }
    RuntimeSnapshot::new(
        ConfigRevision::new(1).expect("config revision"),
        AccountSelectionPolicy::new(
            RotationStrategy::Smart,
            NonZeroU32::new(2).expect("account concurrency"),
            Duration::from_millis(50),
        ),
        instances,
        provider_models,
        Vec::new(),
    )
    .expect("snapshot")
    .plan(
        &public_model,
        operation,
        &RoutingContext {
            provider_kind: Some(provider),
            ..RoutingContext::default()
        },
    )
    .expect("routing plan")
}

fn model_request(operation: &Operation, deadline: SystemTime) -> NewModelRequest {
    let client_key = ClientApiKeyId::new("key_client_1").expect("client key id");
    NewModelRequest {
        id: ModelRequestId::new("req_core_1").expect("request id"),
        client_api_key_id: Some(client_key.clone()),
        client_api_key_ref: client_key,
        config_revision: ConfigRevision::new(1).expect("config revision"),
        protocol: "openai".to_owned(),
        operation: operation.kind(),
        endpoint: "responses".to_owned(),
        client_transport: "http_sse".to_owned(),
        requested_model: PublicModelId::new("gpt-5").expect("model"),
        input_token_estimate: 3,
        client_ip: Some("127.0.0.1".parse().expect("client IP")),
        user_agent: Some("gateway-core-test".to_owned()),
        reasoning_effort: Some("medium".to_owned()),
        reasoning_preset: Some("balanced".to_owned()),
        request_kind: Some("responses".to_owned()),
        subagent_kind: None,
        compact: false,
        image_generation_requested: operation.image_generation_requested(),
        started_at: SystemTime::now(),
        deadline_at: deadline,
    }
}

fn coordinator(
    scripts: Vec<Script>,
) -> (
    AttemptCoordinator<FakeStore>,
    Arc<FakeStore>,
    Arc<ScriptedProvider>,
) {
    let store = Arc::new(FakeStore::default());
    let provider = Arc::new(ScriptedProvider::new(scripts));
    let mut registry = ProviderRegistry::builder();
    registry
        .register(provider.clone())
        .expect("register provider");
    let engine = GatewayEngine::new(store.clone(), registry.build());
    (AttemptCoordinator::new(engine), store, provider)
}

fn terminal_non_idempotent_failure(
    items: Vec<Result<GatewayEvent, ProviderError>>,
    continuation: Option<ContinuationBinding>,
) -> (Arc<FakeStore>, Arc<ScriptedProvider>) {
    let operation = operation(RetrySafety::NonIdempotent);
    let route_plan = plan(&operation, 1);
    let (coordinator, store, provider) = coordinator(vec![
        Script::Stream {
            account_id: "acct_first",
            items,
        },
        Script::Stream {
            account_id: "acct_second",
            items: complete_stream(None),
        },
    ]);
    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        None,
        continuation,
        CancellationToken::new(),
    ))
    .expect("start execution");
    let error = block_on(session.collect_uncommitted()).expect_err("failure must be terminal");
    assert!(matches!(
        error,
        gateway_core::engine::EngineError::Provider(_)
    ));
    (store, provider)
}

#[test]
fn success_updates_one_model_request_and_persists_usage() {
    let operation = operation(RetrySafety::Idempotent);
    let route_plan = plan(&operation, 1);
    let (coordinator, store, _) = coordinator(vec![Script::Stream {
        account_id: "acct_one",
        items: complete_stream(Some(12)),
    }]);

    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        None,
        None,
        CancellationToken::new(),
    ))
    .expect("start execution");
    let events = block_on(session.collect_uncommitted()).expect("collect response");
    assert_eq!(events.len(), 3);
    let started_id = match &events[0].canonical_facts()[0] {
        GatewayEvent::Started(metadata) => metadata.response_id(),
        event => panic!("unexpected first event: {event:?}"),
    };
    let completed_id = match &events[2].canonical_facts()[0] {
        GatewayEvent::Completed(metadata) => metadata.response_id(),
        event => panic!("unexpected final event: {event:?}"),
    };
    assert!(started_id.starts_with("resp_"));
    assert_eq!(started_id, completed_id);
    assert_ne!(started_id, "response-1");
    assert!(!session.is_finalized());
    block_on(session.commit_downstream(Some(200))).expect("commit response");

    assert!(session.is_finalized());
    assert_eq!(
        session.provider_attempt_outcomes(),
        &[ProviderAttemptOutcome::Succeeded {
            provider_instance_id: ProviderInstanceId::new("inst_openai_1").expect("instance"),
        }]
    );
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.created, 1);
    assert_eq!(state.attempts.len(), 1);
    assert_eq!(state.commits, 1);
    assert_eq!(state.committed_statuses, vec![Some(200)]);
    assert_eq!(state.intermediate_failures, 0);
    let finalization = &state.finalizations[0];
    assert_eq!(finalization.outcome, ExecutionOutcome::Succeeded);
    assert_eq!(finalization.send_state, UpstreamSendState::Sent);
    assert_eq!(finalization.attempt_count, 1);
    assert!(finalization.committed);
    assert_eq!(finalization.client_status_code, Some(200));
    assert_eq!(finalization.total_tokens, Some(12));
    assert!(finalization.latency_ms.is_some());
    assert_eq!(finalization.client_response_id.as_deref(), Some(started_id));
    assert_eq!(
        finalization.upstream_response_id.as_deref(),
        Some("response-1")
    );
}

fn finalized_image_request(image_output_tokens: Option<u64>) -> FinalState {
    let operation = image_generate_operation();
    let route_plan = plan(&operation, 1);
    let (coordinator, store, _) = coordinator(vec![Script::Stream {
        account_id: "acct_image",
        items: image_stream(image_output_tokens),
    }]);
    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        None,
        None,
        CancellationToken::new(),
    ))
    .expect("start image execution");
    block_on(session.collect_uncommitted()).expect("collect image response");
    block_on(session.commit_downstream(Some(200))).expect("commit image response");

    store.state.lock().expect("store lock").finalizations[0].clone()
}

#[test]
fn image_request_with_output_should_persist_success_and_image_tokens() {
    let finalization = finalized_image_request(Some(9));

    assert_eq!(
        (
            finalization.image_generation_succeeded,
            finalization.image_input_tokens,
            finalization.image_output_tokens,
        ),
        (Some(true), Some(31), Some(9))
    );
}

#[test]
fn image_request_without_output_should_persist_failure() {
    let finalization = finalized_image_request(None);

    assert_eq!(finalization.image_generation_succeeded, Some(false));
}

#[test]
fn failed_image_request_should_persist_failure() {
    let operation = image_generate_operation();
    let route_plan = plan(&operation, 1);
    let (coordinator, store, _) = coordinator(vec![Script::Stream {
        account_id: "acct_image",
        items: vec![Err(ProviderError::new(
            ProviderErrorKind::Protocol,
            UpstreamSendState::Sent,
        ))],
    }]);
    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        None,
        None,
        CancellationToken::new(),
    ))
    .expect("start image execution");
    block_on(session.collect_uncommitted()).expect_err("image request should fail");
    let state = store.state.lock().expect("store lock");

    assert_eq!(
        state.finalizations[0].image_generation_succeeded,
        Some(false)
    );
}

#[test]
fn response_observation_is_persisted_but_never_delivered() {
    let operation = operation(RetrySafety::Idempotent);
    let route_plan = plan(&operation, 1);
    let observation = ProviderResponseObservation::new(
        UpstreamTransport::new("http_sse").expect("actual transport"),
    )
    .with_http_version(UpstreamHttpVersion::Http2)
    .with_websocket_pool(WebSocketPoolKind::New)
    .with_status_code(200)
    .with_request_id(SafeUpstreamValue::new("upstream-observed").expect("request id"))
    .with_timings(ProviderResponseTimings {
        transport_decision_wait_ms: Some(7),
        connect_ms: Some(11),
        headers_ms: Some(13),
        first_event_ms: Some(17),
    });
    let mut items = vec![Ok(ProviderEvent::observation(observation))];
    items.extend(
        complete_stream(Some(21))
            .into_iter()
            .map(|event| event.map(ProviderEvent::canonical)),
    );
    let (coordinator, store, _) = coordinator(vec![Script::ObservedStream {
        account_id: "acct_observed",
        items,
    }]);
    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        None,
        None,
        CancellationToken::new(),
    ))
    .expect("start execution");

    let events = block_on(session.collect_uncommitted()).expect("collect response");
    assert_eq!(events.len(), 3);
    block_on(session.commit_downstream(Some(200))).expect("commit response");

    let state = store.state.lock().expect("store lock");
    let finalization = &state.finalizations[0];
    assert_eq!(finalization.upstream_transport.as_deref(), Some("http_sse"));
    assert_eq!(finalization.http_version.as_deref(), Some("HTTP/2"));
    assert_eq!(finalization.websocket_pool.as_deref(), Some("new"));
    assert_eq!(
        finalization.upstream_request_id.as_deref(),
        Some("upstream-observed")
    );
    assert_eq!(finalization.upstream_status_code, Some(200));
    assert_eq!(finalization.transport_decision_wait_ms, Some(7));
    assert_eq!(finalization.connect_ms, Some(11));
    assert_eq!(finalization.headers_ms, Some(13));
    assert_eq!(finalization.first_event_ms, Some(17));
}

#[test]
fn unknown_wire_event_before_response_identity_is_discarded_with_retried_attempt() {
    let operation = operation(RetrySafety::Idempotent);
    let route_plan = plan(&operation, 1);
    let unknown = ProtocolWireEvent::json(
        "openai",
        Some("response.future_event".to_owned()),
        serde_json::json!({"type": "response.future_event", "opaque": true}),
    )
    .expect("wire event");
    let (coordinator, store, provider) = coordinator(vec![
        Script::ObservedStream {
            account_id: "acct_first",
            items: vec![
                Ok(ProviderEvent::wire(unknown)),
                Err(ProviderError::new(
                    ProviderErrorKind::Unavailable,
                    UpstreamSendState::Sent,
                )),
            ],
        },
        Script::Stream {
            account_id: "acct_second",
            items: complete_stream(None),
        },
    ]);
    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        None,
        None,
        CancellationToken::new(),
    ))
    .expect("start execution");

    block_on(session.collect_uncommitted()).expect("retry response");
    block_on(session.commit_downstream(Some(200))).expect("commit winning response");

    assert_eq!(provider.contexts.lock().expect("contexts lock").len(), 2);
    assert!(provider.scripts.lock().expect("scripts lock").is_empty());
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.intermediate_failures, 1);
    assert_eq!(state.finalizations[0].outcome, ExecutionOutcome::Succeeded);
}

#[test]
fn discarded_attempt_observation_does_not_leak_into_retry_result() {
    let operation = operation(RetrySafety::Idempotent);
    let route_plan = plan(&operation, 1);
    let first_observation = ProviderResponseObservation::new(
        UpstreamTransport::new("websocket").expect("first transport"),
    )
    .with_http_version(UpstreamHttpVersion::Http11)
    .with_status_code(503)
    .with_request_id(SafeUpstreamValue::new("discarded-request").expect("request id"))
    .with_timings(ProviderResponseTimings {
        transport_decision_wait_ms: Some(7),
        connect_ms: Some(11),
        headers_ms: Some(13),
        first_event_ms: Some(987_654),
    });
    let second_observation = ProviderResponseObservation::new(
        UpstreamTransport::new("http_sse").expect("second transport"),
    )
    .with_http_version(UpstreamHttpVersion::Http2)
    .with_status_code(200)
    .with_request_id(SafeUpstreamValue::new("winning-request").expect("request id"));
    let mut second_items = vec![Ok(ProviderEvent::observation(second_observation))];
    second_items.extend(
        complete_stream(None)
            .into_iter()
            .map(|event| event.map(ProviderEvent::canonical)),
    );
    let (coordinator, store, _) = coordinator(vec![
        Script::ObservedStream {
            account_id: "acct_first",
            items: vec![
                Ok(ProviderEvent::observation(first_observation)),
                Err(
                    ProviderError::new(ProviderErrorKind::Unavailable, UpstreamSendState::Sent)
                        .with_replay_safe(),
                ),
            ],
        },
        Script::ObservedStream {
            account_id: "acct_second",
            items: second_items,
        },
    ]);
    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        None,
        None,
        CancellationToken::new(),
    ))
    .expect("start execution");

    block_on(session.collect_uncommitted()).expect("retry response");
    block_on(session.commit_downstream(Some(200))).expect("commit response");

    let state = store.state.lock().expect("store lock");
    let finalization = &state.finalizations[0];
    assert_eq!(finalization.upstream_transport.as_deref(), Some("http_sse"));
    assert_eq!(finalization.http_version.as_deref(), Some("HTTP/2"));
    assert_eq!(
        finalization.upstream_request_id.as_deref(),
        Some("winning-request")
    );
    assert_eq!(finalization.upstream_status_code, Some(200));
    assert_eq!(finalization.transport_decision_wait_ms, None);
    assert_eq!(finalization.connect_ms, None);
    assert_eq!(finalization.headers_ms, None);
    assert!(
        finalization
            .first_event_ms
            .is_some_and(|elapsed| elapsed < 987_654)
    );
    assert_eq!(state.intermediate_status_codes, vec![Some(503)]);
    assert_eq!(
        state.intermediate_request_ids,
        vec![Some("discarded-request".to_owned())]
    );
}

#[test]
fn websocket_success_keeps_client_http_status_absent() {
    let operation = operation(RetrySafety::Idempotent);
    let route_plan = plan(&operation, 1);
    let (coordinator, store, _) = coordinator(vec![Script::Stream {
        account_id: "acct_one",
        items: complete_stream(None),
    }]);
    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        None,
        None,
        CancellationToken::new(),
    ))
    .expect("start execution");

    block_on(session.collect_uncommitted()).expect("collect response");
    block_on(session.commit_downstream(None)).expect("commit WebSocket response");

    let state = store.state.lock().expect("store lock");
    assert_eq!(state.committed_statuses, vec![None]);
    assert_eq!(state.finalizations[0].client_status_code, None);
}

#[test]
fn authenticated_native_continuation_reaches_every_attempt_context() {
    let operation = generate_operation();
    let route_plan = plan(&operation, 1);
    let (coordinator, _, provider) = coordinator(vec![Script::Stream {
        account_id: "acct_one",
        items: complete_stream(None),
    }]);
    let continuation = NativeContinuationPin::new(
        PreviousResponseId::new("previous-secret-id").expect("previous response"),
        SafeUpstreamValue::new("provider-native-id").expect("upstream response"),
        ProviderKind::new("openai").expect("provider"),
        ProviderInstanceId::new("inst_openai_1").expect("instance"),
        ProviderAccountId::new("acct_one").expect("account"),
    );

    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        None,
        Some(ContinuationBinding::Pinned(continuation)),
        CancellationToken::new(),
    ))
    .expect("start execution");
    let _events = block_on(session.collect_uncommitted()).expect("collect response");
    block_on(session.commit_downstream(Some(200))).expect("commit response");

    let contexts = provider.contexts.lock().expect("contexts lock");
    assert_eq!(
        contexts[0]
            .continuation()
            .expect("continuation")
            .pinned()
            .expect("pinned continuation")
            .previous_response_id()
            .as_str(),
        "previous-secret-id"
    );
    assert_eq!(
        contexts[0]
            .continuation()
            .expect("continuation")
            .pinned()
            .expect("pinned continuation")
            .upstream_response_id()
            .as_str(),
        "provider-native-id"
    );
    let owner = contexts[0]
        .account_state_owner()
        .expect("continuation account-state owner");
    assert!(owner.matches(
        &ProviderKind::new("openai").expect("provider"),
        &ProviderInstanceId::new("inst_openai_1").expect("instance"),
        &ProviderAccountId::new("acct_one").expect("account"),
    ));
}

#[test]
fn native_continuation_replays_owner_before_safely_switching_account() {
    let Operation::Generate(generate) = generate_operation() else {
        panic!("generate operation");
    };
    let mut payload = Map::new();
    payload.insert("transcript".to_owned(), Value::Array(Vec::new()));
    let operation = Operation::Generate(generate.with_provider_session_state(
        ProviderSessionState::new("openai", payload).expect("provider session state"),
    ));
    let route_plan = plan(&operation, 1);
    let (coordinator, _, provider) = coordinator(vec![
        Script::Stream {
            account_id: "acct_one",
            items: vec![
                Ok(GatewayEvent::Started(ResponseMeta::new(
                    "response-native",
                    "gpt-5",
                ))),
                Err(
                    ProviderError::new(ProviderErrorKind::Unavailable, UpstreamSendState::Sent)
                        .with_continuation_failure(ContinuationFailure::HistoryUnavailable)
                        .with_replay_safe(),
                ),
            ],
        },
        Script::Stream {
            account_id: "acct_one",
            items: vec![Err(ProviderError::new(
                ProviderErrorKind::Transport,
                UpstreamSendState::NotSent,
            )
            .with_replay_safe())],
        },
        Script::Stream {
            account_id: "acct_two",
            items: complete_stream(None),
        },
    ]);
    let continuation = NativeContinuationPin::new(
        PreviousResponseId::new("previous-secret-id").expect("previous response"),
        SafeUpstreamValue::new("provider-native-id").expect("upstream response"),
        ProviderKind::new("openai").expect("provider"),
        ProviderInstanceId::new("inst_openai_1").expect("instance"),
        ProviderAccountId::new("acct_one").expect("account"),
    );

    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        None,
        Some(ContinuationBinding::Pinned(continuation)),
        CancellationToken::new(),
    ))
    .expect("start execution");
    block_on(session.collect_uncommitted()).expect("portable replay succeeds");
    block_on(session.commit_downstream(Some(200))).expect("commit response");

    let contexts = provider.contexts.lock().expect("contexts lock");
    assert_eq!(contexts.len(), 3);
    assert_eq!(
        contexts[0].continuation_attempt(),
        ContinuationAttempt::Native
    );
    assert_eq!(
        contexts[1].continuation_attempt(),
        ContinuationAttempt::ReplayOwner
    );
    assert_eq!(
        contexts[2].continuation_attempt(),
        ContinuationAttempt::ReplayAny
    );
    assert!(
        contexts[2]
            .excluded_accounts()
            .contains(&ProviderAccountId::new("acct_one").expect("account"))
    );
}

#[test]
fn required_account_reaches_provider_and_matching_metadata_succeeds() {
    let operation = operation(RetrySafety::Idempotent);
    let route_plan = plan(&operation, 2);
    let (coordinator, _, provider) = coordinator(vec![Script::Stream {
        account_id: "acct_required",
        items: complete_stream(None),
    }]);
    let required = ProviderAccountId::new("acct_required").expect("account id");
    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        Some(required.clone()),
        None,
        CancellationToken::new(),
    ))
    .expect("start execution");
    block_on(session.collect_uncommitted()).expect("required account succeeds");
    block_on(session.commit_downstream(Some(200))).expect("commit response");

    let contexts = provider.contexts.lock().expect("contexts lock");
    assert_eq!(contexts.len(), 1);
    assert_eq!(contexts[0].required_account(), Some(&required));
}

#[test]
fn provider_metadata_for_another_account_fails_closed() {
    let operation = operation(RetrySafety::Idempotent);
    let route_plan = plan(&operation, 2);
    let (coordinator, store, provider) = coordinator(vec![
        Script::Stream {
            account_id: "acct_wrong",
            items: complete_stream(None),
        },
        Script::Stream {
            account_id: "acct_required",
            items: complete_stream(None),
        },
    ]);
    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        Some(ProviderAccountId::new("acct_required").expect("account id")),
        None,
        CancellationToken::new(),
    ))
    .expect("start execution");
    let error = block_on(session.collect_uncommitted())
        .expect_err("different account metadata must fail closed");

    assert!(matches!(
        error,
        gateway_core::engine::EngineError::RequiredAccountMismatch
    ));
    assert_eq!(provider.contexts.lock().expect("contexts lock").len(), 1);
    assert_eq!(provider.scripts.lock().expect("scripts lock").len(), 1);
    let state = store.state.lock().expect("store lock");
    assert!(state.attempts.is_empty());
    assert_eq!(state.finalizations[0].outcome, ExecutionOutcome::Failed);
}

#[test]
fn required_account_disables_instance_fallback_before_stream() {
    let operation = operation(RetrySafety::Idempotent);
    let route_plan = plan(&operation, 2);
    let (coordinator, store, provider) = coordinator(vec![
        Script::Error(ProviderError::new(
            ProviderErrorKind::Unavailable,
            UpstreamSendState::NotSent,
        )),
        Script::Stream {
            account_id: "acct_required",
            items: complete_stream(None),
        },
    ]);
    let required = ProviderAccountId::new("acct_required").expect("account id");
    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        Some(required.clone()),
        None,
        CancellationToken::new(),
    ))
    .expect("start execution");
    let error = block_on(session.collect_uncommitted())
        .expect_err("required account must not switch targets");

    assert!(matches!(
        error,
        gateway_core::engine::EngineError::Provider(_)
    ));
    let contexts = provider.contexts.lock().expect("contexts lock");
    assert_eq!(contexts.len(), 1);
    assert_eq!(contexts[0].required_account(), Some(&required));
    assert_eq!(provider.scripts.lock().expect("scripts lock").len(), 1);
    let state = store.state.lock().expect("store lock");
    assert!(state.attempts.is_empty());
    assert_eq!(state.finalizations[0].outcome, ExecutionOutcome::Failed);
}

#[test]
fn local_selection_failure_falls_back_without_instance_failure_observation() {
    let operation = operation(RetrySafety::Idempotent);
    let route_plan = plan(&operation, 2);
    let (coordinator, store, provider) = coordinator(vec![
        Script::Error(ProviderError::new(
            ProviderErrorKind::Unavailable,
            UpstreamSendState::NotSent,
        )),
        Script::Stream {
            account_id: "acct_second",
            items: complete_stream(None),
        },
    ]);
    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        None,
        None,
        CancellationToken::new(),
    ))
    .expect("start execution");

    block_on(session.collect_uncommitted()).expect("fallback response");
    block_on(session.commit_downstream(Some(200))).expect("commit response");

    assert_eq!(provider.contexts.lock().expect("contexts lock").len(), 2);
    assert_eq!(
        session.provider_attempt_outcomes(),
        &[ProviderAttemptOutcome::Succeeded {
            provider_instance_id: ProviderInstanceId::new("inst_openai_2")
                .expect("second instance"),
        }]
    );
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.attempts.len(), 1);
    assert_eq!(
        state.attempts[0].provider_instance_id,
        ProviderInstanceId::new("inst_openai_2").expect("second instance")
    );
}

#[test]
fn required_account_disables_account_retry_after_stream_creation() {
    let operation = operation(RetrySafety::Idempotent);
    let route_plan = plan(&operation, 1);
    let (coordinator, store, provider) = coordinator(vec![
        Script::Stream {
            account_id: "acct_required",
            items: vec![Err(ProviderError::new(
                ProviderErrorKind::Unavailable,
                UpstreamSendState::NotSent,
            ))],
        },
        Script::Stream {
            account_id: "acct_other",
            items: complete_stream(None),
        },
    ]);
    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        Some(ProviderAccountId::new("acct_required").expect("account id")),
        None,
        CancellationToken::new(),
    ))
    .expect("start execution");
    let error = block_on(session.collect_uncommitted())
        .expect_err("required account must not rotate after failure");

    assert!(matches!(
        error,
        gateway_core::engine::EngineError::Provider(_)
    ));
    assert_eq!(
        session.provider_attempt_outcomes(),
        &[ProviderAttemptOutcome::Failed {
            provider_instance_id: ProviderInstanceId::new("inst_openai_1").expect("instance"),
            error_kind: ProviderErrorKind::Unavailable,
        }]
    );
    assert_eq!(provider.contexts.lock().expect("contexts lock").len(), 1);
    assert_eq!(provider.scripts.lock().expect("scripts lock").len(), 1);
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.attempts.len(), 1);
    assert_eq!(state.intermediate_failures, 0);
    assert_eq!(state.finalizations[0].outcome, ExecutionOutcome::Failed);
}

#[test]
fn latest_provider_reported_cost_is_persisted_as_known_usd_total() {
    let operation = operation(RetrySafety::Idempotent);
    let route_plan = plan(&operation, 1);
    let events = vec![
        Ok(GatewayEvent::Started(ResponseMeta::new(
            "native-cost",
            "gpt-5",
        ))),
        Ok(GatewayEvent::ProviderCost(
            ProviderReportedCost::from_usd_ticks(10).expect("first cost"),
        )),
        Ok(GatewayEvent::ProviderCost(
            ProviderReportedCost::from_usd_ticks(25).expect("latest cost"),
        )),
        Ok(GatewayEvent::Completed(ResponseMeta::new(
            "native-cost",
            "gpt-5",
        ))),
    ];
    let (coordinator, store, _) = coordinator(vec![Script::Stream {
        account_id: "acct_one",
        items: events,
    }]);
    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        None,
        None,
        CancellationToken::new(),
    ))
    .expect("start execution");
    block_on(session.collect_uncommitted()).expect("collect response");
    block_on(session.commit_downstream(Some(200))).expect("commit response");
    let state = store.state.lock().expect("store lock");
    assert_eq!(
        state.finalizations[0].cost_source,
        CostSource::ProviderReported
    );
    assert_eq!(state.finalizations[0].cost_ticks, Some(25));
}

#[test]
fn calculated_cost_is_persisted_when_provider_does_not_report_cost() {
    let operation = operation(RetrySafety::Idempotent);
    let route_plan = plan(&operation, 1);
    let events = vec![
        Ok(GatewayEvent::Started(ResponseMeta::new(
            "calculated-cost",
            "grok-4.5",
        ))),
        Ok(GatewayEvent::CalculatedCost(
            CalculatedCost::from_usd_ticks(123).expect("calculated cost"),
        )),
        Ok(GatewayEvent::Completed(ResponseMeta::new(
            "calculated-cost",
            "grok-4.5",
        ))),
    ];
    let (coordinator, store, _) = coordinator(vec![Script::Stream {
        account_id: "acct_one",
        items: events,
    }]);
    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        None,
        None,
        CancellationToken::new(),
    ))
    .expect("start execution");
    block_on(session.collect_uncommitted()).expect("collect response");
    block_on(session.commit_downstream(Some(200))).expect("commit response");
    let state = store.state.lock().expect("store lock");

    assert_eq!(state.finalizations[0].cost_source, CostSource::Calculated);
    assert_eq!(state.finalizations[0].cost_ticks, Some(123));
}

#[test]
fn provider_reported_cost_should_not_be_replaced_by_calculated_cost() {
    let operation = operation(RetrySafety::Idempotent);
    let route_plan = plan(&operation, 1);
    let events = vec![
        Ok(GatewayEvent::Started(ResponseMeta::new(
            "reported-cost",
            "grok-4.5",
        ))),
        Ok(GatewayEvent::CalculatedCost(
            CalculatedCost::from_usd_ticks(10).expect("first calculated cost"),
        )),
        Ok(GatewayEvent::ProviderCost(
            ProviderReportedCost::from_usd_ticks(25).expect("provider cost"),
        )),
        Ok(GatewayEvent::CalculatedCost(
            CalculatedCost::from_usd_ticks(999).expect("later calculated cost"),
        )),
        Ok(GatewayEvent::Completed(ResponseMeta::new(
            "reported-cost",
            "grok-4.5",
        ))),
    ];
    let (coordinator, store, _) = coordinator(vec![Script::Stream {
        account_id: "acct_one",
        items: events,
    }]);
    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        None,
        None,
        CancellationToken::new(),
    ))
    .expect("start execution");
    block_on(session.collect_uncommitted()).expect("collect response");
    block_on(session.commit_downstream(Some(200))).expect("commit response");
    let state = store.state.lock().expect("store lock");

    assert_eq!(
        (
            state.finalizations[0].cost_source,
            state.finalizations[0].cost_ticks
        ),
        (CostSource::ProviderReported, Some(25))
    );
}

#[test]
fn discarded_attempt_cost_never_leaks_into_retry_result() {
    let operation = operation(RetrySafety::Idempotent);
    let route_plan = plan(&operation, 1);
    let (coordinator, store, _) = coordinator(vec![
        Script::Stream {
            account_id: "acct_first",
            items: vec![
                Ok(GatewayEvent::Started(ResponseMeta::new(
                    "discarded",
                    "gpt-5",
                ))),
                Ok(GatewayEvent::CalculatedCost(
                    CalculatedCost::from_usd_ticks(888).expect("discarded calculated cost"),
                )),
                Ok(GatewayEvent::ProviderCost(
                    ProviderReportedCost::from_usd_ticks(999).expect("discarded cost"),
                )),
                Err(ProviderError::new(
                    ProviderErrorKind::Unavailable,
                    UpstreamSendState::Sent,
                )),
            ],
        },
        Script::Stream {
            account_id: "acct_second",
            items: complete_stream(None),
        },
    ]);
    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        None,
        None,
        CancellationToken::new(),
    ))
    .expect("start execution");
    block_on(session.collect_uncommitted()).expect("retry succeeds");
    block_on(session.commit_downstream(Some(200))).expect("commit response");
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.finalizations[0].cost_source, CostSource::Unavailable);
    assert_eq!(state.finalizations[0].cost_ticks, None);
}

#[test]
fn pre_commit_failure_excludes_account_and_retries_same_target() {
    let operation = operation(RetrySafety::Idempotent);
    let route_plan = plan(&operation, 1);
    let (coordinator, store, provider) = coordinator(vec![
        Script::Stream {
            account_id: "acct_first",
            items: vec![
                Ok(GatewayEvent::Started(ResponseMeta::new(
                    "discarded-response",
                    "gpt-5",
                ))),
                Err(ProviderError::new(
                    ProviderErrorKind::Unauthorized,
                    UpstreamSendState::Sent,
                )),
            ],
        },
        Script::Stream {
            account_id: "acct_second",
            items: complete_stream(None),
        },
    ]);

    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        None,
        None,
        CancellationToken::new(),
    ))
    .expect("start execution");
    let events = block_on(session.collect_uncommitted()).expect("second account succeeds");
    assert_eq!(events.len(), 2);
    block_on(session.commit_downstream(Some(200))).expect("commit response");

    let contexts = provider.contexts.lock().expect("contexts lock");
    assert_eq!(contexts.len(), 2);
    assert!(contexts[0].account_state_owner().is_none());
    assert!(
        contexts[1]
            .account_state_owner()
            .is_some_and(|owner| owner.matches(
                &ProviderKind::new("openai").expect("provider"),
                &ProviderInstanceId::new("inst_openai_1").expect("instance"),
                &ProviderAccountId::new("acct_first").expect("account"),
            ))
    );
    assert!(
        contexts[1]
            .excluded_accounts()
            .contains(&ProviderAccountId::new("acct_first").expect("account id"))
    );
    assert_eq!(
        contexts[1].account_selection_policy().strategy(),
        RotationStrategy::Smart
    );
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.created, 1);
    assert_eq!(state.attempts.len(), 2);
    assert_eq!(state.intermediate_failures, 1);
    assert_eq!(state.finalizations[0].outcome, ExecutionOutcome::Succeeded);
}

#[test]
fn recovered_credential_retries_the_same_account_exactly_once() {
    let operation = operation(RetrySafety::NonIdempotent);
    let route_plan = plan(&operation, 1);
    let (coordinator, store, provider) = coordinator(vec![
        Script::Stream {
            account_id: "acct_first",
            items: vec![Err(ProviderError::new(
                ProviderErrorKind::Unauthorized,
                UpstreamSendState::Sent,
            )
            .with_status(401)
            .with_replay_safe()
            .with_same_account_retry())],
        },
        Script::Stream {
            account_id: "acct_first",
            items: complete_stream(None),
        },
    ]);

    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        None,
        None,
        CancellationToken::new(),
    ))
    .expect("start execution");
    block_on(session.collect_uncommitted()).expect("recovered account succeeds");
    block_on(session.commit_downstream(Some(200))).expect("commit response");

    let contexts = provider.contexts.lock().expect("contexts lock");
    let account = ProviderAccountId::new("acct_first").expect("account");
    assert_eq!(contexts.len(), 2);
    assert_eq!(contexts[1].required_account(), Some(&account));
    assert!(!contexts[1].excluded_accounts().contains(&account));
    assert!(contexts[1].credential_recovery_attempted());
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.attempts.len(), 2);
    assert_eq!(state.intermediate_failures, 1);
    assert_eq!(state.finalizations[0].outcome, ExecutionOutcome::Succeeded);
}

#[test]
fn non_idempotent_explicit_429_rejection_rotates_account_before_output() {
    let operation = operation(RetrySafety::NonIdempotent);
    let route_plan = plan(&operation, 1);
    let (coordinator, store, provider) = coordinator(vec![
        Script::Stream {
            account_id: "acct_first",
            items: vec![Err(ProviderError::new(
                ProviderErrorKind::RateLimited,
                UpstreamSendState::Sent,
            )
            .with_status(429)
            .with_replay_safe())],
        },
        Script::Stream {
            account_id: "acct_second",
            items: complete_stream(None),
        },
    ]);

    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        None,
        None,
        CancellationToken::new(),
    ))
    .expect("start execution");
    block_on(session.collect_uncommitted()).expect("second account succeeds");
    block_on(session.commit_downstream(Some(200))).expect("commit response");

    let contexts = provider.contexts.lock().expect("contexts lock");
    assert_eq!(contexts.len(), 2);
    assert!(
        contexts[1]
            .excluded_accounts()
            .contains(&ProviderAccountId::new("acct_first").expect("account id"))
    );
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.attempts.len(), 2);
    assert_eq!(state.intermediate_failures, 1);
    assert_eq!(state.finalizations[0].outcome, ExecutionOutcome::Succeeded);
}

#[test]
fn non_idempotent_sent_failure_without_provider_proof_is_not_retried() {
    let (store, provider) = terminal_non_idempotent_failure(
        vec![Err(ProviderError::new(
            ProviderErrorKind::RateLimited,
            UpstreamSendState::Sent,
        ))],
        None,
    );
    assert_eq!(provider.contexts.lock().expect("contexts lock").len(), 1);
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.attempts.len(), 1);
    assert_eq!(state.intermediate_failures, 0);
}

#[test]
fn explicit_429_with_ambiguous_send_state_is_not_retried() {
    let (store, provider) = terminal_non_idempotent_failure(
        vec![Err(ProviderError::new(
            ProviderErrorKind::RateLimited,
            UpstreamSendState::Ambiguous,
        )
        .with_status(429)
        .with_replay_safe())],
        None,
    );
    assert_eq!(provider.contexts.lock().expect("contexts lock").len(), 1);
    let state = store.state.lock().expect("store lock");
    assert_eq!(
        state.finalizations[0].send_state,
        UpstreamSendState::Ambiguous
    );
    assert_eq!(state.intermediate_failures, 0);
}

#[test]
fn explicit_429_after_structural_event_should_retry_before_commit() {
    let operation = operation(RetrySafety::NonIdempotent);
    let route_plan = plan(&operation, 1);
    let (coordinator, store, provider) = coordinator(vec![
        Script::Stream {
            account_id: "acct_first",
            items: vec![
                Ok(GatewayEvent::Started(ResponseMeta::new(
                    "response-started",
                    "gpt-5",
                ))),
                Err(
                    ProviderError::new(ProviderErrorKind::RateLimited, UpstreamSendState::Sent)
                        .with_status(429)
                        .with_replay_safe(),
                ),
            ],
        },
        Script::Stream {
            account_id: "acct_second",
            items: complete_stream(None),
        },
    ]);
    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        None,
        None,
        CancellationToken::new(),
    ))
    .expect("start execution");

    block_on(session.collect_uncommitted()).expect("second account succeeds");
    block_on(session.commit_downstream(Some(200))).expect("commit response");

    assert_eq!(provider.contexts.lock().expect("contexts lock").len(), 2);
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.attempts.len(), 2);
    assert_eq!(state.intermediate_failures, 1);
    assert_eq!(state.finalizations[0].outcome, ExecutionOutcome::Succeeded);
}

#[test]
fn native_continuation_explicit_429_is_not_retried() {
    let continuation = NativeContinuationPin::new(
        PreviousResponseId::new("previous-secret-id").expect("previous response"),
        SafeUpstreamValue::new("provider-native-id").expect("upstream response"),
        ProviderKind::new("openai").expect("provider"),
        ProviderInstanceId::new("inst_openai_1").expect("instance"),
        ProviderAccountId::new("acct_first").expect("account"),
    );
    let (store, provider) = terminal_non_idempotent_failure(
        vec![Err(ProviderError::new(
            ProviderErrorKind::RateLimited,
            UpstreamSendState::Sent,
        )
        .with_status(429)
        .with_replay_safe())],
        Some(ContinuationBinding::Pinned(continuation)),
    );
    assert_eq!(provider.contexts.lock().expect("contexts lock").len(), 1);
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.attempts.len(), 1);
    assert_eq!(state.intermediate_failures, 0);
}

#[test]
fn external_continuation_explicit_429_is_not_retried() {
    let continuation = ContinuationBinding::External(
        PreviousResponseId::new("external-provider-response").expect("previous response"),
    );
    let (store, provider) = terminal_non_idempotent_failure(
        vec![Err(ProviderError::new(
            ProviderErrorKind::RateLimited,
            UpstreamSendState::Sent,
        )
        .with_status(429)
        .with_replay_safe())],
        Some(continuation),
    );
    assert_eq!(provider.contexts.lock().expect("contexts lock").len(), 1);
    assert!(
        provider.contexts.lock().expect("contexts lock")[0]
            .account_state_owner()
            .is_none()
    );
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.attempts.len(), 1);
    assert_eq!(state.intermediate_failures, 0);
}

#[test]
fn non_idempotent_not_sent_failure_can_fallback_target() {
    let operation = operation(RetrySafety::NonIdempotent);
    let route_plan = plan(&operation, 2);
    let (coordinator, store, provider) = coordinator(vec![
        Script::Error(ProviderError::new(
            ProviderErrorKind::Unavailable,
            UpstreamSendState::NotSent,
        )),
        Script::Stream {
            account_id: "acct_second",
            items: complete_stream(None),
        },
    ]);

    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        None,
        None,
        CancellationToken::new(),
    ))
    .expect("start execution");
    block_on(session.collect_uncommitted()).expect("safe instance fallback succeeds");
    block_on(session.commit_downstream(Some(200))).expect("commit response");

    let contexts = provider.contexts.lock().expect("contexts lock");
    assert_eq!(contexts.len(), 2);
    assert!(contexts[0].account_state_owner().is_none());
    assert!(contexts[1].account_state_owner().is_none());
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.attempts.len(), 1);
    assert_eq!(
        state.attempts[0].provider_instance_id,
        ProviderInstanceId::new("inst_openai_2").expect("second instance")
    );
}

#[test]
fn ambiguous_send_state_stops_retry() {
    let operation = operation(RetrySafety::Idempotent);
    let route_plan = plan(&operation, 1);
    let (coordinator, store, provider) = coordinator(vec![
        Script::Stream {
            account_id: "acct_first",
            items: vec![Err(ProviderError::new(
                ProviderErrorKind::Transport,
                UpstreamSendState::Ambiguous,
            ))],
        },
        Script::Stream {
            account_id: "acct_second",
            items: complete_stream(None),
        },
    ]);

    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        None,
        None,
        CancellationToken::new(),
    ))
    .expect("start execution");
    let error =
        block_on(session.collect_uncommitted()).expect_err("ambiguous send cannot be replayed");

    assert!(matches!(
        error,
        gateway_core::engine::EngineError::Provider(_)
    ));
    assert_eq!(provider.contexts.lock().expect("contexts lock").len(), 1);
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.attempts.len(), 1);
    assert_eq!(state.intermediate_failures, 0);
    let finalization = &state.finalizations[0];
    assert_eq!(finalization.outcome, ExecutionOutcome::Failed);
    assert_eq!(finalization.send_state, UpstreamSendState::Ambiguous);
    assert_eq!(finalization.attempt_count, 1);
    assert!(!finalization.committed);
    assert!(finalization.latency_ms.is_some());
}

#[test]
fn structural_event_before_replay_safe_failure_should_switch_account_before_commit() {
    let operation = operation(RetrySafety::Idempotent);
    let route_plan = plan(&operation, 1);
    let (coordinator, store, provider) = coordinator(vec![
        Script::Stream {
            account_id: "acct_first",
            items: vec![
                Ok(GatewayEvent::Started(ResponseMeta::new(
                    "response-1",
                    "gpt-5",
                ))),
                Err(
                    ProviderError::new(ProviderErrorKind::Transport, UpstreamSendState::Sent)
                        .with_replay_safe(),
                ),
            ],
        },
        Script::Stream {
            account_id: "acct_second",
            items: complete_stream(None),
        },
    ]);

    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        None,
        None,
        CancellationToken::new(),
    ))
    .expect("start execution");
    let first = block_on(session.next_event())
        .expect("first event")
        .expect("started event");
    assert_eq!(
        first.commit_requirement(),
        CommitRequirement::CommitBeforeDelivery
    );
    assert_eq!(first.into_provider_events().len(), 1);
    block_on(session.commit_downstream(Some(200))).expect("commit first event");
    block_on(session.next_event()).expect_err("committed stream failure must not be replayed");
    assert_eq!(
        session.provider_attempt_outcomes(),
        &[ProviderAttemptOutcome::Failed {
            provider_instance_id: ProviderInstanceId::new("inst_openai_1").expect("instance"),
            error_kind: ProviderErrorKind::Transport,
        }]
    );
    assert_eq!(provider.contexts.lock().expect("contexts lock").len(), 1);
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.commits, 1);
    assert_eq!(state.intermediate_failures, 0);
    assert_eq!(state.finalizations[0].outcome, ExecutionOutcome::Incomplete);
    assert!(state.finalizations[0].committed);
    assert_eq!(state.finalizations[0].client_status_code, Some(200));
}

#[test]
fn cancellation_before_pending_delivery_commit_reaches_terminal_state() {
    let operation = operation(RetrySafety::Idempotent);
    let route_plan = plan(&operation, 1);
    let (coordinator, store, _) = coordinator(vec![Script::Stream {
        account_id: "acct_first",
        items: complete_stream(None),
    }]);
    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        None,
        None,
        CancellationToken::new(),
    ))
    .expect("start execution");
    let first = block_on(session.next_event())
        .expect("first event")
        .expect("started event");
    assert_eq!(
        first.commit_requirement(),
        CommitRequirement::CommitBeforeDelivery
    );

    block_on(session.cancel_and_finalize()).expect("cancel finalization");

    assert!(session.is_finalized());
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.commits, 0);
    assert_eq!(state.finalizations[0].outcome, ExecutionOutcome::Cancelled);
    assert!(!state.finalizations[0].committed);
}

#[test]
fn local_unavailability_before_stream_does_not_create_attempt_or_instance_failure() {
    let operation = operation(RetrySafety::Idempotent);
    let route_plan = plan(&operation, 1);
    let (coordinator, store, _) = coordinator(vec![Script::Error(
        ProviderError::new(ProviderErrorKind::Unavailable, UpstreamSendState::NotSent)
            .with_upstream_code(SafeUpstreamValue::new("overloaded").expect("provider code"))
            .with_retry_after(Duration::from_millis(250)),
    )]);

    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        None,
        None,
        CancellationToken::new(),
    ))
    .expect("start execution");
    let error = block_on(session.collect_uncommitted())
        .expect_err("provider failed before returning metadata");

    assert!(matches!(
        error,
        gateway_core::engine::EngineError::Provider(_)
    ));
    assert!(session.provider_attempt_outcomes().is_empty());
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.created, 1);
    assert!(state.attempts.is_empty());
    assert_eq!(
        state.finalizations[0].provider_error_code.as_deref(),
        Some("overloaded")
    );
    assert_eq!(state.finalizations[0].retry_after_ms, Some(250));
}

#[test]
fn expired_deadline_finalizes_without_calling_provider() {
    let operation = operation(RetrySafety::Idempotent);
    let route_plan = plan(&operation, 1);
    let (coordinator, store, provider) = coordinator(vec![]);

    let error = match block_on(coordinator.start(
        model_request(&operation, SystemTime::UNIX_EPOCH),
        operation,
        route_plan,
        None,
        None,
        CancellationToken::new(),
    )) {
        Ok(_) => panic!("deadline already elapsed"),
        Err(error) => error,
    };

    assert!(matches!(error, gateway_core::engine::EngineError::Deadline));
    assert!(provider.contexts.lock().expect("contexts lock").is_empty());
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.created, 1);
    assert!(state.attempts.is_empty());
    assert_eq!(state.finalizations[0].attempt_count, 0);
}
