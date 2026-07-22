use std::collections::{BTreeSet, VecDeque};
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use futures::{StreamExt, stream};
use gateway_core::engine::continuation::{
    ContinuationBinding, NativeContinuationPin, PreviousResponseId,
};
use gateway_core::engine::credential::{
    AccountSelectionPolicy, CredentialRevision, ProviderAccountStore, RotationStrategy,
};
use gateway_core::engine::provider::{Provider, ProviderRequest};
use gateway_core::engine::{
    AccountAttemptContext, AttemptContext, CancellationToken, ContinuationAttempt, ModelRequestId,
    UpstreamSendState,
};
use gateway_core::error::{
    ContinuationFailure, ProviderError, ProviderErrorKind, SafeUpstreamValue,
};
use gateway_core::event::{GatewayEvent, UpstreamHttpVersion};
use gateway_core::operation::{
    CompactConversationRequest, Feature, GenerateRequest, Operation, OperationKind,
    ProtocolPayload, ProviderSessionState,
};
use gateway_core::routing::{
    ConfigRevision, ModelCapabilities, ProviderKind, ProviderModel, PublicModelId, RoutingContext,
    RuntimeSnapshot, UpstreamModelId,
};
use provider_xai::{
    GrokBuildProvider, GrokCredentialCatalogCache, GrokCredentialFailure,
    GrokCredentialFeedbackFuture, GrokCredentialRecovery, GrokCredentialRecoveryOutcome,
    GrokCredentialRepository, GrokInferenceRequest, GrokInferenceResponse, GrokInferenceTransport,
    GrokInferenceTransportError, GrokInferenceTransportErrorKind, GrokInferenceTransportFuture,
    GrokModelCatalogRequest, GrokModelCatalogTransport, GrokModelCatalogTransportFuture,
    GrokModelCatalogTransportResponse, GrokSessionBinding, GrokSessionSelection,
    GrokSessionSelector, GrokSessionSelectorError, GrokSessionSelectorFuture, SecretValue,
    SelectedGrokSession,
};
use serde_json::{Map, json};

use crate::support::{
    MemoryGrokCatalogCache, MemoryProviderAccountStore, account_id, create_input, seed_input,
};

const MODEL: &str = "grok-4.5";
const CATALOG_FIXTURE: &[u8] =
    include_bytes!("../transport/catalog/fixtures/official_grok_models_snapshot.json");
const CATALOG_WITHOUT_TOOL_METADATA: &[u8] = br#"{
  "object": "list",
  "data": [{
    "id": "grok-4.5-catalog-entry",
    "model": "grok-4.5",
    "contextWindow": 1000000,
    "maxCompletionTokens": 131072,
    "apiBackend": "responses",
    "supportedInApi": true,
    "supportsReasoningEffort": true
  }]
}"#;
const SUCCESS_SSE: &[u8] = concat!(
    "event: response.created\n",
    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_xai\",\"model\":\"grok-4.5\"}}\n\n",
    "event: response.content_part.added\n",
    "data: {\"type\":\"response.content_part.added\",\"output_index\":0,\"content_index\":0,\"part\":{\"type\":\"output_text\"}}\n\n",
    "event: response.output_text.delta\n",
    "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"hello\"}\n\n",
    "event: response.completed\n",
    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_xai\",\"model\":\"grok-4.5\",\"status\":\"completed\"}}\n\n",
)
.as_bytes();

fn stateful_sse(encrypted_content: &str) -> Vec<u8> {
    format!(
        concat!(
            "event: response.created\n",
            "data: {{\"type\":\"response.created\",\"response\":{{\"id\":\"resp_state\",\"model\":\"grok-4.5\"}}}}\n\n",
            "event: response.output_item.done\n",
            "data: {{\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{{\"id\":\"reason_account_bound\",\"type\":\"reasoning\",\"status\":\"completed\",\"summary\":[],\"content\":null,\"encrypted_content\":\"{}\"}}}}\n\n",
            "event: response.completed\n",
            "data: {{\"type\":\"response.completed\",\"response\":{{\"id\":\"resp_state\",\"model\":\"grok-4.5\",\"status\":\"completed\"}}}}\n\n"
        ),
        encrypted_content
    )
    .into_bytes()
}

fn custom_apply_patch_sse(patch: &str) -> Vec<u8> {
    let arguments = serde_json::to_string(&json!({"patch": patch})).expect("patch arguments");
    let arguments_json = serde_json::to_string(&arguments).expect("arguments JSON string");
    format!(
        concat!(
            "event: response.created\n",
            "data: {{\"type\":\"response.created\",\"response\":{{\"id\":\"resp_custom_patch\",\"model\":\"grok-4.5\"}}}}\n\n",
            "event: response.output_item.done\n",
            "data: {{\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{{\"id\":\"item_custom_patch\",\"type\":\"function_call\",\"call_id\":\"call_custom_patch\",\"name\":\"apply_patch\",\"arguments\":{arguments_json}}}}}\n\n",
            "event: response.completed\n",
            "data: {{\"type\":\"response.completed\",\"response\":{{\"id\":\"resp_custom_patch\",\"model\":\"grok-4.5\",\"status\":\"completed\"}}}}\n\n",
        ),
        arguments_json = arguments_json,
    )
    .into_bytes()
}

fn compaction_sse(summary: &str, reasoning: Option<&str>) -> Vec<u8> {
    let summary = serde_json::to_string(summary).expect("summary JSON string");
    let reasoning = reasoning.map_or_else(String::new, |reasoning| {
        let reasoning = serde_json::to_string(reasoning).expect("reasoning JSON string");
        format!(
            concat!(
                "event: response.output_item.added\n",
                "data: {{\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{{\"type\":\"reasoning\",\"id\":\"reason_summary\"}}}}\n\n",
                "event: response.reasoning_summary_part.added\n",
                "data: {{\"type\":\"response.reasoning_summary_part.added\",\"output_index\":0,\"summary_index\":0,\"part\":{{\"type\":\"summary_text\"}}}}\n\n",
                "event: response.reasoning_summary_text.delta\n",
                "data: {{\"type\":\"response.reasoning_summary_text.delta\",\"output_index\":0,\"summary_index\":0,\"delta\":{reasoning}}}\n\n",
            ),
            reasoning = reasoning,
        )
    });
    format!(
        concat!(
            "event: response.created\n",
            "data: {{\"type\":\"response.created\",\"response\":{{\"id\":\"resp_compaction\",\"model\":\"grok-4.5\"}}}}\n\n",
            "{reasoning}",
            "event: response.content_part.added\n",
            "data: {{\"type\":\"response.content_part.added\",\"output_index\":1,\"content_index\":0,\"part\":{{\"type\":\"output_text\"}}}}\n\n",
            "event: response.output_text.delta\n",
            "data: {{\"type\":\"response.output_text.delta\",\"output_index\":1,\"content_index\":0,\"delta\":{summary}}}\n\n",
            "event: response.completed\n",
            "data: {{\"type\":\"response.completed\",\"response\":{{\"id\":\"resp_compaction\",\"model\":\"grok-4.5\",\"status\":\"completed\",\"usage\":{{\"input_tokens\":100,\"output_tokens\":20,\"total_tokens\":120}}}}}}\n\n",
        ),
        reasoning = reasoning,
        summary = summary,
    )
    .into_bytes()
}

