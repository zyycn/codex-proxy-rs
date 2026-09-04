//! 单行 `model_requests`、账号重试与下游提交屏障测试。

use std::collections::{BTreeSet, VecDeque};
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use futures::executor::block_on;

use gateway_core::account::{AccountSelectionPolicy, ProviderAccountId, RotationStrategy};
use gateway_core::engine::continuation::{
    ContinuationBinding, NativeContinuationPin, PreviousResponseId,
};
use gateway_core::engine::execution::gateway_error_from_engine;
use gateway_core::engine::provider::{
    Provider, ProviderCallMetadata, ProviderCatalogGeneration, ProviderModelCapabilities,
    ProviderRegistry, ProviderRequest, ProviderStream,
};
use gateway_core::engine::{
    AttemptContext, AttemptCoordinator, AttemptRecord, AttemptTransport, CommitRequirement,
    ContinuationAttempt, EngineError, ExecutionOutcome, ExecutionStore, GatewayEngine,
    IntermediateFailure, ModelRequestFinalization, ModelRequestId, NewModelRequest,
    ProviderAttemptOutcome, RecoveryReport,
};
use gateway_core::error::{
    ClientVisibleUpstreamError, ClientVisibleUpstreamResponse, ContinuationFailure,
    ContinuationRecoveryDisposition, GatewayErrorKind, OpaqueUpstreamValue, ProviderError,
    ProviderErrorKind, StoreError, StoreErrorKind,
};
use gateway_core::event::{
    ContentItem, ContentKind, GatewayEvent, ProtocolWireEvent, ProviderEvent,
    ProviderResponseObservation, ProviderResponseTimings, ResponseMeta, ToolCallDelta,
    UpstreamHttpVersion, WebSocketPoolKind,
};
use gateway_core::lifecycle::CancellationToken;
use gateway_core::metering::{CalculatedCost, CostSource, ProviderReportedCost, Usage};
use gateway_core::operation::{GenerateRequest, Operation, ProtocolPayload, ProviderSessionState};
use gateway_core::policy::ClientApiKeyId;
use gateway_core::routing::{
    AccountRoutingSnapshot, ClientRoutingScope, ConfigRevision, FrozenAccountScope,
    ModelCapabilities, ProviderKind, ProviderModel, PublicModelId, RoutingContext, RoutingPlan,
    RuntimeAccount, RuntimeAccountDirectory, RuntimeSnapshot, UpstreamModelId,
};
use gateway_core::upstream::{UpstreamSendState, UpstreamTransport};
use serde_json::{Map, Value, json};

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
    service_tier: Option<String>,
    upstream_request_id: Option<String>,
    upstream_status_code: Option<u16>,
    transport_decision_wait_ms: Option<u64>,
    connect_ms: Option<u64>,
    headers_ms: Option<u64>,
    first_event_ms: Option<u64>,
    first_reasoning_ms: Option<u64>,
    first_text_ms: Option<u64>,
    first_token_ms: Option<u64>,
    provider_processing_ms: Option<u64>,
    provider_metadata_json: Option<String>,
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
    failures: BTreeSet<StoreWriteFailure>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum StoreWriteFailure {
    Create,
    Commit,
    Finalize,
}

impl FakeStore {
    fn failing(failure: StoreWriteFailure) -> Self {
        Self {
            state: Mutex::new(StoreState::default()),
            failures: BTreeSet::from([failure]),
        }
    }

    fn fails(&self, failure: StoreWriteFailure) -> Result<(), StoreError> {
        if self.failures.contains(&failure) {
            Err(StoreError::new(StoreErrorKind::Unavailable))
        } else {
            Ok(())
        }
    }
}

#[async_trait]
impl ExecutionStore for FakeStore {
    async fn create_model_request(&self, _request: NewModelRequest) -> Result<(), StoreError> {
        self.fails(StoreWriteFailure::Create)?;
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
        self.fails(StoreWriteFailure::Commit)?;
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
        self.fails(StoreWriteFailure::Finalize)?;
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
                service_tier: finalization.service_tier,
                upstream_request_id: finalization.upstream_request_id,
                upstream_status_code: finalization.upstream_status_code,
                transport_decision_wait_ms: finalization.timings.transport_decision_wait_ms,
                connect_ms: finalization.timings.connect_ms,
                headers_ms: finalization.timings.headers_ms,
                first_event_ms: finalization.timings.first_event_ms,
                first_reasoning_ms: finalization.timings.first_reasoning_ms,
                first_text_ms: finalization.timings.first_text_ms,
                first_token_ms: finalization.timings.first_token_ms,
                provider_processing_ms: finalization.timings.provider_processing_ms,
                provider_metadata_json: finalization.provider_metadata_json,
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
    /// 产出 `items` 后永久悬挂的流；用于逼出会话级 deadline。
    HangingStream {
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
    operations: Mutex<Vec<Operation>>,
}

impl ScriptedProvider {
    fn new(scripts: Vec<Script>) -> Self {
        Self {
            scripts: Mutex::new(scripts.into()),
            contexts: Mutex::new(Vec::new()),
            operations: Mutex::new(Vec::new()),
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
    ) -> Result<Vec<ProviderModelCapabilities>, ProviderError> {
        Ok(Vec::new())
    }

    async fn execute(
        &self,
        request: ProviderRequest,
        context: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        self.operations
            .lock()
            .expect("operations lock")
            .push(request.operation().clone());
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
                    candidate
                        .upstream_model()
                        .cloned()
                        .expect("model route candidate"),
                    ProviderAccountId::new(account_id).expect("account id"),
                    UpstreamTransport::new("http_sse").expect("transport"),
                )
                .with_upstream_request_id(OpaqueUpstreamValue::new("upstream-request"));
                Ok(ProviderStream::new(
                    metadata,
                    Box::pin(futures::stream::iter(
                        items.into_iter().map(canonical_provider_event),
                    )),
                    (),
                ))
            }
            Script::HangingStream { account_id, items } => {
                let candidate = request.candidate();
                let metadata = ProviderCallMetadata::new(
                    candidate.provider().clone(),
                    candidate
                        .upstream_model()
                        .cloned()
                        .expect("model route candidate"),
                    ProviderAccountId::new(account_id).expect("account id"),
                    UpstreamTransport::new("http_sse").expect("transport"),
                );
                Ok(ProviderStream::new(
                    metadata,
                    Box::pin(
                        futures::stream::iter(items.into_iter().map(canonical_provider_event))
                            .chain(futures::stream::pending()),
                    ),
                    (),
                ))
            }
            Script::ObservedStream { account_id, items } => {
                let candidate = request.candidate();
                let metadata = ProviderCallMetadata::new(
                    candidate.provider().clone(),
                    candidate
                        .upstream_model()
                        .cloned()
                        .expect("model route candidate"),
                    ProviderAccountId::new(account_id).expect("account id"),
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

fn generate_operation() -> Operation {
    let body = json!({
        "model": "gpt-5",
        "input": [{"type": "message", "role": "user", "content": "hello"}],
    });
    Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", body.as_object().expect("request object").clone())
            .expect("OpenAI payload"),
    ))
}

fn image_generate_operation() -> Operation {
    let body = json!({
        "model": "gpt-5",
        "input": [{"type": "message", "role": "user", "content": "draw"}],
        "tools": [{"type": "image_generation"}],
    });
    Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", body.as_object().expect("request object").clone())
            .expect("OpenAI payload"),
    ))
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
    events.push(Ok(GatewayEvent::Completed(ResponseMeta::new(
        "response-1",
        "gpt-5",
    ))));
    events
}

fn canonical_provider_event(
    event: Result<GatewayEvent, ProviderError>,
) -> Result<ProviderEvent, ProviderError> {
    event.map(ProviderEvent::canonical)
}

fn atomic_response_failed(response_id: &str, marker: &str) -> ProviderError {
    let started = ProviderEvent::canonical_with_wire(
        vec![GatewayEvent::Started(ResponseMeta::new(
            response_id,
            "gpt-5",
        ))],
        ProtocolWireEvent::json(
            "openai",
            Some("response.created".to_owned()),
            json!({
                "type": "response.created",
                "response": {"id": response_id, "model": "gpt-5", "status": "in_progress"}
            }),
        )
        .expect("started wire"),
    );
    let failed = ProviderEvent::wire(
        ProtocolWireEvent::json(
            "openai",
            Some("response.failed".to_owned()),
            json!({
                "type": "response.failed",
                "response": {
                    "id": response_id,
                    "status": "failed",
                    "error": {"code": "rate_limit_exceeded", "message": marker}
                }
            }),
        )
        .expect("failed wire"),
    );
    ProviderError::new(ProviderErrorKind::RateLimited, UpstreamSendState::Sent)
        .with_status(429)
        .with_replay_safe()
        .with_atomic_client_events(vec![started, failed])
}

fn bare_atomic_response_failed(response_id: &str) -> ProviderError {
    let failed = ProviderEvent::canonical_with_wire(
        vec![GatewayEvent::Started(ResponseMeta::new(
            response_id,
            "fallback-model",
        ))],
        ProtocolWireEvent::json(
            "openai",
            Some("response.failed".to_owned()),
            json!({
                "type": "response.failed",
                "response": {
                    "id": response_id,
                    "status": "failed",
                    "error": {"code": "rate_limit_exceeded", "message": "bare failure"}
                }
            }),
        )
        .expect("bare failed wire"),
    );
    ProviderError::new(ProviderErrorKind::RateLimited, UpstreamSendState::Sent)
        .with_status(429)
        .with_replay_safe()
        .with_atomic_client_events(vec![failed])
}

fn atomic_history_unavailable(response_id: &str) -> ProviderError {
    let failed = ProviderEvent::wire(
        ProtocolWireEvent::json(
            "openai",
            Some("response.failed".to_owned()),
            json!({
                "type": "response.failed",
                "response": {
                    "id": response_id,
                    "status": "failed",
                    "error": {
                        "code": "previous_response_not_found",
                        "message": "Previous response was not found. Retrying the full request."
                    }
                }
            }),
        )
        .expect("history unavailable wire"),
    );
    ProviderError::new(
        ProviderErrorKind::ContinuationRecoveryRequired,
        UpstreamSendState::Sent,
    )
    .with_status(400)
    .with_continuation_failure(ContinuationFailure::HistoryUnavailable)
    .with_atomic_client_events(vec![failed])
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

fn plan(operation: &Operation) -> RoutingPlan {
    plan_with_policy(
        operation,
        AccountSelectionPolicy::new(
            RotationStrategy::Smart,
            NonZeroU32::new(2).expect("account concurrency"),
            Duration::from_millis(50),
        ),
    )
}

fn plan_with_policy(
    operation: &Operation,
    account_selection_policy: AccountSelectionPolicy,
) -> RoutingPlan {
    let provider = ProviderKind::new("openai").expect("provider");
    let public_model = PublicModelId::new("gpt-5").expect("public model");
    let capabilities = ModelCapabilities::new(BTreeSet::from([operation.kind()]), Some(32_000))
        .with_upstream_feature_validation();
    let directory = Arc::new(RuntimeAccountDirectory::new(
        [
            "acct_failed",
            "acct_first",
            "acct_image",
            "acct_observation_mismatch",
            "acct_observed",
            "acct_one",
            "acct_only",
            "acct_other",
            "acct_required",
            "acct_second",
            "acct_tool",
            "acct_two",
            "acct_wrong",
        ]
        .into_iter()
        .map(|id| {
            (
                ProviderAccountId::new(id).expect("account"),
                RuntimeAccount::new(provider.clone(), BTreeSet::new()),
            )
        })
        .collect(),
    ));
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).expect("config revision"),
        account_selection_policy,
        vec![provider.clone()],
        vec![ProviderModel::new(
            provider.clone(),
            UpstreamModelId::new("gpt-5").expect("upstream model"),
            capabilities,
        )],
        Vec::new(),
    )
    .expect("snapshot")
    .with_account_directory(Arc::clone(&directory));
    snapshot
        .plan(
            &public_model,
            operation,
            Arc::new(FrozenAccountScope::new(
                directory,
                ClientRoutingScope::all_accounts(),
            )),
            &RoutingContext {
                required_provider: Some(provider),
                ..RoutingContext::default()
            },
        )
        .expect("routing plan")
}

