use std::collections::{BTreeSet, VecDeque};
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use futures::{StreamExt, stream};
use gateway_core::engine::continuation::{
    ContinuationBinding, NativeContinuationPin, NativeContinuationReuse, PreviousResponseId,
};
use gateway_core::engine::credential::{
    AccountSelectionPolicy, CredentialRevision, ProviderAccountStore, RotationStrategy,
};
use gateway_core::engine::provider::{Provider, ProviderRequest};
use gateway_core::engine::{
    AccountAttemptContext, AttemptContext, CancellationToken, ModelRequestId, UpstreamSendState,
};
use gateway_core::error::{ProviderError, ProviderErrorKind, SafeUpstreamValue};
use gateway_core::event::{GatewayEvent, UpstreamHttpVersion};
use gateway_core::operation::{GenerateRequest, Operation, OperationKind, ProtocolPayload};
use gateway_core::routing::{
    ConfigRevision, InstanceHealth, ModelCapabilities, ProviderInstance, ProviderKind,
    ProviderModel, PublicModelId, RoutingContext, RuntimeSnapshot, UpstreamModelId,
};
use provider_xai::{
    GROK_CLI_BASE_URL, GrokBuildProvider, GrokCredentialCatalogCache, GrokCredentialCatalogService,
    GrokCredentialFailure, GrokCredentialFeedbackFuture, GrokCredentialRepository,
    GrokInferenceRequest, GrokInferenceResponse, GrokInferenceTransport,
    GrokInferenceTransportError, GrokInferenceTransportErrorKind, GrokInferenceTransportFuture,
    GrokModelCatalogRequest, GrokModelCatalogTransport, GrokModelCatalogTransportFuture,
    GrokModelCatalogTransportResponse, GrokSessionBinding, GrokSessionSelection,
    GrokSessionSelector, GrokSessionSelectorError, GrokSessionSelectorFuture, SecretValue,
    SelectedGrokSession,
};
use serde_json::{Map, json};

use crate::support::{
    MemoryGrokCatalogCache, MemoryProviderAccountStore, account_id, create_input, instance_id,
    seed_input,
};

const MODEL: &str = "grok-4.5";
const CATALOG_FIXTURE: &[u8] =
    include_bytes!("../transport/catalog/fixtures/official_grok_models_snapshot.json");
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

async fn provider(
    selector: Arc<StubSelector>,
    transport: Arc<StubInferenceTransport>,
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
    let catalog = Arc::new(GrokCredentialCatalogService::new(
        repository,
        Arc::new(StaticCatalogTransport),
        cache,
    ));
    GrokBuildProvider::new(selector, transport, catalog)
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

fn instance(provider: &str) -> ProviderInstance {
    ProviderInstance::new(
        instance_id(),
        ProviderKind::new(provider).expect("provider"),
        GROK_CLI_BASE_URL.to_owned(),
        true,
        InstanceHealth::Healthy,
    )
}

fn provider_request(provider_kind: &str) -> ProviderRequest {
    let operation = operation();
    let provider_model = ProviderModel::new(
        instance_id(),
        UpstreamModelId::new(MODEL).expect("model"),
        ModelCapabilities::new(
            BTreeSet::from([OperationKind::Generate]),
            1_000_000,
            Some(131_072),
        ),
    );
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).expect("revision"),
        selection_policy(),
        vec![instance(provider_kind)],
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
    let account_state_owner = continuation
        .as_ref()
        .and_then(ContinuationBinding::pinned)
        .map(gateway_core::engine::ProviderAccountStateOwner::from_continuation);
    AttemptContext::new(
        ModelRequestId::new("req_xai").expect("request ID"),
        NonZeroU32::new(1).expect("attempt"),
        SystemTime::now() + Duration::from_secs(30),
        selection_policy(),
        AccountAttemptContext::new(BTreeSet::new(), required_account, account_state_owner),
        continuation,
        cancellation,
    )
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
            context(CancellationToken::new(), None),
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
        ProviderKind::new("xai").expect("provider"),
        instance_id(),
        account_id("provider"),
        NativeContinuationReuse::Reusable,
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
        Ok(_) => panic!("xAI native continuation must fail"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), ProviderErrorKind::Unsupported);
    assert_eq!(selector.calls.load(Ordering::SeqCst), 0);
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
    assert_eq!(error.kind(), ProviderErrorKind::Unsupported);
    assert_eq!(selector.calls.load(Ordering::SeqCst), 0);
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
        .query_model_capabilities(&instance("xai"))
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