fn compaction_sse_without_terminal(summary: &str) -> Vec<u8> {
    let summary = serde_json::to_string(summary).expect("summary JSON string");
    format!(
        concat!(
            "event: response.created\n",
            "data: {{\"type\":\"response.created\",\"response\":{{\"id\":\"resp_compaction\",\"model\":\"grok-4.5\"}}}}\n\n",
            "event: response.content_part.added\n",
            "data: {{\"type\":\"response.content_part.added\",\"output_index\":0,\"content_index\":0,\"part\":{{\"type\":\"output_text\"}}}}\n\n",
            "event: response.output_text.delta\n",
            "data: {{\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":{summary}}}\n\n",
        ),
        summary = summary,
    )
    .into_bytes()
}

fn malformed_compaction_sse() -> Vec<u8> {
    concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_compaction\",\"model\":\"grok-4.5\"}}\n\n",
        "event: response.output_text.delta\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"invalid\"}\n\n",
    )
    .as_bytes()
    .to_vec()
}

fn valid_compaction_summary(marker: &str) -> String {
    format!(
        "<summary>\n{marker}\n{}\n</summary>",
        "preserved implementation context ".repeat(20)
    )
}

struct StubSelector {
    calls: AtomicUsize,
    feedback: Mutex<Vec<GrokCredentialFailure>>,
    error: Mutex<Option<GrokSessionSelectorError>>,
    required_accounts: Mutex<Vec<Option<gateway_core::engine::credential::ProviderAccountId>>>,
}

impl StubSelector {
    fn success() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            feedback: Mutex::new(Vec::new()),
            error: Mutex::new(None),
            required_accounts: Mutex::new(Vec::new()),
        })
    }

    fn failing(error: GrokSessionSelectorError) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            feedback: Mutex::new(Vec::new()),
            error: Mutex::new(Some(error)),
            required_accounts: Mutex::new(Vec::new()),
        })
    }
}

impl GrokSessionSelector for StubSelector {
    fn select(&self, request: GrokSessionSelection) -> GrokSessionSelectorFuture<'_> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let error = self.error.lock().expect("selector error").take();
        let required_account = request.required_account().cloned();
        self.required_accounts
            .lock()
            .expect("required accounts")
            .push(required_account.clone());
        Box::pin(async move {
            if let Some(error) = error {
                return Err(error);
            }
            let id = account_id("provider");
            if request.excluded_accounts().contains(&id)
                || required_account
                    .as_ref()
                    .is_some_and(|required| required != &id)
            {
                return Err(GrokSessionSelectorError::NoEligibleSession);
            }
            SelectedGrokSession::new(
                id,
                CredentialRevision::new(1).expect("revision"),
                SecretValue::new("oauth-access"),
                SecretValue::new("verified-user"),
                Some(SecretValue::new("user@example.com")),
                GrokSessionBinding::new("acct_provider").expect("binding"),
                (),
            )
            .map_err(|_| GrokSessionSelectorError::InvalidSession)
        })
    }

    fn record_failure<'a>(
        &'a self,
        _: &'a SelectedGrokSession,
        failure: GrokCredentialFailure,
    ) -> GrokCredentialFeedbackFuture<'a> {
        Box::pin(async move {
            self.feedback.lock().expect("feedback").push(failure);
        })
    }
}

enum InferenceMode {
    Success,
    SuccessBody(Vec<u8>),
    Error(GrokInferenceTransportError),
    StreamError(GrokInferenceTransportError),
}

struct StubInferenceTransport {
    calls: AtomicUsize,
    requests: Mutex<Vec<GrokInferenceRequest>>,
    modes: Mutex<VecDeque<InferenceMode>>,
}

impl StubInferenceTransport {
    fn success() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
            modes: Mutex::new(VecDeque::from([InferenceMode::Success])),
        })
    }

    fn error(error: GrokInferenceTransportError) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
            modes: Mutex::new(VecDeque::from([InferenceMode::Error(error)])),
        })
    }

    fn stream_error(error: GrokInferenceTransportError) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
            modes: Mutex::new(VecDeque::from([InferenceMode::StreamError(error)])),
        })
    }

    fn sequence(modes: impl IntoIterator<Item = InferenceMode>) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
            modes: Mutex::new(modes.into_iter().collect()),
        })
    }
}

impl GrokInferenceTransport for StubInferenceTransport {
    fn execute(&self, request: GrokInferenceRequest) -> GrokInferenceTransportFuture<'_> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().expect("requests").push(request);
        let mode = self
            .modes
            .lock()
            .expect("modes")
            .pop_front()
            .expect("one transport mode");
        Box::pin(async move {
            match mode {
                InferenceMode::Success => Ok(GrokInferenceResponse::new(
                    Box::pin(stream::iter([Ok(SUCCESS_SSE.to_vec())])),
                    UpstreamHttpVersion::Http2,
                    200,
                    None,
                )),
                InferenceMode::SuccessBody(body) => Ok(GrokInferenceResponse::new(
                    Box::pin(stream::iter([Ok(body)])),
                    UpstreamHttpVersion::Http2,
                    200,
                    None,
                )),
                InferenceMode::Error(error) => Err(error),
                InferenceMode::StreamError(error) => Ok(GrokInferenceResponse::new(
                    Box::pin(stream::iter([Err(error)])),
                    UpstreamHttpVersion::Http2,
                    200,
                    None,
                )),
            }
        })
    }
}

struct StaticCatalogTransport;

impl GrokModelCatalogTransport for StaticCatalogTransport {
    fn execute(&self, _: GrokModelCatalogRequest) -> GrokModelCatalogTransportFuture<'_> {
        Box::pin(async {
            Ok(GrokModelCatalogTransportResponse::new(
                CATALOG_FIXTURE,
                None,
            ))
        })
    }
}

struct CatalogWithoutToolMetadataTransport;