fn model_request(operation: &Operation, deadline: SystemTime) -> NewModelRequest {
    let client_key = ClientApiKeyId::new("key_client_1").expect("client key id");
    NewModelRequest {
        admission_decision_ms: None,
        id: ModelRequestId::new("req_core_1").expect("request id"),
        client_api_key_id: Some(client_key.clone()),
        client_api_key_ref: client_key,
        config_revision: ConfigRevision::new(1).expect("config revision"),
        routing: AccountRoutingSnapshot::all(),
        protocol: "openai".to_owned(),
        operation: operation.kind(),
        endpoint: "responses".to_owned(),
        client_transport: "http_sse".to_owned(),
        requested_model: Some(PublicModelId::new("gpt-5").expect("model")),
        client_ip: Some("127.0.0.1".parse().expect("client IP")),
        user_agent: Some("gateway-core-test".to_owned()),
        reasoning_effort: Some("medium".to_owned()),
        reasoning_preset: Some("balanced".to_owned()),
        request_kind: Some("responses".to_owned()),
        subagent_kind: None,
        compact: false,
        continuation: Default::default(),
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
    coordinator_with_store(scripts, store)
}

fn coordinator_with_store(
    scripts: Vec<Script>,
    store: Arc<FakeStore>,
) -> (
    AttemptCoordinator<FakeStore>,
    Arc<FakeStore>,
    Arc<ScriptedProvider>,
) {
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
    let operation = generate_operation();
    let route_plan = plan(&operation);
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
    let operation = generate_operation();
    let route_plan = plan(&operation);
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
    assert_eq!(started_id, "response-1");
    assert_eq!(started_id, completed_id);
    assert!(!session.is_finalized());
    block_on(session.commit_downstream(Some(200))).expect("commit response");

    assert!(session.is_finalized());
    assert_eq!(
        session.provider_attempt_outcomes(),
        &[ProviderAttemptOutcome::Succeeded {
            provider_kind: ProviderKind::new("openai").expect("provider"),
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

#[test]
fn observation_store_failures_never_block_successful_delivery() {
    for failure in [
        StoreWriteFailure::Create,
        StoreWriteFailure::Commit,
        StoreWriteFailure::Finalize,
    ] {
        let operation = generate_operation();
        let route_plan = plan(&operation);
        let store = Arc::new(FakeStore::failing(failure));
        let (coordinator, _, provider) = coordinator_with_store(
            vec![Script::Stream {
                account_id: "acct_one",
                items: complete_stream(None),
            }],
            store,
        );
        let mut session = block_on(coordinator.start(
            model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
            operation,
            route_plan,
            None,
            None,
            CancellationToken::new(),
        ))
        .expect("start execution");

        let events = block_on(session.collect_uncommitted())
            .unwrap_or_else(|error| panic!("{failure:?} blocked collection: {error}"));
        assert_eq!(events.len(), 2);
        block_on(session.commit_downstream(Some(200)))
            .unwrap_or_else(|error| panic!("{failure:?} blocked commit: {error}"));
        assert!(session.is_finalized());
        assert_eq!(provider.contexts.lock().expect("contexts lock").len(), 1);
    }
}

#[test]
fn terminal_provider_error_preserves_request_local_upstream_response() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let raw_body = Bytes::from_static(b"{\"upstream\":\"unchanged\"}\x00");
    let provider_error =
        ProviderError::new(ProviderErrorKind::InvalidRequest, UpstreamSendState::Sent)
            .with_client_visible_upstream_response(ClientVisibleUpstreamResponse::new(
                422,
                Some(b"application/problem+json".to_vec()),
                raw_body.clone(),
            ));
    let (coordinator, _, _) = coordinator(vec![Script::Error(provider_error)]);
    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        None,
        None,
        CancellationToken::new(),
    ))
    .expect("start execution");

    let error = block_on(session.collect_uncommitted()).expect_err("provider rejection");
    let EngineError::Provider(error) = error else {
        panic!("expected provider error");
    };
    let response = error
        .client_visible_upstream_response()
        .expect("request-local upstream response");

    assert_eq!(response.status(), 422);
    assert_eq!(response.body(), &raw_body);
}

#[test]
fn success_preserves_opaque_upstream_response_id_and_builds_native_pin() {
    let response_id = format!("resp_{}\0opaque", "x".repeat(4_096));
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let (coordinator, store, _) = coordinator(vec![Script::Stream {
        account_id: "acct_one",
        items: vec![
            Ok(GatewayEvent::Started(ResponseMeta::new(
                response_id.clone(),
                "gpt-5",
            ))),
            Ok(GatewayEvent::Completed(ResponseMeta::new(
                response_id.clone(),
                "gpt-5",
            ))),
        ],
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

    block_on(session.collect_uncommitted()).expect("collect opaque response");
    let provider_state = ProviderSessionState::new("openai", Map::new()).expect("provider state");
    let pin = session
        .native_continuation_pin(&provider_state)
        .expect("native continuation pin");
    assert_eq!(pin.previous_response_id().as_str(), response_id);
    assert_eq!(pin.upstream_response_id().as_str(), response_id);

    block_on(session.commit_downstream(Some(200))).expect("commit response");
    let state = store.state.lock().expect("store lock");
    assert_eq!(
        state.finalizations[0].client_response_id,
        Some(response_id.clone())
    );
    assert_eq!(
        state.finalizations[0].upstream_response_id,
        Some(response_id)
    );
}

#[test]
fn canonical_identity_change_and_empty_terminal_id_are_observational() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let (coordinator, store, _) = coordinator(vec![Script::Stream {
        account_id: "acct_one",
        items: vec![
            Ok(GatewayEvent::Started(ResponseMeta::new(
                "response-created",
                "gpt-5",
            ))),
            Ok(GatewayEvent::Completed(ResponseMeta::new("", "gpt-5"))),
        ],
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

    block_on(session.collect_uncommitted()).expect("identity observation cannot fail delivery");
    let provider_state = ProviderSessionState::new("openai", Map::new()).expect("provider state");
    let pin = session
        .native_continuation_pin(&provider_state)
        .expect("empty opaque response ID remains recordable");
    assert_eq!(pin.previous_response_id().as_str(), "");
    assert_eq!(pin.upstream_response_id().as_str(), "");

    block_on(session.commit_downstream(Some(200))).expect("commit response");
    let state = store.state.lock().expect("store lock");
    assert_eq!(
        state.finalizations[0].client_response_id.as_deref(),
        Some("")
    );
    assert_eq!(
        state.finalizations[0].upstream_response_id.as_deref(),
        Some("")
    );
}

fn finalized_image_request(image_output_tokens: Option<u64>) -> FinalState {
    let operation = image_generate_operation();
    let route_plan = plan(&operation);
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
    let route_plan = plan(&operation);
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
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let observation = ProviderResponseObservation::new(
        UpstreamTransport::new("http_sse").expect("actual transport"),
    )
    .with_http_version(UpstreamHttpVersion::Http2)
    .with_websocket_pool(WebSocketPoolKind::New)
    .with_status_code(200)
    .with_request_id(OpaqueUpstreamValue::new("upstream-observed"))
    .try_with_service_tier("priority")
    .expect("service tier")
    .with_timings(ProviderResponseTimings {
        transport_decision_wait_ms: Some(7),
        connect_ms: Some(11),
        headers_ms: Some(13),
        first_event_ms: Some(17),
        ..ProviderResponseTimings::default()
    });
    let mut items = vec![Ok(ProviderEvent::observation(observation))];
    items.extend(
        complete_stream(Some(21))
            .into_iter()
            .map(canonical_provider_event),
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
    assert_eq!(finalization.service_tier.as_deref(), Some("priority"));
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
fn mismatched_response_observation_should_not_interrupt_client_events() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let first = ProviderEvent::observation(ProviderResponseObservation::new(
        UpstreamTransport::new("websocket").expect("first transport"),
    ));
    let mismatched = ProviderEvent::observation(ProviderResponseObservation::new(
        UpstreamTransport::new("http_sse").expect("mismatched transport"),
    ));
    let mut items = vec![Ok(first), Ok(mismatched)];
    items.extend(
        complete_stream(None)
            .into_iter()
            .map(canonical_provider_event),
    );
    let (coordinator, store, _) = coordinator(vec![Script::ObservedStream {
        account_id: "acct_observation_mismatch",
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

    let events = block_on(session.collect_uncommitted()).expect("client events survive");
    block_on(session.commit_downstream(Some(200))).expect("commit response");

    assert_eq!(events.len(), 2);
    assert_eq!(
        store.state.lock().expect("store lock").finalizations[0]
            .upstream_transport
            .as_deref(),
        Some("websocket")
    );
}

#[test]
fn response_observation_should_persist_provider_metadata_and_output_timings() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let metadata = gateway_core::event::ProviderResponseMetadata::new(
        serde_json::json!({"effectiveModel": "gpt-test", "firstTokenMs": 23}).to_string(),
    )
    .expect("provider metadata object");
    let observation = ProviderResponseObservation::new(
        UpstreamTransport::new("websocket").expect("actual transport"),
    )
    .with_timings(ProviderResponseTimings {
        first_event_ms: Some(11),
        first_reasoning_ms: Some(17),
        first_text_ms: Some(19),
        first_token_ms: Some(17),
        provider_processing_ms: Some(5),
        ..ProviderResponseTimings::default()
    })
    .with_provider_metadata(metadata);
    let mut items = vec![Ok(ProviderEvent::observation(observation))];
    items.extend(
        complete_stream(Some(21))
            .into_iter()
            .map(canonical_provider_event),
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

    block_on(session.collect_uncommitted()).expect("collect response");
    block_on(session.commit_downstream(Some(200))).expect("commit response");

    let state = store.state.lock().expect("store lock");
    let finalization = &state.finalizations[0];
    assert_eq!(finalization.first_event_ms, Some(11));
    assert_eq!(finalization.first_reasoning_ms, Some(17));
    assert_eq!(finalization.first_text_ms, Some(19));
    assert_eq!(finalization.first_token_ms, Some(17));
    assert_eq!(finalization.provider_processing_ms, Some(5));
    assert_eq!(
        finalization.provider_metadata_json.as_deref(),
        Some("{\"effectiveModel\":\"gpt-test\",\"firstTokenMs\":23}")
    );
}

#[test]
fn empty_tool_call_delta_should_not_preempt_provider_first_token_timing() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let transport = UpstreamTransport::new("websocket").expect("actual transport");
    let initial_observation = ProviderResponseObservation::new(transport.clone());
    let output_observation =
        ProviderResponseObservation::new(transport).with_timings(ProviderResponseTimings {
            first_token_ms: Some(37),
            ..ProviderResponseTimings::default()
        });
    let items = vec![
        Ok(ProviderEvent::observation(initial_observation)),
        Ok(ProviderEvent::canonical(GatewayEvent::Started(
            ResponseMeta::new("response-tool", "gpt-5"),
        ))),
        Ok(ProviderEvent::canonical(GatewayEvent::ContentAdded(
            ContentItem::new(0, ContentKind::ToolCall),
        ))),
        Ok(ProviderEvent::canonical(GatewayEvent::ToolCallDelta(
            ToolCallDelta {
                content_index: 0,
                call_id: "call-tool".to_owned(),
                name: Some("apply_patch".to_owned()),
                arguments_delta: String::new(),
            },
        ))),
        Ok(ProviderEvent::observation(output_observation)),
        Ok(ProviderEvent::canonical(GatewayEvent::Completed(
            ResponseMeta::new("response-tool", "gpt-5"),
        ))),
    ];
    let (coordinator, store, _) = coordinator(vec![Script::ObservedStream {
        account_id: "acct_tool",
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

    block_on(session.collect_uncommitted()).expect("collect response");
    block_on(session.commit_downstream(Some(200))).expect("commit response");

    let state = store.state.lock().expect("store lock");
    assert_eq!(state.finalizations[0].first_token_ms, Some(37));
}

#[test]
fn unknown_wire_event_before_response_identity_is_discarded_with_retried_attempt() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
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
                Err(
                    ProviderError::new(ProviderErrorKind::Unavailable, UpstreamSendState::Sent)
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
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let first_observation = ProviderResponseObservation::new(
        UpstreamTransport::new("websocket").expect("first transport"),
    )
    .with_http_version(UpstreamHttpVersion::Http11)
    .with_status_code(503)
    .with_request_id(OpaqueUpstreamValue::new("discarded-request"))
    .with_timings(ProviderResponseTimings {
        transport_decision_wait_ms: Some(7),
        connect_ms: Some(11),
        headers_ms: Some(13),
        first_event_ms: Some(987_654),
        provider_processing_ms: Some(41),
        ..ProviderResponseTimings::default()
    });
    let second_observation = ProviderResponseObservation::new(
        UpstreamTransport::new("http_sse").expect("second transport"),
    )
    .with_http_version(UpstreamHttpVersion::Http2)
    .with_status_code(200)
    .with_request_id(OpaqueUpstreamValue::new("winning-request"));
    let mut second_items = vec![Ok(ProviderEvent::observation(second_observation))];
    second_items.extend(
        complete_stream(None)
            .into_iter()
            .map(canonical_provider_event),
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
    assert_eq!(finalization.provider_processing_ms, None);
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
    let operation = generate_operation();
    let route_plan = plan(&operation);
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
fn success_without_response_observation_persists_no_upstream_status() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
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
    block_on(session.commit_downstream(Some(200))).expect("commit response");

    let state = store.state.lock().expect("store lock");
    assert_eq!(state.finalizations[0].outcome, ExecutionOutcome::Succeeded);
    assert_eq!(state.finalizations[0].upstream_status_code, None);
}

#[test]
fn missing_attempt_is_classified_internal_not_no_available_provider() {
    assert_eq!(
        gateway_error_from_engine(&EngineError::EmptyRoutingPlan).kind(),
        GatewayErrorKind::NoAvailableProvider
    );
    assert_eq!(
        gateway_error_from_engine(&EngineError::NoActiveAttempt).kind(),
        GatewayErrorKind::Internal
    );
}

#[test]
fn authenticated_native_continuation_reaches_every_attempt_context() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let (coordinator, _, provider) = coordinator(vec![Script::Stream {
        account_id: "acct_one",
        items: complete_stream(None),
    }]);
    let continuation = NativeContinuationPin::new(
        PreviousResponseId::new("previous-secret-id"),
        PreviousResponseId::new("provider-native-id"),
        ClientApiKeyId::new("key_client_1").expect("client key"),
        ProviderKind::new("openai").expect("provider"),
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
        &ProviderAccountId::new("acct_one").expect("account"),
    ));
}

#[test]
fn streaming_native_continuation_delivers_started_event_before_later_output() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let (coordinator, _, _) = coordinator(vec![Script::Stream {
        account_id: "acct_one",
        items: complete_stream(None),
    }]);
    let continuation = NativeContinuationPin::new(
        PreviousResponseId::new("previous-response"),
        PreviousResponseId::new("upstream-response"),
        ClientApiKeyId::new("key_client_1").expect("client key"),
        ProviderKind::new("openai").expect("provider"),
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

    let first = block_on(session.next_event())
        .expect("first event")
        .expect("started event");
    assert_eq!(
        first.commit_requirement(),
        CommitRequirement::CommitBeforeDelivery
    );
    let events = first.into_provider_events();
    assert!(matches!(
        events.as_slice(),
        [event]
            if matches!(event.canonical_facts(), [GatewayEvent::Started(_)])
    ));

    block_on(session.commit_downstream(Some(200))).expect("commit started event");
    while block_on(session.next_event())
        .expect("remaining event")
        .is_some()
    {}
}

#[test]
fn native_continuation_uses_provider_defined_owner_then_cross_account_recovery() {
    let Operation::Generate(generate) = generate_operation() else {
        panic!("generate operation");
    };
    let mut payload = Map::new();
    payload.insert("transcript".to_owned(), Value::Array(Vec::new()));
    let operation = Operation::Generate(generate.with_provider_session_state(
        ProviderSessionState::new("openai", payload).expect("provider session state"),
    ));
    let route_plan = plan(&operation);
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
                        .with_continuation_recovery_disposition(
                            ContinuationRecoveryDisposition::ProviderReplayAllowed,
                        )
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
        PreviousResponseId::new("previous-secret-id"),
        PreviousResponseId::new("provider-native-id"),
        ClientApiKeyId::new("key_client_1").expect("client key"),
        ProviderKind::new("openai").expect("provider"),
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
    block_on(session.collect_uncommitted()).expect("provider-defined recovery succeeds");
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
fn native_continuation_client_replay_required_is_terminal() {
    let Operation::Generate(generate) = generate_operation() else {
        panic!("generate operation");
    };
    let operation = Operation::Generate(generate.with_provider_session_state(
        ProviderSessionState::new("openai", Map::new()).expect("provider session state"),
    ));
    let route_plan = plan(&operation);
    let (coordinator, store, provider) = coordinator(vec![
        Script::Stream {
            account_id: "acct_one",
            items: vec![Err(ProviderError::new(
                ProviderErrorKind::ContinuationRecoveryRequired,
                UpstreamSendState::NotSent,
            )
            .with_continuation_failure(ContinuationFailure::HistoryUnavailable)
            .with_continuation_recovery_disposition(
                ContinuationRecoveryDisposition::ClientReplayRequired,
            ))],
        },
        Script::Stream {
            account_id: "acct_one",
            items: complete_stream(None),
        },
    ]);
    let continuation = NativeContinuationPin::new(
        PreviousResponseId::new("previous-secret-id"),
        PreviousResponseId::new("provider-native-id"),
        ClientApiKeyId::new("key_client_1").expect("client key"),
        ProviderKind::new("openai").expect("provider"),
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
    let error = block_on(session.collect_uncommitted())
        .expect_err("client replay requirement must stop proxy recovery");

    assert!(matches!(error, EngineError::Provider(_)));
    let contexts = provider.contexts.lock().expect("contexts lock");
    assert_eq!(contexts.len(), 1);
    assert_eq!(
        contexts[0].continuation_attempt(),
        ContinuationAttempt::Native
    );
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.intermediate_failures, 0);
    assert_eq!(state.finalizations[0].attempt_count, 1);
    assert_eq!(state.finalizations[0].outcome, ExecutionOutcome::Failed);
}

#[test]
fn native_continuation_exact_retry_keeps_scope_and_account() {
    let Operation::Generate(generate) = generate_operation() else {
        panic!("generate operation");
    };
    let operation = Operation::Generate(generate.with_provider_session_state(
        ProviderSessionState::new("openai", Map::new()).expect("provider session state"),
    ));
    let route_plan = plan(&operation);
    let (coordinator, _, provider) = coordinator(vec![
        Script::Stream {
            account_id: "acct_one",
            items: vec![Err(ProviderError::new(
                ProviderErrorKind::Unavailable,
                UpstreamSendState::NotSent,
            )
            .with_continuation_failure(ContinuationFailure::Busy)
            .with_continuation_recovery_disposition(
                ContinuationRecoveryDisposition::RetryExactConnection,
            ))],
        },
        Script::Stream {
            account_id: "acct_one",
            items: complete_stream(None),
        },
    ]);
    let account = ProviderAccountId::new("acct_one").expect("account");
    let continuation = NativeContinuationPin::new(
        PreviousResponseId::new("previous-secret-id"),
        PreviousResponseId::new("provider-native-id"),
        ClientApiKeyId::new("key_client_1").expect("client key"),
        ProviderKind::new("openai").expect("provider"),
        account.clone(),
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
    block_on(session.collect_uncommitted()).expect("exact continuation retry succeeds");
    block_on(session.commit_downstream(Some(200))).expect("commit response");

    let contexts = provider.contexts.lock().expect("contexts lock");
    assert_eq!(contexts.len(), 2);
    assert!(
        contexts
            .iter()
            .all(|context| context.continuation_attempt() == ContinuationAttempt::Native)
    );
    assert_eq!(contexts[1].required_account(), Some(&account));
}

#[test]
fn unavailable_native_continuation_uses_provider_recovery_with_the_configured_account_policy() {
    let Operation::Generate(generate) = generate_operation() else {
        panic!("generate operation");
    };
    let operation = Operation::Generate(generate.with_provider_session_state(
        ProviderSessionState::new("openai", Map::new()).expect("provider session state"),
    ));
    let policy = AccountSelectionPolicy::new(
        RotationStrategy::RoundRobin,
        NonZeroU32::new(2).expect("account concurrency"),
        Duration::from_millis(50),
    );
    let route_plan = plan_with_policy(&operation, policy);
    let (coordinator, store, provider) = coordinator(vec![
        Script::Error(ProviderError::new(
            ProviderErrorKind::NoEligibleAccount,
            UpstreamSendState::NotSent,
        )),
        Script::Stream {
            account_id: "acct_two",
            items: complete_stream(None),
        },
    ]);
    let original = ProviderAccountId::new("acct_one").expect("account");
    let continuation = NativeContinuationPin::new(
        PreviousResponseId::new("previous-response"),
        PreviousResponseId::new("upstream-response"),
        ClientApiKeyId::new("key_client_1").expect("client key"),
        ProviderKind::new("openai").expect("provider"),
        original.clone(),
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
    block_on(session.collect_uncommitted()).expect("provider-defined recovery succeeds");
    block_on(session.commit_downstream(Some(200))).expect("commit response");

    let contexts = provider.contexts.lock().expect("contexts lock");
    assert_eq!(contexts.len(), 2);
    assert_eq!(
        contexts[0].continuation_attempt(),
        ContinuationAttempt::Native
    );
    assert_eq!(
        contexts[1].continuation_attempt(),
        ContinuationAttempt::ReplayAny
    );
    assert_eq!(
        contexts[1].account_selection_policy().strategy(),
        RotationStrategy::RoundRobin
    );
    assert!(contexts[1].excluded_accounts().contains(&original));
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.attempts.len(), 1);
    assert_eq!(state.finalizations[0].outcome, ExecutionOutcome::Succeeded);
}

#[test]
fn continuation_history_failure_without_replay_proof_is_delivered_without_hidden_retry() {
    let Operation::Generate(generate) = generate_operation() else {
        panic!("generate operation");
    };
    let operation = Operation::Generate(generate.with_provider_session_state(
        ProviderSessionState::new("openai", Map::new()).expect("provider session state"),
    ));
    let route_plan = plan(&operation);
    let (coordinator, _, provider) = coordinator(vec![
        Script::ObservedStream {
            account_id: "acct_one",
            items: vec![Err(atomic_history_unavailable("response-history-missing"))],
        },
        Script::Stream {
            account_id: "acct_two",
            items: complete_stream(None),
        },
    ]);
    let continuation = NativeContinuationPin::new(
        PreviousResponseId::new("previous-response"),
        PreviousResponseId::new("upstream-response"),
        ClientApiKeyId::new("key_client_1").expect("client key"),
        ProviderKind::new("openai").expect("provider"),
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

    let terminal = block_on(session.next_event())
        .expect("history failure remains deliverable")
        .expect("terminal history failure batch");
    assert_eq!(
        terminal.commit_requirement(),
        CommitRequirement::CommitBeforeDelivery
    );
    let events = terminal.into_provider_events();
    let failure = events
        .iter()
        .filter_map(ProviderEvent::wire_event)
        .find(|wire| wire.event_type() == Some("response.failed"))
        .expect("raw response.failed event");
    assert_eq!(
        failure
            .data()
            .pointer("/response/error/code")
            .and_then(Value::as_str),
        Some("previous_response_not_found")
    );
    block_on(session.commit_downstream(Some(200))).expect("commit terminal history failure");
    assert!(matches!(
        block_on(session.next_event()),
        Err(EngineError::Provider(_))
    ));

    let contexts = provider.contexts.lock().expect("contexts lock");
    assert_eq!(contexts.len(), 1);
    assert_eq!(
        contexts[0].continuation_attempt(),
        ContinuationAttempt::Native
    );
}

#[test]
fn diagnostic_account_reaches_provider_and_matching_metadata_succeeds() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let (coordinator, _, provider) = coordinator(vec![Script::Stream {
        account_id: "acct_required",
        items: complete_stream(None),
    }]);
    let required = ProviderAccountId::new("acct_required").expect("account id");
    let mut session = block_on(coordinator.start_diagnostic(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        required.clone(),
        None,
        CancellationToken::new(),
    ))
    .expect("start execution");
    block_on(session.collect_uncommitted()).expect("required account succeeds");
    block_on(session.commit_downstream(Some(200))).expect("commit response");

    let contexts = provider.contexts.lock().expect("contexts lock");
    assert_eq!(contexts.len(), 1);
    assert_eq!(contexts[0].required_account(), Some(&required));
    assert!(contexts[0].is_diagnostic_required_account());
}

#[test]
fn provider_metadata_for_another_account_fails_closed() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
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
    assert!(state.finalizations.is_empty());
}

#[test]
fn required_account_disables_account_retry_after_stream_creation() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let (coordinator, store, provider) = coordinator(vec![
        Script::Stream {
            account_id: "acct_required",
            items: vec![Err(ProviderError::new(
                ProviderErrorKind::Unavailable,
                UpstreamSendState::NotSent,
            )
            .with_replay_safe())],
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
            provider_kind: ProviderKind::new("openai").expect("provider"),
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
    let operation = generate_operation();
    let route_plan = plan(&operation);
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
    let operation = generate_operation();
    let route_plan = plan(&operation);
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
    let operation = generate_operation();
    let route_plan = plan(&operation);
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
    let operation = generate_operation();
    let route_plan = plan(&operation);
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
                Err(
                    ProviderError::new(ProviderErrorKind::Unavailable, UpstreamSendState::Sent)
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
    block_on(session.collect_uncommitted()).expect("retry succeeds");
    block_on(session.commit_downstream(Some(200))).expect("commit response");
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.finalizations[0].cost_source, CostSource::Unavailable);
    assert_eq!(state.finalizations[0].cost_ticks, None);
}

#[test]
fn pre_commit_failure_excludes_account_and_retries_same_target() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let (coordinator, store, provider) = coordinator(vec![
        Script::Stream {
            account_id: "acct_first",
            items: vec![
                Ok(GatewayEvent::Started(ResponseMeta::new(
                    "discarded-response",
                    "gpt-5",
                ))),
                Err(
                    ProviderError::new(ProviderErrorKind::Unauthorized, UpstreamSendState::Sent)
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
    let operation = generate_operation();
    let route_plan = plan(&operation);
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
fn retryable_error_after_credential_recovery_switches_account_instead_of_terminating() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
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
    block_on(session.collect_uncommitted()).expect("third attempt succeeds on another account");
    block_on(session.commit_downstream(Some(200))).expect("commit response");

    let contexts = provider.contexts.lock().expect("contexts lock");
    let first = ProviderAccountId::new("acct_first").expect("account");
    assert_eq!(contexts.len(), 3);
    assert_eq!(contexts[1].required_account(), Some(&first));
    assert!(contexts[1].credential_recovery_attempted());
    // recovery 钉账号只绑定 replay attempt；replay 上的 429 之后必须能换号。
    assert_eq!(contexts[2].required_account(), None);
    assert!(contexts[2].excluded_accounts().contains(&first));
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.attempts.len(), 3);
    assert_eq!(state.intermediate_failures, 2);
    assert_eq!(state.finalizations[0].outcome, ExecutionOutcome::Succeeded);
}

#[test]
fn non_idempotent_explicit_429_rejection_rotates_account_before_output() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
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
fn rate_limited_account_exhaustion_survives_a_later_empty_selection() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let (coordinator, store, _) = coordinator(vec![
        Script::Stream {
            account_id: "acct_first",
            items: vec![Err(ProviderError::new(
                ProviderErrorKind::RateLimited,
                UpstreamSendState::Sent,
            )
            .with_status(429)
            .with_upstream_code(OpaqueUpstreamValue::new("rate_limit_exceeded"))
            .with_retry_after(Duration::from_secs(30))
            .with_replay_safe())],
        },
        Script::Error(ProviderError::new(
            ProviderErrorKind::NoEligibleAccount,
            UpstreamSendState::NotSent,
        )),
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
    let error = block_on(session.collect_uncommitted())
        .expect_err("exhausted account must remain a rate-limit response");
    let gateway_core::engine::EngineError::Provider(error) = error else {
        panic!("expected provider error")
    };

    assert_eq!(error.kind(), ProviderErrorKind::RateLimited);
    assert_eq!(error.upstream_status(), Some(429));
    assert_eq!(
        error.upstream_code().map(OpaqueUpstreamValue::as_str),
        Some("rate_limit_exceeded")
    );
    assert_eq!(error.retry_after(), Some(Duration::from_secs(30)));
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.created, 1);
    assert_eq!(state.attempts.len(), 1);
    assert_eq!(state.intermediate_failures, 1);
    assert_eq!(state.finalizations.len(), 1);
    // attempt-1 已把 sent 落库；attempt-2 空选路终态不得降级回 not_sent。
    assert_eq!(state.finalizations[0].send_state, UpstreamSendState::Sent);
    assert_eq!(state.finalizations[0].upstream_status_code, Some(429));
    assert_eq!(
        state.finalizations[0].provider_error_code.as_deref(),
        Some("rate_limit_exceeded")
    );
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
    let operation = generate_operation();
    let route_plan = plan(&operation);
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
fn atomic_response_failed_should_rotate_account_before_stream_commit() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let (coordinator, store, provider) = coordinator(vec![
        Script::ObservedStream {
            account_id: "acct_first",
            items: vec![Err(atomic_response_failed(
                "response-first-failed",
                "first account failure",
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

    let first = block_on(session.next_event())
        .expect("second account should produce the first delivery")
        .expect("started event");
    let delivered = first.into_provider_events();
    assert!(delivered.iter().flat_map(ProviderEvent::canonical_facts).any(
        |event| matches!(event, GatewayEvent::Started(meta) if meta.response_id() == "response-1")
    ));
    assert!(
        !delivered
            .iter()
            .filter_map(ProviderEvent::wire_event)
            .any(|wire| {
                wire.data().pointer("/response/id").and_then(Value::as_str)
                    == Some("response-first-failed")
            })
    );
    block_on(session.commit_downstream(Some(200))).expect("commit second account");
    while block_on(session.next_event())
        .expect("successful stream")
        .is_some()
    {}

    assert_eq!(provider.contexts.lock().expect("contexts lock").len(), 2);
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.commits, 1);
    assert_eq!(state.intermediate_failures, 1);
    assert_eq!(state.finalizations[0].outcome, ExecutionOutcome::Succeeded);
}

#[test]
fn bare_atomic_response_failed_should_rotate_before_the_first_delivery() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let (coordinator, store, provider) = coordinator(vec![
        Script::ObservedStream {
            account_id: "acct_first",
            items: vec![Err(bare_atomic_response_failed("response-bare-failed"))],
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
        .expect("second account should replace the bare failure")
        .expect("second account started event");
    assert!(first
        .into_provider_events()
        .iter()
        .flat_map(ProviderEvent::canonical_facts)
        .any(|event| matches!(event, GatewayEvent::Started(meta) if meta.response_id() == "response-1")));
    block_on(session.commit_downstream(Some(200))).expect("commit second account");
    while block_on(session.next_event())
        .expect("successful stream")
        .is_some()
    {}

    assert_eq!(provider.contexts.lock().expect("contexts lock").len(), 2);
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.intermediate_failures, 1);
    assert_eq!(state.finalizations[0].outcome, ExecutionOutcome::Succeeded);
}

#[test]
fn exhausted_atomic_response_failed_should_deliver_only_the_last_failure_once() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let scripts = (1..=route_plan.max_attempts().get())
        .map(|attempt| {
            let response_id = format!("response-attempt-{attempt}-failed");
            let marker = if attempt == route_plan.max_attempts().get() {
                "last upstream failure".to_owned()
            } else {
                format!("discarded failure {attempt}")
            };
            Script::ObservedStream {
                account_id: "acct_failed",
                items: vec![Err(atomic_response_failed(&response_id, &marker))],
            }
        })
        .collect::<Vec<_>>();
    let expected_attempts = route_plan.max_attempts().get();
    let expected_last_response = format!("response-attempt-{expected_attempts}-failed");
    let (coordinator, store, provider) = coordinator(scripts);
    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        None,
        None,
        CancellationToken::new(),
    ))
    .expect("start execution");

    let terminal = block_on(session.next_event())
        .expect("last failure remains deliverable")
        .expect("terminal failure batch");
    assert_eq!(
        terminal.commit_requirement(),
        CommitRequirement::CommitBeforeDelivery
    );
    let events = terminal.into_provider_events();
    let failures = events
        .iter()
        .filter_map(ProviderEvent::wire_event)
        .filter(|wire| wire.event_type() == Some("response.failed"))
        .collect::<Vec<_>>();
    assert_eq!(failures.len(), 1);
    assert_eq!(
        failures[0]
            .data()
            .pointer("/response/id")
            .and_then(Value::as_str),
        Some(expected_last_response.as_str())
    );
    assert!(
        failures[0]
            .data()
            .to_string()
            .contains("last upstream failure")
    );
    block_on(session.commit_downstream(Some(200))).expect("commit original terminal failure");
    let error = block_on(session.next_event()).expect_err("typed failure finalizes the request");
    assert!(matches!(error, EngineError::Provider(_)));
    assert!(session.is_finalized());

    assert_eq!(
        provider.contexts.lock().expect("contexts lock").len(),
        usize::try_from(expected_attempts).expect("attempt count fits usize")
    );
    let state = store.state.lock().expect("store lock");
    assert_eq!(
        state.intermediate_failures,
        usize::try_from(expected_attempts - 1).expect("attempt count fits usize")
    );
    assert_eq!(state.commits, 1);
    assert_eq!(state.finalizations[0].outcome, ExecutionOutcome::Incomplete);
    assert!(state.finalizations[0].committed);
}

#[test]
fn empty_selection_after_atomic_failure_should_deliver_the_last_upstream_wire() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let (coordinator, store, provider) = coordinator(vec![
        Script::ObservedStream {
            account_id: "acct_only",
            items: vec![Err(bare_atomic_response_failed("response-only-failed"))],
        },
        Script::Error(ProviderError::new(
            ProviderErrorKind::NoEligibleAccount,
            UpstreamSendState::NotSent,
        )),
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

    let terminal = block_on(session.next_event())
        .expect("last account failure remains deliverable")
        .expect("terminal failure batch");
    let events = terminal.into_provider_events();
    let failure = events
        .iter()
        .filter_map(ProviderEvent::wire_event)
        .find(|wire| wire.event_type() == Some("response.failed"))
        .expect("last upstream response.failed");
    assert_eq!(
        failure
            .data()
            .pointer("/response/id")
            .and_then(Value::as_str),
        Some("response-only-failed")
    );
    block_on(session.commit_downstream(Some(200))).expect("commit terminal failure");
    assert!(matches!(
        block_on(session.next_event()),
        Err(EngineError::Provider(_))
    ));

    assert_eq!(provider.contexts.lock().expect("contexts lock").len(), 2);
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.attempts.len(), 1);
    assert_eq!(state.intermediate_failures, 1);
    assert_eq!(state.finalizations[0].outcome, ExecutionOutcome::Incomplete);
    assert_eq!(
        state.finalizations[0].client_response_id.as_deref(),
        Some("response-only-failed")
    );
    assert_eq!(
        state.finalizations[0].upstream_response_id.as_deref(),
        Some("response-only-failed")
    );
}

#[test]
fn atomic_response_failed_keeps_collect_uncommitted_error_semantics() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let (coordinator, store, provider) = coordinator(vec![Script::ObservedStream {
        account_id: "acct_required",
        items: vec![Err(atomic_response_failed(
            "response-required-failed",
            "required account failure",
        ))],
    }]);
    let required = ProviderAccountId::new("acct_required").expect("required account");
    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        Some(required),
        None,
        CancellationToken::new(),
    ))
    .expect("start execution");

    let error = block_on(session.collect_uncommitted())
        .expect_err("non-streaming collection must keep returning the typed failure");
    assert!(matches!(error, EngineError::Provider(_)));
    assert!(session.is_finalized());
    assert_eq!(provider.contexts.lock().expect("contexts lock").len(), 1);
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.commits, 0);
    assert_eq!(state.intermediate_failures, 0);
    assert_eq!(state.finalizations[0].outcome, ExecutionOutcome::Failed);
}

#[test]
fn native_continuation_explicit_429_is_not_retried() {
    let continuation = NativeContinuationPin::new(
        PreviousResponseId::new("previous-secret-id"),
        PreviousResponseId::new("provider-native-id"),
        ClientApiKeyId::new("key_client_1").expect("client key"),
        ProviderKind::new("openai").expect("provider"),
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
    let opaque_response_id = format!("resp_{}\0opaque", "x".repeat(257));
    let continuation = ContinuationBinding::External(PreviousResponseId::new(opaque_response_id));
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
fn ambiguous_send_state_stops_retry() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let (coordinator, store, provider) = coordinator(vec![
        Script::Stream {
            account_id: "acct_first",
            items: vec![Err(ProviderError::new(
                ProviderErrorKind::Transport,
                UpstreamSendState::Ambiguous,
            )
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
fn ambiguous_pre_delivery_retry_marker_does_not_rotate_account() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let checkpoint = ProviderSessionState::new(
        "openai",
        Map::from_iter([("checkpoint".to_owned(), json!("opaque-state"))]),
    )
    .expect("provider checkpoint");
    let mut checkpoint_event = ProviderEvent::observation(ProviderResponseObservation::new(
        UpstreamTransport::new("websocket").expect("transport"),
    ));
    checkpoint_event.attach_session_update(checkpoint);
    let (coordinator, store, provider) = coordinator(vec![
        Script::ObservedStream {
            account_id: "acct_first",
            items: vec![
                Ok(checkpoint_event),
                Err(
                    ProviderError::new(ProviderErrorKind::Transport, UpstreamSendState::Ambiguous)
                        .with_pre_delivery_retry(),
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
    let error = block_on(session.collect_uncommitted())
        .expect_err("ambiguous payload state must suppress provider-requested replay");

    assert!(matches!(error, EngineError::Provider(_)));
    let contexts = provider.contexts.lock().expect("contexts lock");
    assert_eq!(contexts.len(), 1);
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.intermediate_failures, 0);
    assert_eq!(state.finalizations[0].attempt_count, 1);
    assert_eq!(state.finalizations[0].outcome, ExecutionOutcome::Failed);
}

#[test]
fn ambiguous_transport_retry_uses_provider_session_checkpoint_on_same_account() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let checkpoint = ProviderSessionState::new(
        "openai",
        Map::from_iter([("checkpoint".to_owned(), json!("opaque-state"))]),
    )
    .expect("provider checkpoint");
    let mut checkpoint_event = ProviderEvent::observation(ProviderResponseObservation::new(
        UpstreamTransport::new("websocket").expect("transport"),
    ));
    checkpoint_event.attach_session_update(checkpoint.clone());
    let retry_index = NonZeroU32::new(1).expect("retry index");
    let (coordinator, store, provider) = coordinator(vec![
        Script::ObservedStream {
            account_id: "acct_first",
            items: vec![
                Ok(checkpoint_event),
                Err(
                    ProviderError::new(ProviderErrorKind::Transport, UpstreamSendState::Ambiguous)
                        .with_replay_safe()
                        .with_pre_delivery_transport_retry(retry_index, Duration::ZERO),
                ),
            ],
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
    block_on(session.collect_uncommitted()).expect("checkpointed retry succeeds");
    block_on(session.commit_downstream(Some(200))).expect("commit winning response");

    let original = ProviderAccountId::new("acct_first").expect("account id");
    let contexts = provider.contexts.lock().expect("contexts lock");
    assert_eq!(contexts.len(), 2);
    assert_eq!(
        contexts[1].transport(),
        AttemptTransport::Retry(retry_index)
    );
    assert_eq!(contexts[1].required_account(), Some(&original));
    drop(contexts);
    let operations = provider.operations.lock().expect("operations lock");
    assert_eq!(
        operations[1].provider_session_state("openai"),
        Some(&checkpoint)
    );
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.intermediate_failures, 1);
    assert_eq!(state.finalizations[0].attempt_count, 2);
    assert_eq!(state.finalizations[0].outcome, ExecutionOutcome::Succeeded);
}

#[test]
fn pre_delivery_transport_fallback_retries_the_same_account_with_fallback_transport() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let (coordinator, store, provider) = coordinator(vec![
        Script::Stream {
            account_id: "acct_first",
            items: vec![Err(ProviderError::new(
                ProviderErrorKind::Transport,
                UpstreamSendState::NotSent,
            )
            .with_pre_delivery_transport_fallback())],
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
    block_on(session.collect_uncommitted()).expect("same-account fallback succeeds");
    block_on(session.commit_downstream(Some(200))).expect("commit winning response");

    let original = ProviderAccountId::new("acct_first").expect("account id");
    let contexts = provider.contexts.lock().expect("contexts lock");
    assert_eq!(contexts.len(), 2);
    assert_eq!(contexts[0].transport(), AttemptTransport::Default);
    assert_eq!(contexts[1].transport(), AttemptTransport::Fallback);
    assert_eq!(contexts[1].required_account(), Some(&original));
    assert!(!contexts[1].excluded_accounts().contains(&original));
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.intermediate_failures, 1);
    assert_eq!(state.finalizations[0].outcome, ExecutionOutcome::Succeeded);
}

#[test]
fn provider_owned_transport_retries_keep_the_same_account_until_fallback() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let retry_one = NonZeroU32::new(1).expect("non-zero retry index");
    let retry_two = NonZeroU32::new(2).expect("non-zero retry index");
    let transport_error =
        || ProviderError::new(ProviderErrorKind::Transport, UpstreamSendState::NotSent);
    let (coordinator, store, provider) = coordinator(vec![
        Script::Stream {
            account_id: "acct_first",
            items: vec![Err(
                transport_error().with_pre_delivery_transport_retry(retry_one, Duration::ZERO)
            )],
        },
        Script::Stream {
            account_id: "acct_first",
            items: vec![Err(
                transport_error().with_pre_delivery_transport_retry(retry_two, Duration::ZERO)
            )],
        },
        Script::Stream {
            account_id: "acct_first",
            items: vec![Err(transport_error().with_pre_delivery_transport_fallback())],
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
    block_on(session.collect_uncommitted()).expect("fallback succeeds");
    block_on(session.commit_downstream(Some(200))).expect("commit winning response");

    let original = ProviderAccountId::new("acct_first").expect("account id");
    let contexts = provider.contexts.lock().expect("contexts lock");
    assert_eq!(contexts.len(), 4);
    assert_eq!(contexts[0].transport(), AttemptTransport::Default);
    assert_eq!(contexts[1].transport(), AttemptTransport::Retry(retry_one));
    assert_eq!(contexts[2].transport(), AttemptTransport::Retry(retry_two));
    assert_eq!(contexts[3].transport(), AttemptTransport::Fallback);
    for context in contexts.iter().skip(1) {
        assert_eq!(context.required_account(), Some(&original));
        assert!(!context.excluded_accounts().contains(&original));
    }
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.intermediate_failures, 3);
    assert_eq!(state.finalizations[0].attempt_count, 4);
    assert_eq!(state.finalizations[0].outcome, ExecutionOutcome::Succeeded);
}

#[test]
fn final_capacity_exhaustion_returns_the_last_retryable_upstream_failure() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let raw_body = Bytes::from_static(b"{\"error\":\"last upstream body\"}\x00");
    let upstream_error =
        ProviderError::new(ProviderErrorKind::Transport, UpstreamSendState::NotSent)
            .with_pre_delivery_retry()
            .with_client_visible_upstream_error(ClientVisibleUpstreamError::new(
                "message too big",
                Some("1009".to_owned()),
                Some("websocket_close_error".to_owned()),
            ))
            .with_client_visible_upstream_response(ClientVisibleUpstreamResponse::new(
                502,
                Some(b"application/json".to_vec()),
                raw_body.clone(),
            ));
    let (coordinator, store, provider) = coordinator(vec![
        Script::Stream {
            account_id: "acct_first",
            items: vec![Err(upstream_error)],
        },
        Script::Error(ProviderError::new(
            ProviderErrorKind::AccountCapacityUnavailable,
            UpstreamSendState::NotSent,
        )),
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
    let error = block_on(session.collect_uncommitted())
        .expect_err("empty selection must return the retained upstream failure");
    let EngineError::Provider(error) = error else {
        panic!("expected retained provider error");
    };

    assert_eq!(error.kind(), ProviderErrorKind::Transport);
    let detail = error
        .client_visible_upstream_error()
        .expect("structured upstream detail");
    assert_eq!(detail.message(), "message too big");
    assert_eq!(detail.code(), Some("1009"));
    assert_eq!(detail.error_type(), Some("websocket_close_error"));
    let response = error
        .client_visible_upstream_response()
        .expect("original upstream response");
    assert_eq!(response.status(), 502);
    assert_eq!(response.body(), &raw_body);

    assert_eq!(provider.contexts.lock().expect("contexts lock").len(), 2);
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.intermediate_failures, 1);
    assert_eq!(state.finalizations[0].outcome, ExecutionOutcome::Failed);
    assert_eq!(
        state.finalizations[0].send_state,
        UpstreamSendState::NotSent
    );
}

#[test]
fn provider_infrastructure_failure_does_not_restore_the_last_upstream_failure() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let upstream_error =
        ProviderError::new(ProviderErrorKind::Transport, UpstreamSendState::NotSent)
            .with_pre_delivery_retry()
            .with_client_visible_upstream_error(ClientVisibleUpstreamError::new(
                "upstream marker",
                Some("upstream_marker".to_owned()),
                Some("upstream_error".to_owned()),
            ))
            .with_client_visible_upstream_response(ClientVisibleUpstreamResponse::new(
                502,
                Some(b"application/json".to_vec()),
                Bytes::from_static(b"{\"error\":\"upstream marker\"}"),
            ));
    let (coordinator, store, provider) = coordinator(vec![
        Script::Stream {
            account_id: "acct_first",
            items: vec![Err(upstream_error)],
        },
        Script::Error(ProviderError::new(
            ProviderErrorKind::ProviderInfrastructureUnavailable,
            UpstreamSendState::NotSent,
        )),
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
    let error = block_on(session.collect_uncommitted())
        .expect_err("provider infrastructure failure must remain terminal");
    let EngineError::Provider(error) = error else {
        panic!("expected provider error");
    };

    assert_eq!(
        error.kind(),
        ProviderErrorKind::ProviderInfrastructureUnavailable
    );
    assert!(error.client_visible_upstream_error().is_none());
    assert!(error.client_visible_upstream_response().is_none());
    assert_eq!(provider.contexts.lock().expect("contexts lock").len(), 2);
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.intermediate_failures, 1);
    assert_eq!(state.finalizations[0].outcome, ExecutionOutcome::Failed);
}

#[test]
fn pre_delivery_retry_marker_is_ignored_after_downstream_commit() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let (coordinator, store, provider) = coordinator(vec![
        Script::Stream {
            account_id: "acct_first",
            items: vec![
                Ok(GatewayEvent::Started(ResponseMeta::new(
                    "response-visible",
                    "gpt-5",
                ))),
                Err(
                    ProviderError::new(ProviderErrorKind::Transport, UpstreamSendState::Ambiguous)
                        .with_pre_delivery_transport_fallback(),
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
        .expect("visible event");
    assert_eq!(
        first.commit_requirement(),
        CommitRequirement::CommitBeforeDelivery
    );
    block_on(session.commit_downstream(Some(200))).expect("commit first event");
    let error = block_on(session.next_event()).expect_err("committed output cannot be replayed");

    assert!(matches!(error, EngineError::Provider(_)));
    assert_eq!(provider.contexts.lock().expect("contexts lock").len(), 1);
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.intermediate_failures, 0);
    assert_eq!(state.finalizations[0].outcome, ExecutionOutcome::Incomplete);
    assert!(state.finalizations[0].committed);
}

#[test]
fn structural_event_before_replay_safe_failure_should_switch_account_before_commit() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
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
            provider_kind: ProviderKind::new("openai").expect("provider"),
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
    let operation = generate_operation();
    let route_plan = plan(&operation);
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
fn no_eligible_account_before_stream_does_not_create_request_detail() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let (coordinator, store, _) = coordinator(vec![Script::Error(ProviderError::new(
        ProviderErrorKind::NoEligibleAccount,
        UpstreamSendState::NotSent,
    ))]);

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
        gateway_core::engine::EngineError::Provider(ref error)
            if error.kind() == ProviderErrorKind::NoEligibleAccount
    ));
    assert!(session.provider_attempt_outcomes().is_empty());
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.created, 0);
    assert!(state.attempts.is_empty());
    assert!(state.finalizations.is_empty());
}

#[test]
fn expired_deadline_finalizes_without_calling_provider() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
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
    assert_eq!(state.created, 0);
    assert!(state.attempts.is_empty());
    assert!(state.finalizations.is_empty());
}

#[test]
fn deadline_after_commit_is_incomplete_without_provider_circuit_failure() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let (coordinator, store, _) = coordinator(vec![Script::HangingStream {
        account_id: "acct_first",
        items: vec![Ok(GatewayEvent::Started(ResponseMeta::new(
            "response-1",
            "gpt-5",
        )))],
    }]);
    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_millis(300)),
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
    block_on(session.commit_downstream(Some(200))).expect("commit started event");

    let error = block_on(session.next_event()).expect_err("deadline elapses mid-stream");

    assert!(matches!(error, EngineError::Deadline));
    // 网关自身请求预算到期不是上游超时；已在交付中的长流集中到期
    // 不得作为 provider Timeout 计入熔断。
    assert!(session.provider_attempt_outcomes().is_empty());
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.finalizations[0].outcome, ExecutionOutcome::Incomplete);
    assert!(state.finalizations[0].committed);
}

#[test]
fn deadline_before_first_event_records_no_provider_circuit_failure() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let (coordinator, store, _) = coordinator(vec![Script::HangingStream {
        account_id: "acct_first",
        items: Vec::new(),
    }]);
    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_millis(300)),
        operation,
        route_plan,
        None,
        None,
        CancellationToken::new(),
    ))
    .expect("start execution");

    let error = block_on(session.next_event()).expect_err("deadline elapses before first event");

    assert!(matches!(error, EngineError::Deadline));
    assert!(session.provider_attempt_outcomes().is_empty());
    let state = store.state.lock().expect("store lock");
    assert_eq!(state.attempts.len(), 1);
    assert_eq!(state.finalizations[0].outcome, ExecutionOutcome::Failed);
    assert!(!state.finalizations[0].committed);
}