impl GrokModelCatalogTransport for CatalogWithoutToolMetadataTransport {
    fn execute(&self, _: GrokModelCatalogRequest) -> GrokModelCatalogTransportFuture<'_> {
        Box::pin(async {
            Ok(GrokModelCatalogTransportResponse::new(
                CATALOG_WITHOUT_TOOL_METADATA,
                None,
            ))
        })
    }
}

struct StubRecovery {
    calls: AtomicUsize,
    outcome: GrokCredentialRecoveryOutcome,
}

impl StubRecovery {
    fn new(outcome: GrokCredentialRecoveryOutcome) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            outcome,
        })
    }
}

#[async_trait::async_trait]
impl GrokCredentialRecovery for StubRecovery {
    async fn recover_unauthorized(
        &self,
        _: &gateway_core::engine::credential::ProviderAccountId,
        _: CredentialRevision,
    ) -> GrokCredentialRecoveryOutcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.outcome
    }
}

async fn provider(
    selector: Arc<StubSelector>,
    transport: Arc<StubInferenceTransport>,
) -> GrokBuildProvider {
    provider_with_recovery(
        selector,
        transport,
        StubRecovery::new(GrokCredentialRecoveryOutcome::Unavailable),
    )
    .await
}

async fn provider_with_recovery(
    selector: Arc<StubSelector>,
    transport: Arc<StubInferenceTransport>,
    recovery: Arc<StubRecovery>,
) -> GrokBuildProvider {
    provider_with_catalog_transport(
        selector,
        transport,
        recovery,
        Arc::new(StaticCatalogTransport),
    )
    .await
}

async fn provider_with_catalog_transport(
    selector: Arc<StubSelector>,
    transport: Arc<StubInferenceTransport>,
    recovery: Arc<StubRecovery>,
    catalog_transport: Arc<dyn GrokModelCatalogTransport>,
) -> GrokBuildProvider {
    let store = MemoryProviderAccountStore::shared();
    let account_store: Arc<dyn ProviderAccountStore> = store.clone();
    let repository = GrokCredentialRepository::new(account_store);
    seed_input(
        &store,
        &create_input("catalog-provider", "subject-provider"),
    )
    .await
    .expect("catalog account");
    let cache: Arc<dyn GrokCredentialCatalogCache> = MemoryGrokCatalogCache::shared();
    let catalog = Arc::new(crate::support::grok_catalog_service(
        repository,
        catalog_transport,
        cache,
    ));
    GrokBuildProvider::new(
        selector,
        transport,
        catalog,
        recovery,
        crate::support::xai_wire_profile(),
    )
    .expect("official xAI provider configuration")
}

async fn mapped_transport_error(
    error: GrokInferenceTransportError,
    body_stream: bool,
) -> ProviderError {
    let selector = StubSelector::success();
    let transport = if body_stream {
        StubInferenceTransport::stream_error(error)
    } else {
        StubInferenceTransport::error(error)
    };
    let provider = provider(selector, transport).await;
    let mut stream = provider
        .execute(
            provider_request("xai"),
            context(CancellationToken::new(), None),
        )
        .await
        .expect("stream");
    next_provider_error(&mut stream).await
}

async fn next_provider_error(
    stream: &mut gateway_core::engine::provider::ProviderStream,
) -> ProviderError {
    loop {
        match stream.next().await.expect("error event") {
            Ok(event) => assert!(!event.has_client_event()),
            Err(error) => return error,
        }
    }
}

async fn collect_provider_state(
    stream: &mut gateway_core::engine::provider::ProviderStream,
) -> Option<ProviderSessionState> {
    let mut state = None;
    while let Some(event) = stream.next().await {
        let mut event = event.expect("successful Provider event");
        if let Some(update) = event.take_session_update() {
            state = Some(update);
        }
    }
    state
}

fn operation() -> Operation {
    let payload = ProtocolPayload::json_object(
        "openai",
        Map::from_iter([
            ("model".to_owned(), json!("client-model")),
            ("input".to_owned(), json!("hello")),
        ]),
    )
    .expect("OpenAI payload");
    Operation::Generate(GenerateRequest::from_protocol_payload(Vec::new(), payload))
}

fn compaction_operation() -> Operation {
    let payload = ProtocolPayload::json_object(
        "openai",
        Map::from_iter([
            ("model".to_owned(), json!("client-model")),
            (
                "input".to_owned(),
                json!([{"type": "message", "role": "user", "content": "history"}]),
            ),
            ("stream".to_owned(), json!(true)),
        ]),
    )
    .expect("OpenAI payload");
    Operation::CompactConversation(CompactConversationRequest::new(
        GenerateRequest::from_protocol_payload(Vec::new(), payload),
    ))
}

fn compaction_operation_with_state(state: ProviderSessionState) -> Operation {
    let payload = ProtocolPayload::json_object(
        "openai",
        Map::from_iter([
            ("model".to_owned(), json!("client-model")),
            (
                "input".to_owned(),
                json!([{
                    "type": "reasoning",
                    "summary": [],
                    "encrypted_content": "account-bound-reasoning"
                }, {
                    "type": "message",
                    "role": "user",
                    "content": "complete history"
                }]),
            ),
            ("stream".to_owned(), json!(true)),
        ]),
    )
    .expect("OpenAI payload");
    Operation::CompactConversation(CompactConversationRequest::new(
        GenerateRequest::from_protocol_payload(Vec::new(), payload)
            .with_provider_session_state(state),
    ))
}

fn provider_request(provider_kind: &str) -> ProviderRequest {
    provider_request_with_operation(provider_kind, operation())
}

fn provider_request_with_operation(provider_kind: &str, operation: Operation) -> ProviderRequest {
    let provider = ProviderKind::new(provider_kind).expect("provider");
    let provider_model = ProviderModel::new(
        provider.clone(),
        UpstreamModelId::new(MODEL).expect("model"),
        ModelCapabilities::new(
            BTreeSet::from([OperationKind::Generate, OperationKind::CompactConversation]),
            1_000_000,
            Some(131_072),
        ),
    );
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).expect("revision"),
        selection_policy(),
        vec![provider],
        vec![provider_model],
        vec![],
    )
    .expect("snapshot");
    let plan = snapshot
        .plan(
            &PublicModelId::new(MODEL).expect("model"),
            &operation,
            &RoutingContext::default(),
        )
        .expect("routing plan");
    ProviderRequest::new(operation, plan.candidates()[0].clone())
}

fn selection_policy() -> AccountSelectionPolicy {
    AccountSelectionPolicy::new(
        RotationStrategy::Smart,
        NonZeroU32::new(2).expect("limit"),
        Duration::ZERO,
    )
}

fn context(
    cancellation: CancellationToken,
    continuation: Option<ContinuationBinding>,
) -> AttemptContext {
    context_with_required(cancellation, continuation, None)
}

fn context_with_required(
    cancellation: CancellationToken,
    continuation: Option<ContinuationBinding>,
    required_account: Option<gateway_core::engine::credential::ProviderAccountId>,
) -> AttemptContext {
    context_with_recovery_state(cancellation, continuation, required_account, false)
}

fn context_with_recovery_state(
    cancellation: CancellationToken,
    continuation: Option<ContinuationBinding>,
    required_account: Option<gateway_core::engine::credential::ProviderAccountId>,
    recovery_attempted: bool,
) -> AttemptContext {
    let account_state_owner = continuation
        .as_ref()
        .and_then(ContinuationBinding::pinned)
        .map(gateway_core::engine::ProviderAccountStateOwner::from_continuation);
    AttemptContext::new(
        gateway_core::engine::RequestAttemptContext::new(
            ModelRequestId::new("req_xai").expect("request ID"),
            gateway_core::policy::ClientApiKeyId::new("key_xai_contract").expect("client key id"),
        ),
        NonZeroU32::new(1).expect("attempt"),
        SystemTime::now() + Duration::from_secs(30),
        selection_policy(),
        AccountAttemptContext::new(BTreeSet::new(), required_account, account_state_owner)
            .with_credential_recovery_attempted(recovery_attempted),
        continuation,
        cancellation,
    )
}

fn context_with_continuation_attempt(
    continuation: ContinuationBinding,
    attempt: ContinuationAttempt,
) -> AttemptContext {
    context(CancellationToken::new(), Some(continuation)).with_continuation_attempt(attempt)
}

fn operation_with_state(body: serde_json::Value, state: ProviderSessionState) -> Operation {
    let serde_json::Value::Object(body) = body else {
        panic!("request body must be an object");
    };
    let request = GenerateRequest::from_protocol_payload(
        Vec::new(),
        ProtocolPayload::json_object("openai", body).expect("OpenAI payload"),
    )
    .with_provider_session_state(state);
    Operation::Generate(request)
}

#[tokio::test]
async fn execute_forwards_required_account_to_selector() {
    let selector = StubSelector::success();
    let transport = StubInferenceTransport::success();
    let provider = provider(selector.clone(), transport).await;
    let required = account_id("provider");
    let stream = provider
        .execute(
            provider_request("xai"),
            context_with_required(CancellationToken::new(), None, Some(required.clone())),
        )
        .await
        .expect("required account stream");

    assert_eq!(stream.metadata().provider_account_id(), Some(&required));
    assert_eq!(
        selector
            .required_accounts
            .lock()
            .expect("required accounts")
            .as_slice(),
        &[Some(required)]
    );
}

#[tokio::test]
async fn execute_returns_cold_stream_and_records_selected_account() {
    let selector = StubSelector::success();
    let transport = StubInferenceTransport::success();
    let provider = provider(selector, transport.clone()).await;
    let mut stream = provider
        .execute(
            provider_request("xai"),
            context(CancellationToken::new(), None),
        )
        .await
        .expect("provider stream");
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        stream.metadata().provider_account_id(),
        Some(&account_id("provider"))
    );
    let events = stream.by_ref().collect::<Vec<_>>().await;
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    assert!(events.iter().all(Result::is_ok));
    let observation = events[0]
        .as_ref()
        .expect("response observation")
        .response_observation()
        .expect("transport facts");
    assert_eq!(observation.transport().as_str(), "http_sse");
    assert_eq!(observation.http_version(), Some(UpstreamHttpVersion::Http2));
    assert_eq!(observation.status_code(), Some(200));
    assert!(
        events
            .iter()
            .filter_map(|event| event.as_ref().ok())
            .flat_map(|event| event.canonical_facts())
            .any(|event| matches!(event, GatewayEvent::Completed(_)))
    );
}

#[tokio::test]
async fn compaction_stream_should_publish_only_typed_summary_and_accounting() {
    let summary = valid_compaction_summary("validated summary");
    let transport = StubInferenceTransport::sequence([InferenceMode::SuccessBody(compaction_sse(
        &summary,
        Some("private reasoning"),
    ))]);
    let provider = provider(StubSelector::success(), transport).await;
    let mut stream = provider
        .execute(
            provider_request_with_operation("xai", compaction_operation()),
            context(CancellationToken::new(), None),
        )
        .await
        .expect("compaction stream");
    let events = stream.by_ref().collect::<Vec<_>>().await;
    let facts = events
        .into_iter()
        .map(|event| event.expect("successful compaction event"))
        .flat_map(|event| event.into_parts().0)
        .collect::<Vec<_>>();

    assert!(matches!(
        facts.as_slice(),
        [
            GatewayEvent::Started(_),
            GatewayEvent::CompactionOutput(_),
            GatewayEvent::Usage(_),
            GatewayEvent::CalculatedCost(_),
            GatewayEvent::Completed(_),
        ]
    ));
}

#[tokio::test]
async fn compaction_should_pin_recorded_account_and_forward_session_headers() {
    let state = ProviderSessionState::new(
        "xai",
        Map::from_iter([
            ("account_id".to_owned(), json!("acct_provider")),
            ("session_id".to_owned(), json!("cache-session")),
            (
                "transcript".to_owned(),
                json!([{"client_input":{"type":"message","role":"user","content":"must-not-be-replayed"}}]),
            ),
        ]),
    )
    .expect("session state");
    let selector = StubSelector::success();
    let transport = StubInferenceTransport::sequence([InferenceMode::SuccessBody(compaction_sse(
        &valid_compaction_summary("session owner"),
        None,
    ))]);
    let provider = provider(selector.clone(), transport.clone()).await;
    let mut stream = provider
        .execute(
            provider_request_with_operation("xai", compaction_operation_with_state(state)),
            context(CancellationToken::new(), None),
        )
        .await
        .expect("compaction stream");
    while stream.next().await.is_some() {}

    assert_eq!(
        selector
            .required_accounts
            .lock()
            .expect("required accounts")
            .as_slice(),
        &[Some(account_id("provider"))]
    );
    let requests = transport.requests.lock().expect("requests");
    let request = &requests[0];
    let header = |name: &str| {
        request
            .headers()
            .iter()
            .find(|header| header.name().eq_ignore_ascii_case(name))
            .map(|header| header.value().expose())
    };
    assert_eq!(header("x-grok-conv-id"), Some("cache-session"));
    assert_eq!(header("x-grok-session-id"), Some("cache-session"));
    let body: serde_json::Value = serde_json::from_slice(request.body()).expect("request body");
    assert!(body.get("previous_response_id").is_none());
    assert!(!body.to_string().contains("must-not-be-replayed"));
    assert!(body.to_string().contains("account-bound-reasoning"));
}

#[tokio::test]
async fn compaction_should_accept_summary_when_upstream_stream_ends_without_terminal() {
    let transport = StubInferenceTransport::sequence([InferenceMode::SuccessBody(
        compaction_sse_without_terminal(&valid_compaction_summary("usable eof summary")),
    )]);
    let provider = provider(StubSelector::success(), transport).await;
    let mut stream = provider
        .execute(
            provider_request_with_operation("xai", compaction_operation()),
            context(CancellationToken::new(), None),
        )
        .await
        .expect("compaction stream");
    let facts = stream
        .by_ref()
        .map(|event| event.expect("successful compaction event"))
        .flat_map(|event| stream::iter(event.into_parts().0))
        .collect::<Vec<_>>()
        .await;

    assert!(matches!(
        facts.as_slice(),
        [
            GatewayEvent::Started(_),
            GatewayEvent::CompactionOutput(_),
            GatewayEvent::Completed(_),
        ]
    ));
}

#[tokio::test]
async fn compaction_stream_should_exclude_reasoning_from_plaintext_summary() {
    let summary = valid_compaction_summary("continuation marker");
    let transport = StubInferenceTransport::sequence([InferenceMode::SuccessBody(compaction_sse(
        &summary,
        Some("private reasoning"),
    ))]);
    let provider = provider(StubSelector::success(), transport).await;
    let mut stream = provider
        .execute(
            provider_request_with_operation("xai", compaction_operation()),
            context(CancellationToken::new(), None),
        )
        .await
        .expect("compaction stream");
    let events = stream.by_ref().collect::<Vec<_>>().await;
    let output = events
        .iter()
        .map(|event| event.as_ref().expect("successful compaction event"))
        .flat_map(|event| event.canonical_facts())
        .find_map(|event| match event {
            GatewayEvent::CompactionOutput(output) => Some(output),
            _ => None,
        })
        .expect("typed compaction output");

    assert!(output.summary().as_str().contains("continuation marker"));
    assert!(!output.summary().as_str().contains("private reasoning"));
}

#[tokio::test]
async fn compaction_stream_should_reject_degenerate_summary_before_commit() {
    let transport = StubInferenceTransport::sequence([InferenceMode::SuccessBody(compaction_sse(
        "<summary>too short</summary>",
        None,
    ))]);
    let provider = provider(StubSelector::success(), transport).await;
    let mut stream = provider
        .execute(
            provider_request_with_operation("xai", compaction_operation()),
            context(CancellationToken::new(), None),
        )
        .await
        .expect("compaction stream");

    let error = next_provider_error(&mut stream).await;

    assert_eq!(error.kind(), ProviderErrorKind::Protocol);
    assert!(error.replay_is_safe());
    assert!(error.retries_same_account());
}

#[tokio::test]
async fn compaction_transport_failure_should_retry_only_the_same_account() {
    let transport = StubInferenceTransport::stream_error(GrokInferenceTransportError::new(
        GrokInferenceTransportErrorKind::Transport,
        UpstreamSendState::Sent,
    ));
    let provider = provider(StubSelector::success(), transport).await;
    let mut stream = provider
        .execute(
            provider_request_with_operation("xai", compaction_operation()),
            context(CancellationToken::new(), None),
        )
        .await
        .expect("compaction stream");

    let error = next_provider_error(&mut stream).await;

    assert_eq!(error.kind(), ProviderErrorKind::Transport);
    assert!(error.replay_is_safe());
    assert!(error.retries_same_account());
}

#[tokio::test]
async fn compaction_protocol_stream_failure_should_retry_only_the_same_account() {
    let transport =
        StubInferenceTransport::sequence([InferenceMode::SuccessBody(malformed_compaction_sse())]);
    let provider = provider(StubSelector::success(), transport).await;
    let mut stream = provider
        .execute(
            provider_request_with_operation("xai", compaction_operation()),
            context(CancellationToken::new(), None),
        )
        .await
        .expect("compaction stream");

    let error = next_provider_error(&mut stream).await;

    assert_eq!(error.kind(), ProviderErrorKind::Protocol);
    assert!(error.replay_is_safe());
    assert!(error.retries_same_account());
}

#[tokio::test]
async fn inference_request_uses_oauth_headers_and_no_api_key() {
    let transport = StubInferenceTransport::success();
    let provider = provider(StubSelector::success(), transport.clone()).await;
    let mut stream = provider
        .execute(
            provider_request("xai"),
            context(CancellationToken::new(), None),
        )
        .await
        .expect("stream");
    while stream.next().await.is_some() {}
    let requests = transport.requests.lock().expect("requests");
    let request = &requests[0];
    assert_eq!(
        request.endpoint().as_str(),
        "https://cli-chat-proxy.grok.com/v1/responses"
    );
    assert!(
        request
            .headers()
            .iter()
            .any(|header| header.name() == "authorization")
    );
    assert!(
        !request
            .headers()
            .iter()
            .any(|header| header.name() == "x-api-key")
    );
    drop(provider);
}

#[tokio::test]
async fn unauthorized_transport_feedback_is_bound_to_selected_account() {
    let selector = StubSelector::success();
    let transport = StubInferenceTransport::error(GrokInferenceTransportError::new(
        GrokInferenceTransportErrorKind::Unauthorized,
        UpstreamSendState::Sent,
    ));
    let provider = provider(selector.clone(), transport).await;
    let mut stream = provider
        .execute(
            provider_request("xai"),
            context_with_recovery_state(CancellationToken::new(), None, None, true),
        )
        .await
        .expect("stream");
    let error = next_provider_error(&mut stream).await;
    assert_eq!(error.kind(), ProviderErrorKind::Unauthorized);
    assert_eq!(
        selector.feedback.lock().expect("feedback").as_slice(),
        &[GrokCredentialFailure::Unauthorized]
    );
}

#[tokio::test]
async fn first_unauthorized_should_refresh_and_request_one_same_account_retry() {
    let selector = StubSelector::success();
    let transport = StubInferenceTransport::error(
        GrokInferenceTransportError::new(
            GrokInferenceTransportErrorKind::Unauthorized,
            UpstreamSendState::Sent,
        )
        .with_status(401)
        .with_credential_recovery(),
    );
    let recovery = StubRecovery::new(GrokCredentialRecoveryOutcome::Recovered);
    let provider = provider_with_recovery(selector.clone(), transport, recovery.clone()).await;
    let mut stream = provider
        .execute(
            provider_request("xai"),
            context(CancellationToken::new(), None),
        )
        .await
        .expect("stream");

    let error = next_provider_error(&mut stream).await;

    assert_eq!(error.kind(), ProviderErrorKind::Unauthorized);
    assert!(error.replay_is_safe());
    assert!(error.retries_same_account());
    assert_eq!(recovery.calls.load(Ordering::SeqCst), 1);
    assert!(selector.feedback.lock().expect("feedback").is_empty());
}

#[tokio::test]
async fn explicit_http_429_marks_provider_error_replay_safe() {
    let error = mapped_transport_error(
        GrokInferenceTransportError::new(
            GrokInferenceTransportErrorKind::RateLimited,
            UpstreamSendState::Sent,
        )
        .with_status(429),
        false,
    )
    .await;

    assert_eq!(error.upstream_status(), Some(429));
    assert!(error.replay_is_safe());
}

#[tokio::test]
async fn explicit_http_408_does_not_mark_provider_error_replay_safe() {
    let error = mapped_transport_error(
        GrokInferenceTransportError::new(
            GrokInferenceTransportErrorKind::Timeout,
            UpstreamSendState::Sent,
        )
        .with_status(408),
        false,
    )
    .await;

    assert_eq!(error.kind(), ProviderErrorKind::Timeout);
    assert!(!error.replay_is_safe());
}

#[tokio::test]
async fn generic_http_403_does_not_mutate_or_switch_credential() {
    let selector = StubSelector::success();
    let transport = StubInferenceTransport::error(
        GrokInferenceTransportError::new(
            GrokInferenceTransportErrorKind::PermissionDenied,
            UpstreamSendState::Sent,
        )
        .with_status(403),
    );
    let provider = provider(selector.clone(), transport.clone()).await;
    let mut stream = provider
        .execute(
            provider_request("xai"),
            context(CancellationToken::new(), None),
        )
        .await
        .expect("stream");
    let error = next_provider_error(&mut stream).await;

    assert_eq!(error.kind(), ProviderErrorKind::PermissionDenied);
    assert!(!error.replay_is_safe());
    assert_eq!(selector.calls.load(Ordering::SeqCst), 1);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    assert!(selector.feedback.lock().expect("feedback").is_empty());
}

#[tokio::test]
async fn generic_http_500_does_not_mark_provider_error_replay_safe() {
    let error = mapped_transport_error(
        GrokInferenceTransportError::new(
            GrokInferenceTransportErrorKind::Unavailable,
            UpstreamSendState::Sent,
        )
        .with_status(500),
        false,
    )
    .await;

    assert_eq!(error.kind(), ProviderErrorKind::Unavailable);
    assert!(!error.replay_is_safe());
}

#[tokio::test]
async fn body_stream_error_does_not_mark_provider_error_replay_safe() {
    let error = mapped_transport_error(
        GrokInferenceTransportError::new(
            GrokInferenceTransportErrorKind::RateLimited,
            UpstreamSendState::Sent,
        )
        .with_status(429),
        true,
    )
    .await;

    assert_eq!(error.upstream_status(), Some(429));
    assert!(!error.replay_is_safe());
}

#[tokio::test]
async fn accepted_stream_without_terminal_marks_only_the_selected_account_interrupted() {
    let selector = StubSelector::success();
    let transport = StubInferenceTransport::stream_error(GrokInferenceTransportError::new(
        GrokInferenceTransportErrorKind::Transport,
        UpstreamSendState::Sent,
    ));
    let provider = provider(selector.clone(), transport).await;
    let mut stream = provider
        .execute(
            provider_request("xai"),
            context(CancellationToken::new(), None),
        )
        .await
        .expect("stream");

    let error = next_provider_error(&mut stream).await;

    assert_eq!(error.kind(), ProviderErrorKind::Transport);
    assert_eq!(
        selector.feedback.lock().expect("feedback").as_slice(),
        &[GrokCredentialFailure::StreamInterrupted]
    );
}

#[tokio::test]
async fn selector_capacity_failure_occurs_before_visible_upstream_send() {
    let selector = StubSelector::failing(GrokSessionSelectorError::CapacityUnavailable {
        retry_after: Some(Duration::from_millis(20)),
    });
    let transport = StubInferenceTransport::success();
    let provider = provider(selector, transport.clone()).await;
    let error = match provider
        .execute(
            provider_request("xai"),
            context(CancellationToken::new(), None),
        )
        .await
    {
        Ok(_) => panic!("capacity must fail"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), ProviderErrorKind::Unavailable);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn native_previous_response_is_rejected_before_selection() {
    let selector = StubSelector::success();
    let provider = provider(selector.clone(), StubInferenceTransport::success()).await;
    let pin = NativeContinuationPin::new(
        PreviousResponseId::new("resp_previous").expect("response ID"),
        SafeUpstreamValue::new("resp_upstream_previous").expect("upstream response ID"),
        ProviderKind::new("openai").expect("provider"),
        account_id("provider"),
    );
    let error = match provider
        .execute(
            provider_request("xai"),
            context(
                CancellationToken::new(),
                Some(ContinuationBinding::Pinned(pin)),
            ),
        )
        .await
    {
        Ok(_) => panic!("continuation owned by another Provider must fail"),
        Err(error) => error,
    };

    assert_eq!(error.kind(), ProviderErrorKind::InvalidRequest);
    assert_eq!(
        error.continuation_failure(),
        Some(ContinuationFailure::HistoryUnavailable)
    );
    assert_eq!(selector.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn native_previous_response_pins_account_and_sends_upstream_handle() {
    let selector = StubSelector::success();
    let transport = StubInferenceTransport::success();
    let provider = provider(selector.clone(), transport.clone()).await;
    let pin = NativeContinuationPin::new(
        PreviousResponseId::new("resp_previous").expect("response ID"),
        SafeUpstreamValue::new("resp_upstream_previous").expect("upstream response ID"),
        ProviderKind::new("xai").expect("provider"),
        account_id("provider"),
    );
    let mut stream = provider
        .execute(
            provider_request("xai"),
            context(
                CancellationToken::new(),
                Some(ContinuationBinding::Pinned(pin)),
            ),
        )
        .await
        .expect("native continuation stream");
    while stream.next().await.is_some() {}

    let requests = transport.requests.lock().expect("requests");
    let body: serde_json::Value = serde_json::from_slice(requests[0].body()).expect("request body");
    assert_eq!(
        body.get("previous_response_id")
            .and_then(serde_json::Value::as_str),
        Some("resp_upstream_previous")
    );
    assert_eq!(
        selector
            .required_accounts
            .lock()
            .expect("required accounts")
            .as_slice(),
        &[Some(account_id("provider"))]
    );
}

#[tokio::test]
async fn native_previous_response_does_not_allow_quota_or_rate_limit_account_rotation() {
    let transport = StubInferenceTransport::error(
        GrokInferenceTransportError::new(
            GrokInferenceTransportErrorKind::RateLimited,
            UpstreamSendState::Sent,
        )
        .with_status(429),
    );
    let provider = provider(StubSelector::success(), transport).await;
    let pin = NativeContinuationPin::new(
        PreviousResponseId::new("resp_previous").expect("response ID"),
        SafeUpstreamValue::new("resp_upstream_previous").expect("upstream response ID"),
        ProviderKind::new("xai").expect("provider"),
        account_id("provider"),
    );
    let mut stream = provider
        .execute(
            provider_request("xai"),
            context(
                CancellationToken::new(),
                Some(ContinuationBinding::Pinned(pin)),
            ),
        )
        .await
        .expect("native continuation stream");

    let error = next_provider_error(&mut stream).await;

    assert_eq!(error.kind(), ProviderErrorKind::RateLimited);
    assert!(!error.replay_is_safe());
}

#[tokio::test]
async fn external_previous_response_is_rejected_before_selection() {
    let selector = StubSelector::success();
    let provider = provider(selector.clone(), StubInferenceTransport::success()).await;
    let binding = ContinuationBinding::External(
        PreviousResponseId::new("external-provider-response").expect("response ID"),
    );
    let error = match provider
        .execute(
            provider_request("xai"),
            context(CancellationToken::new(), Some(binding)),
        )
        .await
    {
        Ok(_) => panic!("xAI external continuation must fail"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), ProviderErrorKind::InvalidRequest);
    assert_eq!(selector.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn connection_state_inherits_session_and_recovers_reasoning_on_pinned_account() {
    use base64::Engine as _;

    let encrypted_content =
        base64::engine::general_purpose::STANDARD_NO_PAD.encode((0_u8..=127).collect::<Vec<_>>());
    let transport = StubInferenceTransport::sequence([
        InferenceMode::SuccessBody(stateful_sse(&encrypted_content)),
        InferenceMode::Error(
            GrokInferenceTransportError::new(
                GrokInferenceTransportErrorKind::InvalidRequest,
                UpstreamSendState::Sent,
            )
            .with_status(400)
            .with_upstream_code(
                SafeUpstreamValue::new("reasoning_decode_failed").expect("safe code"),
            ),
        ),
        InferenceMode::Success,
    ]);
    let selector = StubSelector::success();
    let provider = provider(selector.clone(), transport.clone()).await;
    let first_operation = Operation::Generate(GenerateRequest::from_protocol_payload(
        Vec::new(),
        ProtocolPayload::json_object(
            "openai",
            Map::from_iter([
                ("model".to_owned(), json!("client-model")),
                ("prompt_cache_key".to_owned(), json!("conversation-42")),
                (
                    "input".to_owned(),
                    json!([{"type":"message","role":"user","content":"first"}]),
                ),
            ]),
        )
        .expect("OpenAI payload"),
    ));
    let mut first = provider
        .execute(
            provider_request_with_operation("xai", first_operation),
            context(CancellationToken::new(), None),
        )
        .await
        .expect("first stream");
    let state = collect_provider_state(&mut first)
        .await
        .expect("connection session state");

    let pin = NativeContinuationPin::new(
        PreviousResponseId::new("resp_gateway_first").expect("response ID"),
        SafeUpstreamValue::new("resp_state").expect("upstream response ID"),
        ProviderKind::new("xai").expect("provider"),
        account_id("provider"),
    );
    let continued_operation = operation_with_state(
        json!({
            "model": "client-model",
            "previous_response_id": "resp_gateway_first",
            "input": [{"type":"message","role":"user","content":"second"}]
        }),
        state.clone(),
    );
    let mut continued = provider
        .execute(
            provider_request_with_operation("xai", continued_operation),
            context(
                CancellationToken::new(),
                Some(ContinuationBinding::Pinned(pin.clone())),
            ),
        )
        .await
        .expect("continued stream");
    let error = next_provider_error(&mut continued).await;
    assert_eq!(
        error.continuation_failure(),
        Some(ContinuationFailure::HistoryUnavailable)
    );
    assert!(error.replay_is_safe());

    let recovery_operation = operation_with_state(
        json!({
            "model": "client-model",
            "previous_response_id": "resp_gateway_first",
            "input": [{"type":"message","role":"user","content":"second"}]
        }),
        state,
    );
    let mut recovered = provider
        .execute(
            provider_request_with_operation("xai", recovery_operation),
            context_with_continuation_attempt(
                ContinuationBinding::Pinned(pin),
                ContinuationAttempt::ReplayOwner,
            ),
        )
        .await
        .expect("recovery stream");
    while recovered.next().await.is_some() {}

    let requests = transport.requests.lock().expect("requests");
    let first_body: serde_json::Value =
        serde_json::from_slice(requests[0].body()).expect("first body");
    let continued_body: serde_json::Value =
        serde_json::from_slice(requests[1].body()).expect("continued body");
    let recovery_body: serde_json::Value =
        serde_json::from_slice(requests[2].body()).expect("recovery body");
    assert_eq!(
        continued_body
            .get("prompt_cache_key")
            .and_then(serde_json::Value::as_str),
        first_body
            .get("prompt_cache_key")
            .and_then(serde_json::Value::as_str)
    );
    assert_eq!(
        continued_body
            .get("previous_response_id")
            .and_then(serde_json::Value::as_str),
        Some("resp_state")
    );
    assert!(recovery_body.get("previous_response_id").is_none());
    assert!(recovery_body.get("prompt_cache_key").is_none());
    assert!(
        recovery_body
            .pointer("/input/1/encrypted_content")
            .is_none()
    );
    assert_eq!(selector.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn replay_owner_should_reencode_custom_apply_patch_call_for_grok() {
    let patch = concat!(
        "*** Begin Patch\n",
        "*** Update File: src/lib.rs\n",
        "@@\n",
        "-let value = \"old\\\\path\";\n",
        "+let value = \"new\\\\path\";\n",
        "*** End Patch\n",
    );
    let transport = StubInferenceTransport::sequence([
        InferenceMode::SuccessBody(custom_apply_patch_sse(patch)),
        InferenceMode::Success,
    ]);
    let provider = provider(StubSelector::success(), transport.clone()).await;
    let first_operation = Operation::Generate(GenerateRequest::from_protocol_payload(
        Vec::new(),
        ProtocolPayload::json_object(
            "openai",
            Map::from_iter([
                ("model".to_owned(), json!("client-model")),
                (
                    "input".to_owned(),
                    json!([{"type":"message","role":"user","content":"edit"}]),
                ),
                (
                    "tools".to_owned(),
                    json!([{"type":"custom","name":"apply_patch"}]),
                ),
            ]),
        )
        .expect("OpenAI payload"),
    ));
    let mut first = provider
        .execute(
            provider_request_with_operation("xai", first_operation),
            context(CancellationToken::new(), None),
        )
        .await
        .expect("first stream");
    let state = collect_provider_state(&mut first)
        .await
        .expect("connection session state");
    let pin = NativeContinuationPin::new(
        PreviousResponseId::new("resp_gateway_patch").expect("response ID"),
        SafeUpstreamValue::new("resp_custom_patch").expect("upstream response ID"),
        ProviderKind::new("xai").expect("provider"),
        account_id("provider"),
    );
    let replay_operation = operation_with_state(
        json!({
            "model": "client-model",
            "previous_response_id": "resp_gateway_patch",
            "tools": [{"type":"custom","name":"apply_patch"}],
            "input": [{"type":"message","role":"user","content":"continue"}]
        }),
        state,
    );
    let mut replay = provider
        .execute(
            provider_request_with_operation("xai", replay_operation),
            context_with_continuation_attempt(
                ContinuationBinding::Pinned(pin),
                ContinuationAttempt::ReplayOwner,
            ),
        )
        .await
        .expect("replay stream");
    while replay.next().await.is_some() {}

    let requests = transport.requests.lock().expect("requests");
    let replay_body: serde_json::Value =
        serde_json::from_slice(requests[1].body()).expect("replay body");
    let replay_input = replay_body
        .get("input")
        .and_then(serde_json::Value::as_array)
        .expect("replay input");
    let replayed_call = replay_input
        .iter()
        .find(|item| {
            item.get("call_id").and_then(serde_json::Value::as_str) == Some("call_custom_patch")
        })
        .expect("replayed custom call");
    let arguments = replayed_call
        .get("arguments")
        .and_then(serde_json::Value::as_str)
        .and_then(|arguments| serde_json::from_str::<serde_json::Value>(arguments).ok())
        .expect("replayed arguments");

    assert_eq!(replayed_call.get("type"), Some(&json!("function_call")));
    assert_eq!(replayed_call.get("name"), Some(&json!("apply_patch")));
    assert_eq!(replayed_call.get("input"), None);
    assert_eq!(arguments, json!({"patch": patch}));
    assert!(!replay_input.iter().any(|item| {
        item.get("type").and_then(serde_json::Value::as_str) == Some("custom_tool_call")
    }));
}

#[tokio::test]
async fn missing_native_response_should_be_replay_safe_for_the_same_account() {
    let transport = StubInferenceTransport::error(
        GrokInferenceTransportError::new(
            GrokInferenceTransportErrorKind::InvalidRequest,
            UpstreamSendState::Sent,
        )
        .with_status(404)
        .with_upstream_code(SafeUpstreamValue::new("not_found").expect("safe code")),
    );
    let provider = provider(StubSelector::success(), transport).await;
    let state = ProviderSessionState::new(
        "xai",
        Map::from_iter([
            ("account_id".to_owned(), json!("acct_provider")),
            ("session_id".to_owned(), json!("cache-session")),
            ("transcript".to_owned(), json!([])),
        ]),
    )
    .expect("session state");
    let pin = NativeContinuationPin::new(
        PreviousResponseId::new("resp_gateway_first").expect("response ID"),
        SafeUpstreamValue::new("resp_upstream_first").expect("upstream response ID"),
        ProviderKind::new("xai").expect("provider"),
        account_id("provider"),
    );
    let operation = operation_with_state(
        json!({
            "model": "client-model",
            "previous_response_id": "resp_gateway_first",
            "input": [{"type":"message","role":"user","content":"second"}]
        }),
        state,
    );
    let mut stream = provider
        .execute(
            provider_request_with_operation("xai", operation),
            context(
                CancellationToken::new(),
                Some(ContinuationBinding::Pinned(pin)),
            ),
        )
        .await
        .expect("native continuation stream");

    let error = next_provider_error(&mut stream).await;

    assert_eq!(
        error.continuation_failure(),
        Some(ContinuationFailure::HistoryUnavailable)
    );
    assert!(error.replay_is_safe());
}

#[tokio::test]
async fn cancellation_before_poll_never_calls_transport() {
    let transport = StubInferenceTransport::success();
    let provider = provider(StubSelector::success(), transport.clone()).await;
    let cancellation = CancellationToken::new();
    let mut stream = provider
        .execute(provider_request("xai"), context(cancellation.clone(), None))
        .await
        .expect("prepared stream");
    cancellation.cancel();
    let error = stream
        .next()
        .await
        .expect("cancel event")
        .expect_err("cancelled");
    assert_eq!(error.kind(), ProviderErrorKind::Cancelled);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn provider_rejects_target_owned_by_other_provider() {
    let selector = StubSelector::success();
    let provider = provider(selector.clone(), StubInferenceTransport::success()).await;
    let error = match provider
        .execute(
            provider_request("openai"),
            context(CancellationToken::new(), None),
        )
        .await
    {
        Ok(_) => panic!("provider mismatch must fail"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), ProviderErrorKind::InvalidRequest);
    assert_eq!(selector.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn provider_compiles_realtime_catalog_capabilities() {
    let provider = provider(StubSelector::success(), StubInferenceTransport::success()).await;
    let capabilities = provider
        .query_model_capabilities()
        .await
        .expect("capabilities");
    assert_eq!(capabilities.len(), 1);
    assert_eq!(capabilities[0].upstream_model().as_str(), MODEL);
    assert!(
        capabilities[0]
            .capabilities()
            .match_requirements(&gateway_core::operation::CapabilityRequirements::new(
                OperationKind::Generate
            ))
            .is_some()
    );
}

#[tokio::test]
async fn missing_catalog_tool_metadata_keeps_build_tools_routable() {
    let provider = provider_with_catalog_transport(
        StubSelector::success(),
        StubInferenceTransport::success(),
        StubRecovery::new(GrokCredentialRecoveryOutcome::Unavailable),
        Arc::new(CatalogWithoutToolMetadataTransport),
    )
    .await;
    let capabilities = provider
        .query_model_capabilities()
        .await
        .expect("capabilities");
    assert!(
        capabilities[0]
            .capabilities()
            .match_requirements(
                &gateway_core::operation::CapabilityRequirements::new(OperationKind::Generate,)
                    .require(Feature::Tools)
            )
            .is_some()
    );
}
