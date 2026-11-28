//! 验证 xAI 原生执行的协议转换、账号选择与交付状态隔离

mod chat;
mod outbound;
mod scheduling;
mod selection_retry;
mod timing;

use gateway_core::engine::probe::AccountProbeTarget;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use futures::{StreamExt, future::BoxFuture, stream};
use gateway_core::account::{
    AccountFeedbackStats, AccountRuntimeSignals, AccountSelectionPolicy, CredentialRevision,
    CredentialState, ProviderAccountId, ProviderAccountStore, RotationStrategy,
};
use gateway_core::engine::continuation::{
    ContinuationBinding, NativeContinuationPin, PreviousResponseId,
};
use gateway_core::engine::execution::ClientTransport;
use gateway_core::engine::middleware::{
    FrozenMiddlewarePlan, MiddlewareContext, MiddlewareError, MiddlewareHeader, MiddlewareMount,
    MiddlewareNext, MiddlewarePlan, MiddlewareRequest, MiddlewareResponse,
};
use gateway_core::engine::provider::{Provider, ProviderRequest};
use gateway_core::engine::upstream_adapter::UpstreamAdapterSelection;
use gateway_core::engine::{
    AccountAttemptContext, AttemptContext, ContinuationAttempt, ModelRequestId,
};
use gateway_core::error::{
    ClientVisibleUpstreamError, ContinuationFailure, OpaqueUpstreamValue, ProviderError,
    ProviderErrorKind,
};
use gateway_core::event::{GatewayEvent, UpstreamHttpVersion};
use gateway_core::lifecycle::CancellationToken;
use gateway_core::operation::{
    Feature, GenerateRequest, Operation, OperationKind, ProtocolPayload, ProviderSessionState,
};
use gateway_core::policy::ClientApiKeyId;
use gateway_core::provider_ports::{
    ProviderLeaseAcquisition, ProviderLeasePort, ProviderLeaseRequest, ProviderSchedulingState,
    ProviderStoreError,
};
use gateway_core::routing::extensions::{ExtensionSetId, ExtensionSetLease, ExtensionSetReference};
use gateway_core::routing::{
    ClientRoutingScope, ConfigRevision, FrozenAccountScope, ModelCapabilities, ProviderKind,
    ProviderModel, PublicModelId, RoutingContext, RuntimeAccount, RuntimeAccountDirectory,
    RuntimeSnapshot, SupportLevel, UpstreamModelId,
};
use gateway_core::upstream::UpstreamSendState;
use provider_xai::{
    GrokAccountSessionSelector, GrokBillingRequest, GrokBillingTransport,
    GrokBillingTransportError, GrokBillingTransportErrorKind, GrokBillingTransportFuture,
    GrokBuildProvider, GrokCredentialCatalogCache, GrokCredentialFailure,
    GrokCredentialFeedbackFuture, GrokCredentialRecovery, GrokCredentialRecoveryOutcome,
    GrokCredentialRepository, GrokInferenceClientCacheStatus, GrokInferenceDnsObservation,
    GrokInferenceDnsSource, GrokInferenceRequest, GrokInferenceResponse, GrokInferenceTransport,
    GrokInferenceTransportError, GrokInferenceTransportErrorKind, GrokInferenceTransportFuture,
    GrokInferenceTransportMetrics, GrokModelCatalogRequest, GrokModelCatalogTransport,
    GrokModelCatalogTransportError, GrokModelCatalogTransportErrorKind,
    GrokModelCatalogTransportFuture, GrokModelCatalogTransportResponse, GrokSessionBinding,
    GrokSessionSelection, GrokSessionSelector, GrokSessionSelectorError, GrokSessionSelectorFuture,
    SecretValue, SelectedGrokSession, UpdateGrokCredentialState,
};
use serde_json::{Map, Value, json};

use crate::support::{
    MemoryCooldownPort, MemoryGrokCatalogCache, MemoryProviderAccountStore, account_id,
    create_input, seed_input,
};

const MODEL: &str = "grok-4.5";
const CATALOG_FIXTURE: &[u8] =
    include_bytes!("../../transport/catalog/fixtures/cli_proxy_models.json");

#[tokio::test]
async fn native_xai_rejects_generate_false_before_upstream_io_or_usage() {
    for store in [false, true] {
        let transport = StubInferenceTransport::success();
        let selector = StubSelector::success();
        let provider = provider(Arc::clone(&selector), transport.clone()).await;
        let request = Operation::Generate(GenerateRequest::from_protocol_payload(
            ProtocolPayload::json_object(
                "openai",
                object(json!({
                    "model": MODEL,
                    "input": "hello",
                    "generate": false,
                    "store": store
                })),
            )
            .unwrap(),
        ));
        let error = match provider
            .execute(
                provider_request_with_operation("xai", request),
                context(CancellationToken::new(), None),
            )
            .await
        {
            Ok(_) => panic!("non-generating requests must fail before a usage-producing stream"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), ProviderErrorKind::Unsupported);
        assert_eq!(error.send_state(), UpstreamSendState::NotSent);
        let detail = error
            .client_visible_upstream_error()
            .expect("prewarm rejection must explain the unsupported request");
        assert_eq!(detail.code(), Some("unsupported_prewarm"));
        assert_eq!(detail.error_type(), Some("invalid_request_error"));
        assert!(detail.message().contains("generate=false"));
        assert_eq!(selector.calls.load(Ordering::SeqCst), 0);
        assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
        assert!(transport.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn native_xai_rejects_translated_generate_false_before_upstream_io() {
    let transport = StubInferenceTransport::success();
    let selector = StubSelector::success();
    let provider = provider(Arc::clone(&selector), transport.clone()).await;
    let source = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("example-source", object(json!({"opaque": "source"})))
            .unwrap(),
    ));
    let translations = Arc::new(Mutex::new(0));
    let error = match provider
        .execute(
            provider_request_with_operation("xai", source),
            context_with_middleware(Arc::new(TranslationMiddleware {
                translations: Arc::clone(&translations),
                target: object(json!({"model": MODEL, "input": "hello", "generate": false})),
            })),
        )
        .await
    {
        Ok(_) => panic!("translated prewarm must not start generation"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), ProviderErrorKind::Unsupported);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    assert_eq!(*translations.lock().unwrap(), 1);
    assert_eq!(selector.calls.load(Ordering::SeqCst), 1);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
    assert!(transport.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn native_xai_preserves_generate_true_and_omitted_requests() {
    for generate in [None, Some(true)] {
        let transport = StubInferenceTransport::success();
        let provider = provider(StubSelector::success(), transport.clone()).await;
        let mut body = object(json!({"model": MODEL, "input": "hello"}));
        if let Some(generate) = generate {
            body.insert("generate".to_owned(), json!(generate));
        }
        let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
            ProtocolPayload::json_object("openai", body).unwrap(),
        ));
        let mut stream = provider
            .execute(
                provider_request_with_operation("xai", operation),
                context(CancellationToken::new(), None),
            )
            .await
            .expect("ordinary generation remains supported");
        let mut completed = false;
        while let Some(event) = stream.next().await {
            completed |= event
                .expect("normal upstream response")
                .into_parts()
                .0
                .iter()
                .any(|event| matches!(event, GatewayEvent::Completed(_)));
        }
        assert!(completed);
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
        let requests = transport.requests.lock().unwrap();
        let sent: Value = serde_json::from_slice(requests[0].body()).unwrap();
        assert_eq!(sent.get("generate").and_then(Value::as_bool), generate);
    }
}

#[tokio::test]
async fn native_xai_translates_a_non_native_source_before_encoding() {
    let transport = StubInferenceTransport::success();
    let selector = StubSelector::success();
    let provider = provider(Arc::clone(&selector), transport.clone()).await;
    let translations = Arc::new(Mutex::new(0_usize));
    let source = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "example-source",
            Map::from_iter([
                ("opaque".to_owned(), json!("source-only")),
                ("generate".to_owned(), json!(false)),
                ("input".to_owned(), json!([{"type":"compaction_trigger"}])),
                ("prompt_cache_key".to_owned(), json!("must-not-be-read")),
            ]),
        )
        .unwrap(),
    ));
    let mut stream = provider
        .execute(
            provider_request_with_operation("xai", source),
            context_with_middleware(Arc::new(TranslationMiddleware {
                translations: Arc::clone(&translations),
                target: Map::from_iter([
                    ("model".to_owned(), json!("ignored-by-forced-model")),
                    ("input".to_owned(), json!("translated")),
                    ("stream".to_owned(), json!(true)),
                ]),
            })),
        )
        .await
        .expect("registered translation should run before native encoding");
    assert_eq!(selector.calls.load(Ordering::SeqCst), 1);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
    while let Some(event) = stream.next().await {
        event.expect("translated request response");
    }
    assert_eq!(*translations.lock().unwrap(), 1);
    let requests = transport.requests.lock().unwrap();
    let sent: Value = serde_json::from_slice(requests[0].body()).unwrap();
    assert_eq!(sent["input"], "translated");
    assert_eq!(sent["model"], MODEL);
}

#[tokio::test]
async fn native_xai_rejects_missing_or_capability_expanding_translation_before_send() {
    let source = || {
        Operation::Generate(GenerateRequest::from_protocol_payload(
            ProtocolPayload::json_object(
                "example-source",
                Map::from_iter([("opaque".to_owned(), json!("source-only"))]),
            )
            .unwrap(),
        ))
    };

    let selector = StubSelector::success();
    let transport = StubInferenceTransport::success();
    let missing_provider = provider(Arc::clone(&selector), transport.clone()).await;
    let missing = match missing_provider
        .execute(
            provider_request_with_operation("xai", source()),
            context_with_middleware(Arc::new(PassThroughMiddleware)),
        )
        .await
    {
        Ok(_) => panic!("missing translation pair must be explicit"),
        Err(error) => error,
    };
    assert_eq!(missing.kind(), ProviderErrorKind::InvalidRequest);
    assert_eq!(missing.send_state(), UpstreamSendState::NotSent);
    assert_eq!(selector.calls.load(Ordering::SeqCst), 1);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);

    let selector = StubSelector::success();
    let transport = StubInferenceTransport::success();
    let expanded_provider = provider(Arc::clone(&selector), transport.clone()).await;
    let expanded = match expanded_provider
        .execute(
            provider_request_with_operation("xai", source()),
            context_with_middleware(Arc::new(TranslationMiddleware {
                translations: Arc::new(Mutex::new(0)),
                target: object(json!({
                    "model": "ignored-by-forced-model",
                    "input": "translated",
                    "tools": [{"type":"function", "name":"new_capability"}]
                })),
            })),
        )
        .await
    {
        Ok(_) => panic!("translation cannot expand routed capabilities"),
        Err(error) => error,
    };
    assert_eq!(expanded.kind(), ProviderErrorKind::InvalidRequest);
    assert_eq!(expanded.send_state(), UpstreamSendState::NotSent);
    assert_eq!(selector.calls.load(Ordering::SeqCst), 1);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn pass_through_middleware_projects_native_response_once() {
    let provider = provider(StubSelector::success(), StubInferenceTransport::success()).await;
    let mut stream = provider
        .execute(
            provider_request("xai"),
            context_with_middleware(Arc::new(PassThroughMiddleware)),
        )
        .await
        .expect("native response stream");

    let mut client_events = 0;
    while let Some(event) = stream.next().await {
        let event = event.expect("projected response event");
        if let Some(wire) = event.wire_event() {
            client_events += 1;
            assert_eq!(wire.protocol(), "openai");
        }
    }
    assert!(client_events > 0);
}

#[tokio::test]
async fn attempt_middleware_runs_once_before_native_encoding() {
    let transport = StubInferenceTransport::success();
    let native_provider = provider(StubSelector::success(), transport.clone()).await;
    let observed = Arc::new(Mutex::new(Vec::new()));
    let body = json!({
        "model": "client-model",
        "input": "hello",
        "service_tier": "priority",
    });
    let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", body.as_object().unwrap().clone()).unwrap(),
    ));
    let mut stream = native_provider
        .execute(
            provider_request_with_operation("xai", operation),
            context_with_middleware(Arc::new(RecordingMiddleware {
                observed: Arc::clone(&observed),
                replacement: None,
                request_headers: vec![
                    MiddlewareHeader::new(
                        "x-business-context",
                        Bytes::from_static(b"tenant-public"),
                    ),
                    MiddlewareHeader::new(
                        "x-business-context",
                        Bytes::from_static(b"trace-public"),
                    ),
                ],
            })),
        )
        .await
        .expect("native conversion should prepare the request");
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
    while let Some(event) = stream.next().await {
        event.expect("successful native response");
    }
    {
        let observed = observed.lock().unwrap();
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].0, "openai");
        assert_eq!(observed[0].1["service_tier"], "priority");
        let requests = transport.requests.lock().unwrap();
        let sent: Map<String, Value> = serde_json::from_slice(requests[0].body()).unwrap();
        assert_eq!(sent["service_tier"], "priority");
        assert_eq!(
            requests[0]
                .headers()
                .iter()
                .filter(|header| header.name() == "x-business-context")
                .map(|header| std::str::from_utf8(header.value()).unwrap())
                .collect::<Vec<_>>(),
            vec!["tenant-public", "trace-public"],
        );
    }

    let overridden_transport = StubInferenceTransport::success();
    let overridden_provider = provider(StubSelector::success(), overridden_transport.clone()).await;
    let mut overridden = overridden_provider
        .execute(
            provider_request("xai"),
            context_with_middleware(Arc::new(RecordingMiddleware {
                observed: Arc::default(),
                replacement: None,
                request_headers: vec![MiddlewareHeader::new(
                    "Authorization",
                    Bytes::from_static(b"Bearer plugin-value"),
                )],
            })),
        )
        .await
        .expect("header validation remains on the cold Provider stream");
    while let Some(event) = overridden.next().await {
        event.unwrap();
    }
    assert_eq!(overridden_transport.calls.load(Ordering::SeqCst), 1);
    let requests = overridden_transport.requests.lock().unwrap();
    let values = requests[0]
        .headers()
        .iter()
        .filter(|header| header.name().eq_ignore_ascii_case("authorization"))
        .map(|header| std::str::from_utf8(header.value()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(values, ["Bearer plugin-value"]);
    assert!(!format!("{:?}", requests[0].headers()).contains("Bearer plugin-value"));
}

#[tokio::test]
async fn attempt_middleware_can_change_reasoning_before_native_validation() {
    let transport = StubInferenceTransport::success();
    let provider = provider(StubSelector::success(), transport.clone()).await;
    let observed = Arc::new(Mutex::new(Vec::new()));
    let mut stream = provider
        .execute(
            provider_request_with_reasoning_operation(
                "xai",
                operation_with_reasoning_effort("minimal"),
            ),
            context_with_middleware(Arc::new(RecordingMiddleware {
                observed: Arc::clone(&observed),
                replacement: Some(("reasoning".to_owned(), json!({"effort":"xhigh"}))),
                request_headers: Vec::new(),
            })),
        )
        .await
        .expect("thinking policy should prepare the xAI stream");
    while let Some(event) = stream.next().await {
        event.expect("successful xAI response");
    }

    let requests = transport.requests.lock().unwrap();
    let body: Value = serde_json::from_slice(requests[0].body()).unwrap();
    assert_eq!(body["reasoning"]["effort"], "xhigh");
    assert_eq!(body["model"], MODEL);
    assert_eq!(
        observed.lock().unwrap()[0].1["reasoning"]["effort"],
        "minimal"
    );
}

fn observed_transport_metrics() -> GrokInferenceTransportMetrics {
    GrokInferenceTransportMetrics::default()
        .with_headers_ms(42)
        .with_client_cache_status(GrokInferenceClientCacheStatus::Miss)
        .with_dns(GrokInferenceDnsObservation::new(
            GrokInferenceDnsSource::System,
            7,
        ))
}

const CATALOG_WITHOUT_FEATURE_METADATA: &[u8] = br#"{
  "object": "list",
  "data": [
    {
      "id": "grok-catalog-entry",
      "model": "grok-catalog-entry",
      "context_window": 1000000,
      "max_completion_tokens": 131072,
      "api_backend": "responses",
      "supported_in_api": true,
      "supports_reasoning_effort": true
    }
  ]
}"#;
const CATALOG_WITH_REASONING_MENU: &[u8] = br#"{
  "object": "list",
  "data": [
    {
      "id": "grok-4.5",
      "model": "grok-4.5",
      "context_window": 500000,
      "max_completion_tokens": 131072,
      "api_backend": "responses",
      "reasoning_efforts": [
        {
          "value": "low"
        },
        {
          "value": "medium"
        },
        {
          "value": "high",
          "default": true
        },
        {
          "value": "xhigh"
        }
      ],
      "stream_tool_calls": true
    }
  ]
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

struct StubSelector {
    calls: AtomicUsize,
    feedback: Mutex<Vec<GrokCredentialFailure>>,
    error: Mutex<Option<GrokSessionSelectorError>>,
    required_accounts: Mutex<Vec<Option<gateway_core::account::ProviderAccountId>>>,
    upstream_models: Mutex<Vec<String>>,
}

impl StubSelector {
    fn success() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            feedback: Mutex::new(Vec::new()),
            error: Mutex::new(None),
            required_accounts: Mutex::new(Vec::new()),
            upstream_models: Mutex::new(Vec::new()),
        })
    }

    fn failing(error: GrokSessionSelectorError) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            feedback: Mutex::new(Vec::new()),
            error: Mutex::new(Some(error)),
            required_accounts: Mutex::new(Vec::new()),
            upstream_models: Mutex::new(Vec::new()),
        })
    }
}

impl GrokSessionSelector for StubSelector {
    fn select(&self, request: GrokSessionSelection) -> GrokSessionSelectorFuture<'_> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.upstream_models
            .lock()
            .expect("upstream models")
            .push(request.upstream_model().as_str().to_owned());
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
            .map_err(|_| GrokSessionSelectorError::InvalidSession(None))
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
    SuccessWithMetrics(GrokInferenceTransportMetrics),
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

    fn success_with_metrics(metrics: GrokInferenceTransportMetrics) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
            modes: Mutex::new(VecDeque::from([InferenceMode::SuccessWithMetrics(metrics)])),
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
                    Box::pin(stream::iter([Ok(bytes::Bytes::from_static(SUCCESS_SSE))])),
                    UpstreamHttpVersion::Http2,
                    200,
                    None,
                )),
                InferenceMode::SuccessWithMetrics(metrics) => Ok(GrokInferenceResponse::new(
                    Box::pin(stream::iter([Ok(bytes::Bytes::from_static(SUCCESS_SSE))])),
                    UpstreamHttpVersion::Http2,
                    200,
                    None,
                )
                .with_transport_metrics(metrics)),
                InferenceMode::SuccessBody(body) => Ok(GrokInferenceResponse::new(
                    Box::pin(stream::iter([Ok(bytes::Bytes::from(body))])),
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

struct CatalogWithoutFeatureMetadataTransport;

impl GrokModelCatalogTransport for CatalogWithoutFeatureMetadataTransport {
    fn execute(&self, _: GrokModelCatalogRequest) -> GrokModelCatalogTransportFuture<'_> {
        Box::pin(async {
            Ok(GrokModelCatalogTransportResponse::new(
                CATALOG_WITHOUT_FEATURE_METADATA,
                None,
            ))
        })
    }
}

struct CatalogWithReasoningMenuTransport;

impl GrokModelCatalogTransport for CatalogWithReasoningMenuTransport {
    fn execute(&self, _: GrokModelCatalogRequest) -> GrokModelCatalogTransportFuture<'_> {
        Box::pin(async {
            Ok(GrokModelCatalogTransportResponse::new(
                CATALOG_WITH_REASONING_MENU,
                None,
            ))
        })
    }
}

struct UnavailableCatalogTransport;

struct CatalogWithServerReasoningDefaultsTransport;

impl GrokModelCatalogTransport for CatalogWithServerReasoningDefaultsTransport {
    fn execute(&self, _: GrokModelCatalogRequest) -> GrokModelCatalogTransportFuture<'_> {
        Box::pin(async {
            Ok(GrokModelCatalogTransportResponse::new(
                br#"{"object":"list","data":[
                    {"model":"grok-4.7","api_backend":"responses","context_window":256000,"context_windows":[256000,500000],"reasoning_efforts":[{"value":"low"},{"value":"high"}]},
                    {"model":"grok-future","api_backend":"responses","reasoning_efforts":[{"value":"low"},{"value":"high"},{"value":"unknown","default":true}]}
                ]}"#,
                None,
            ))
        })
    }
}

impl GrokModelCatalogTransport for UnavailableCatalogTransport {
    fn execute(&self, _: GrokModelCatalogRequest) -> GrokModelCatalogTransportFuture<'_> {
        Box::pin(async {
            Err(GrokModelCatalogTransportError::new(
                GrokModelCatalogTransportErrorKind::Unavailable,
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
        _: &gateway_core::account::ProviderAccountId,
        _: CredentialRevision,
    ) -> GrokCredentialRecoveryOutcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.outcome
    }
}

async fn provider(
    selector: Arc<StubSelector>,
    transport: Arc<StubInferenceTransport>,
) -> Arc<GrokBuildProvider> {
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
) -> Arc<GrokBuildProvider> {
    provider_with_catalog_transport(
        selector,
        transport,
        recovery,
        Arc::new(StaticCatalogTransport),
    )
    .await
}

async fn provider_with_catalog_transport(
    selector: Arc<dyn GrokSessionSelector>,
    transport: Arc<dyn GrokInferenceTransport>,
    recovery: Arc<StubRecovery>,
    catalog_transport: Arc<dyn GrokModelCatalogTransport>,
) -> Arc<GrokBuildProvider> {
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
    Arc::new(
        GrokBuildProvider::new(
            selector,
            transport,
            catalog,
            recovery,
            Arc::new(AccountFeedbackStats::default()),
            crate::support::xai_wire_profile(),
        )
        .expect("official xAI provider configuration"),
    )
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
    Operation::Generate(GenerateRequest::from_protocol_payload(payload))
}

fn operation_with_reasoning_effort(effort: &str) -> Operation {
    let payload = ProtocolPayload::json_object(
        "openai",
        Map::from_iter([
            ("model".to_owned(), json!("client-model")),
            ("input".to_owned(), json!("hello")),
            ("reasoning".to_owned(), json!({"effort": effort})),
        ]),
    )
    .expect("OpenAI payload");
    Operation::Generate(GenerateRequest::from_protocol_payload(payload))
}

fn object(value: serde_json::Value) -> Map<String, serde_json::Value> {
    let serde_json::Value::Object(object) = value else {
        panic!("request body must be an object");
    };
    object
}

fn provider_request(provider_kind: &str) -> ProviderRequest {
    provider_request_with_operation(provider_kind, operation())
}

fn provider_request_with_operation(provider_kind: &str, operation: Operation) -> ProviderRequest {
    provider_request_with_model_capabilities(provider_kind, MODEL, operation, false)
}

fn provider_request_with_reasoning_operation(
    provider_kind: &str,
    operation: Operation,
) -> ProviderRequest {
    provider_request_with_model_capabilities(provider_kind, MODEL, operation, true)
}

fn provider_request_with_upstream_model(
    provider_kind: &str,
    upstream_model: &str,
    operation: Operation,
) -> ProviderRequest {
    provider_request_with_model_capabilities(provider_kind, upstream_model, operation, false)
}

fn provider_request_with_model_capabilities(
    provider_kind: &str,
    upstream_model: &str,
    operation: Operation,
    supports_reasoning: bool,
) -> ProviderRequest {
    let provider = ProviderKind::new(provider_kind).expect("provider");
    let mut capabilities =
        ModelCapabilities::new(BTreeSet::from([OperationKind::Generate]), Some(131_072))
            .with_feature(Feature::Tools, SupportLevel::Native)
            .with_feature(Feature::NativeContinuation, SupportLevel::Native);
    if supports_reasoning {
        capabilities = capabilities.with_feature(Feature::Reasoning, SupportLevel::Native);
    }
    let provider_model = ProviderModel::new(
        provider.clone(),
        UpstreamModelId::new(upstream_model).expect("model"),
        capabilities,
    );
    let account_scope = Arc::new(FrozenAccountScope::new(
        Arc::new(RuntimeAccountDirectory::new(BTreeMap::from([(
            account_id("provider"),
            RuntimeAccount::new(provider.clone(), BTreeSet::new()),
        )]))),
        ClientRoutingScope::all_accounts(),
    ));
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).expect("revision"),
        gateway_core::settings::SettingsValues::new(2, "smart", Default::default(), None, None),
        vec![provider],
        vec![provider_model],
        vec![],
    )
    .expect("snapshot");
    let plan = snapshot
        .plan(
            &PublicModelId::new(upstream_model).expect("model"),
            &operation,
            account_scope,
            &RoutingContext::default(),
        )
        .expect("routing plan");
    ProviderRequest::new(operation, plan.candidates()[0].clone())
}

fn selection_policy() -> AccountSelectionPolicy {
    AccountSelectionPolicy::new(RotationStrategy::Smart, NonZeroU32::new(2).expect("limit"))
}

#[derive(Debug)]
struct TranslationMiddleware {
    translations: Arc<Mutex<usize>>,
    target: Map<String, Value>,
}

impl MiddlewarePlan for TranslationMiddleware {
    fn handle(
        &self,
        context: MiddlewareContext,
        request: MiddlewareRequest,
        next: MiddlewareNext,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        assert_eq!(context.mount(), MiddlewareMount::Attempt);
        assert!(context.account_id().is_some());
        *self.translations.lock().unwrap() += 1;
        let (_, headers, _) = request.into_parts();
        next.run(MiddlewareRequest::new(
            "openai",
            headers,
            Bytes::from(serde_json::to_vec(&self.target).unwrap()),
        ))
    }
}

#[derive(Debug)]
struct PassThroughMiddleware;

impl MiddlewarePlan for PassThroughMiddleware {
    fn handle(
        &self,
        _: MiddlewareContext,
        request: MiddlewareRequest,
        next: MiddlewareNext,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        next.run(request)
    }
}

type ObservedRequest = (String, Map<String, Value>);

#[derive(Debug)]
struct RecordingMiddleware {
    observed: Arc<Mutex<Vec<ObservedRequest>>>,
    replacement: Option<(String, Value)>,
    request_headers: Vec<MiddlewareHeader>,
}

impl MiddlewarePlan for RecordingMiddleware {
    fn handle(
        &self,
        context: MiddlewareContext,
        request: MiddlewareRequest,
        next: MiddlewareNext,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        assert_eq!(context.mount(), MiddlewareMount::Attempt);
        let (protocol, mut headers, bytes) = request.into_parts();
        let mut body: Map<String, Value> = serde_json::from_slice(&bytes).unwrap();
        self.observed
            .lock()
            .unwrap()
            .push((protocol.clone(), body.clone()));
        if let Some((field, value)) = &self.replacement {
            body.insert(field.clone(), value.clone());
        }
        headers.extend(self.request_headers.clone());
        next.run(MiddlewareRequest::new(
            protocol,
            headers,
            Bytes::from(serde_json::to_vec(&body).unwrap()),
        ))
    }
}

#[derive(Debug)]
struct TestExtensionLease;

impl ExtensionSetLease for TestExtensionLease {
    fn is_ready(&self) -> bool {
        true
    }
}

fn context_with_middleware(plan: Arc<dyn MiddlewarePlan>) -> AttemptContext {
    context_with_middleware_and_cancellation(plan, CancellationToken::new())
}

fn context_with_middleware_and_cancellation(
    plan: Arc<dyn MiddlewarePlan>,
    cancellation: CancellationToken,
) -> AttemptContext {
    let plan = FrozenMiddlewarePlan::new(
        plan,
        ExtensionSetReference::new(
            ExtensionSetId::new("native-xai-middleware".to_owned()).unwrap(),
            Arc::new(TestExtensionLease),
        ),
    );
    AttemptContext::new(
        gateway_core::engine::RequestAttemptContext::new(
            ModelRequestId::new("req_xai_middleware").unwrap(),
            ClientApiKeyId::new("key_xai_contract").unwrap(),
        )
        .with_middleware(
            Some(plan),
            Arc::from([]),
            "/v1/responses".to_owned(),
            ClientTransport::HttpSse,
        ),
        NonZeroU32::MIN,
        SystemTime::now() + Duration::from_secs(30),
        selection_policy(),
        AccountAttemptContext::new(BTreeSet::new(), None, None),
        None,
        cancellation,
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
    required_account: Option<gateway_core::account::ProviderAccountId>,
) -> AttemptContext {
    context_with_recovery_state(cancellation, continuation, required_account, false)
}

fn context_with_recovery_state(
    cancellation: CancellationToken,
    continuation: Option<ContinuationBinding>,
    required_account: Option<gateway_core::account::ProviderAccountId>,
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

async fn execute_successfully(provider: &Arc<GrokBuildProvider>, operation: Operation) {
    let mut stream = Arc::clone(provider)
        .execute(
            provider_request_with_operation("xai", operation),
            context(CancellationToken::new(), None),
        )
        .await
        .expect("provider stream");
    let events = stream.by_ref().collect::<Vec<_>>().await;
    assert!(events.iter().all(Result::is_ok));
}

/// 与 Postgres 调度列表一致：常规选择不返回停用账号，只有诊断能取回
struct DiagnosticLeasePort;

impl ProviderLeasePort for DiagnosticLeasePort {
    fn load_state<'a>(
        &'a self,
        _: &'a ClientApiKeyId,
        _: &'a ProviderKind,
        account_ids: &'a [gateway_core::account::ProviderAccountId],
        _pool: gateway_core::provider_ports::ProviderConcurrencyPool,
    ) -> futures::future::BoxFuture<'a, Result<ProviderSchedulingState, ProviderStoreError>> {
        Box::pin(async move {
            Ok(ProviderSchedulingState::new(
                account_ids
                    .iter()
                    .cloned()
                    .map(|account_id| {
                        (
                            account_id,
                            AccountRuntimeSignals {
                                in_flight: 0,
                                last_started_at: None,
                                quota_reset_at: None,
                                quota_reset_windows: None,
                                quota_remaining_rank: None,
                                cooldown: None,
                                failure_rate_basis_points: None,
                                first_output_latency_ms: None,
                            },
                        )
                    })
                    .collect(),
                0,
            ))
        })
    }

    fn try_acquire(
        &self,
        request: ProviderLeaseRequest,
    ) -> futures::future::BoxFuture<'_, Result<ProviderLeaseAcquisition, ProviderStoreError>> {
        Box::pin(async move {
            let ProviderLeaseRequest::Scheduling(_) = request else {
                panic!("expected scheduling lease request");
            };
            Ok(ProviderLeaseAcquisition::Acquired(Box::new(())))
        })
    }
}

struct UnavailableBillingTransport;

impl GrokBillingTransport for UnavailableBillingTransport {
    fn execute(&self, _: GrokBillingRequest) -> GrokBillingTransportFuture<'_> {
        Box::pin(async {
            Err(GrokBillingTransportError::new(
                GrokBillingTransportErrorKind::Unavailable,
            ))
        })
    }
}

#[tokio::test]
async fn disabled_account_diagnostic_selects_the_pinned_account_without_state_writes() {
    let store = MemoryProviderAccountStore::shared();
    let account_store: Arc<dyn ProviderAccountStore> = store.clone();
    let repository = GrokCredentialRepository::new(account_store);
    let input = create_input("disabled-diagnostic", "subject-disabled-diagnostic");
    seed_input(&store, &input).await.expect("seed account");
    repository
        .update_state(&UpdateGrokCredentialState {
            account_id: input.account_id.clone(),
            expected_revision: CredentialRevision::new(1).expect("revision"),
            credential_state: CredentialState::Ready,
            error_reason: None,
            error_message: None,
            observed_at: chrono::Utc::now(),
        })
        .await
        .expect("ready account");
    store
        .set_enabled(&input.account_id, false)
        .await
        .expect("disable account");

    let cache: Arc<dyn GrokCredentialCatalogCache> = MemoryGrokCatalogCache::shared();
    let quota = Arc::new(crate::support::grok_quota_service(
        repository.clone(),
        Arc::new(UnavailableBillingTransport),
    ));
    let selector = GrokAccountSessionSelector::new(
        ProviderKind::new("xai").expect("provider"),
        repository.clone(),
        cache,
        quota,
        Arc::new(DiagnosticLeasePort),
        Arc::new(MemoryCooldownPort::default()),
        Arc::new(AccountFeedbackStats::default()),
        Arc::new(crate::support::TestSessionAffinity::default()),
    );
    let provider = Arc::new(
        GrokBuildProvider::new(
            Arc::new(selector),
            StubInferenceTransport::success(),
            Arc::new(crate::support::grok_catalog_service(
                repository,
                Arc::new(StaticCatalogTransport),
                MemoryGrokCatalogCache::shared(),
            )),
            StubRecovery::new(GrokCredentialRecoveryOutcome::Unavailable),
            Arc::new(AccountFeedbackStats::default()),
            crate::support::xai_wire_profile(),
        )
        .expect("official xAI provider configuration"),
    );

    let mut stream = provider
        .execute(
            provider_request_with_operation("xai", operation()),
            diagnostic_context("req_disabled_diagnostic", "disabled-diagnostic"),
        )
        .await
        .expect("disabled diagnostic prepares a fixed-account stream");
    let mut completed = false;
    while let Some(event) = stream.next().await {
        let event = event.expect("disabled diagnostic upstream response");
        completed |= event
            .canonical_facts()
            .iter()
            .any(|event| matches!(event, GatewayEvent::Completed(_)));
    }
    assert!(completed);

    let account = store
        .account(&input.account_id)
        .expect("disabled account after diagnostic");
    assert!(!account.enabled());
    assert_eq!(account.credential_state(), CredentialState::Ready);
}

fn diagnostic_context(request_id: &str, account_suffix: &str) -> AttemptContext {
    AttemptContext::new(
        gateway_core::engine::RequestAttemptContext::new(
            ModelRequestId::new(request_id).expect("request id"),
            ClientApiKeyId::new("key_xai_contract").expect("client key id"),
        ),
        NonZeroU32::new(1).expect("attempt"),
        SystemTime::now() + Duration::from_secs(30),
        selection_policy(),
        AccountAttemptContext::diagnostic(
            BTreeSet::new(),
            AccountProbeTarget::ConnectionTest(account_id(account_suffix)),
            None,
        ),
        None,
        CancellationToken::new(),
    )
}

#[tokio::test]
async fn account_selection_uses_the_exact_routed_model() {
    let selector = StubSelector::success();
    let provider = provider(selector.clone(), StubInferenceTransport::success()).await;
    let mut stream = provider
        .execute(
            provider_request_with_upstream_model("xai", "grok-4.6-latest", operation()),
            context(CancellationToken::new(), None),
        )
        .await
        .expect("provider stream");
    while let Some(event) = stream.next().await {
        event.expect("provider event");
    }

    assert_eq!(
        selector
            .upstream_models
            .lock()
            .expect("upstream models")
            .as_slice(),
        ["grok-4.6-latest"]
    );
}

fn operation_with_state(body: serde_json::Value, state: ProviderSessionState) -> Operation {
    let serde_json::Value::Object(body) = body else {
        panic!("request body must be an object");
    };
    let request = GenerateRequest::from_protocol_payload(
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

    assert_eq!(stream.metadata().provider_account_id(), &required);
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
    let transport = StubInferenceTransport::success_with_metrics(observed_transport_metrics());
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
        &account_id("provider")
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
    assert_eq!(observation.timings().headers_ms, Some(42));
    let metadata: serde_json::Value = serde_json::from_str(
        observation
            .provider_metadata()
            .expect("xAI transport metadata")
            .as_json(),
    )
    .expect("valid metadata JSON");
    assert_eq!(
        metadata,
        json!({
            "schemaVersion": 2,
            "clientCache": "miss",
            "dnsSource": "system",
            "dnsMs": 7
        })
    );
    assert!(
        events
            .iter()
            .filter_map(|event| event.as_ref().ok())
            .flat_map(|event| event.canonical_facts())
            .any(|event| matches!(event, GatewayEvent::Completed(_)))
    );
}

#[tokio::test]
async fn transport_error_observation_should_retain_available_metrics() {
    let transport = StubInferenceTransport::error(
        GrokInferenceTransportError::new(
            GrokInferenceTransportErrorKind::Unavailable,
            UpstreamSendState::Sent,
        )
        .with_status(503)
        .with_transport_metrics(observed_transport_metrics()),
    );
    let provider = provider(StubSelector::success(), transport).await;
    let mut stream = provider
        .execute(
            provider_request("xai"),
            context(CancellationToken::new(), None),
        )
        .await
        .expect("provider stream");

    let event = stream
        .next()
        .await
        .expect("observation event")
        .expect("observation must succeed");
    let observation = event.response_observation().expect("transport observation");
    assert_eq!(observation.status_code(), Some(503));
    assert_eq!(observation.timings().headers_ms, Some(42));
    let metadata: serde_json::Value = serde_json::from_str(
        observation
            .provider_metadata()
            .expect("xAI transport metadata")
            .as_json(),
    )
    .expect("valid metadata JSON");
    assert_eq!(metadata["clientCache"], "miss");
    assert_eq!(metadata["dnsSource"], "system");
    assert_eq!(metadata["dnsMs"], 7);

    let error = next_provider_error(&mut stream).await;
    assert_eq!(error.kind(), ProviderErrorKind::Unavailable);
}

#[tokio::test]
async fn billing_observation_should_persist_the_final_response_tier_before_completion() {
    for (actual, expected_cost) in [("priority", 400_000_000), ("default", 200_000_000)] {
        let body = format!(
            concat!(
                "event: response.created\ndata: {{\"type\":\"response.created\",\"response\":{{\"id\":\"resp_tier\",\"model\":\"grok-4.6\",\"service_tier\":\"priority\"}}}}\n\n",
                "event: response.completed\ndata: {{\"type\":\"response.completed\",\"response\":{{\"id\":\"resp_tier\",\"model\":\"grok-4.6\",\"service_tier\":\"{}\",\"status\":\"completed\",\"output\":[],\"usage\":{{\"input_tokens\":10000,\"output_tokens\":0}}}}}}\n\n"
            ),
            actual
        );
        let transport =
            StubInferenceTransport::sequence([InferenceMode::SuccessBody(body.into_bytes())]);
        let provider = provider(StubSelector::success(), transport).await;
        let mut stream = provider
            .execute(
                provider_request("xai"),
                context(CancellationToken::new(), None),
            )
            .await
            .expect("provider stream");
        let mut tier = None;
        let mut cost = None;
        let mut completed = false;
        while let Some(event) = stream.next().await {
            let event = event.expect("provider event");
            if let Some(value) = event
                .response_observation()
                .and_then(|value| value.service_tier())
            {
                tier = Some(value.to_owned());
            }
            for fact in event.canonical_facts() {
                match fact {
                    GatewayEvent::CalculatedCost(value) => {
                        cost = Some(value.total().amount().scaled())
                    }
                    GatewayEvent::Completed(_) => completed = true,
                    _ => {}
                }
            }
            if completed {
                break;
            }
        }
        assert!(completed);
        assert_eq!(tier.as_deref(), Some(actual));
        assert_eq!(cost, Some(expected_cost));
    }
}

#[tokio::test]
async fn inference_request_uses_oauth_headers_and_no_api_key() {
    let transport = StubInferenceTransport::success();
    let provider = provider(StubSelector::success(), transport.clone()).await;
    let mut stream = Arc::clone(&provider)
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
async fn transport_diagnostic_survives_provider_mapping_without_enabling_replay() {
    let transport = StubInferenceTransport::stream_error(
        GrokInferenceTransportError::new(
            GrokInferenceTransportErrorKind::Transport,
            UpstreamSendState::Sent,
        )
        .with_diagnostic(
            gateway_core::error::ProviderDiagnostic::new("xAI HTTP body read failed")
                .with_classification("receive", "http_body_failed"),
        ),
    );
    let provider = provider(StubSelector::success(), transport).await;
    let mut stream = provider
        .execute(
            provider_request("xai"),
            context(CancellationToken::new(), None),
        )
        .await
        .expect("stream");
    let error = next_provider_error(&mut stream).await;
    let diagnostic = error.diagnostic().expect("preserved transport diagnosis");
    assert_eq!(diagnostic.code(), Some("http_body_failed"));
    assert_eq!(diagnostic.stage(), Some("receive"));
    assert_eq!(diagnostic.as_str(), "xAI HTTP body read failed");
    assert_eq!(error.send_state(), UpstreamSendState::Sent);
    assert!(!error.replay_is_safe());
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
async fn unavailable_unauthorized_recovery_records_temporary_credential_feedback() {
    let selector = StubSelector::success();
    let transport = StubInferenceTransport::error(
        GrokInferenceTransportError::new(
            GrokInferenceTransportErrorKind::Unauthorized,
            UpstreamSendState::Sent,
        )
        .with_status(401)
        .with_credential_recovery(),
    );
    let recovery = StubRecovery::new(GrokCredentialRecoveryOutcome::Unavailable);
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
    assert!(!error.retries_same_account());
    assert_eq!(recovery.calls.load(Ordering::SeqCst), 1);
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
async fn explicit_quota_http_429_marks_provider_error_replay_safe() {
    let error = mapped_transport_error(
        GrokInferenceTransportError::new(
            GrokInferenceTransportErrorKind::QuotaExhausted,
            UpstreamSendState::Sent,
        )
        .with_status(429),
        false,
    )
    .await;

    assert_eq!(error.kind(), ProviderErrorKind::QuotaExhausted);
    assert_eq!(error.upstream_status(), Some(429));
    assert!(error.replay_is_safe());
}

#[tokio::test]
async fn unknown_http_402_is_retryable_with_payment_quota_feedback() {
    let selector = StubSelector::success();
    let transport = StubInferenceTransport::error(
        GrokInferenceTransportError::new(
            GrokInferenceTransportErrorKind::PaymentRequired,
            UpstreamSendState::Sent,
        )
        .with_status(402),
    );
    let provider = provider(selector.clone(), transport).await;
    let mut stream = provider
        .execute(
            provider_request("xai"),
            context(CancellationToken::new(), None),
        )
        .await
        .expect("stream");

    let error = next_provider_error(&mut stream).await;

    assert_eq!(error.kind(), ProviderErrorKind::PermissionDenied);
    assert!(error.replay_is_safe());
    assert_eq!(
        selector.feedback.lock().expect("feedback").as_slice(),
        &[GrokCredentialFailure::PaymentRequired { retry_after: None }]
    );
}

#[tokio::test]
async fn model_quota_transport_failure_preserves_model_scoped_feedback() {
    let selector = StubSelector::success();
    let transport = StubInferenceTransport::error(
        GrokInferenceTransportError::new(
            GrokInferenceTransportErrorKind::ModelQuotaExhausted,
            UpstreamSendState::Sent,
        )
        .with_status(403)
        .with_raw_upstream_error(gateway_core::error::RawUpstreamError::new(
            "PRIVATE_QUOTA_ERROR",
        )),
    );
    let provider = provider(selector.clone(), transport).await;
    let mut stream = provider
        .execute(
            provider_request("xai"),
            context(CancellationToken::new(), None),
        )
        .await
        .expect("stream");

    let error = next_provider_error(&mut stream).await;

    assert_eq!(error.kind(), ProviderErrorKind::QuotaExhausted);
    assert_eq!(
        error.raw_upstream_error().unwrap().as_str(),
        "PRIVATE_QUOTA_ERROR"
    );
    assert!(!format!("{error:?} {error}").contains("PRIVATE_QUOTA_ERROR"));
    assert!(error.replay_is_safe());
    assert_eq!(
        selector.feedback.lock().expect("feedback").as_slice(),
        &[GrokCredentialFailure::ModelQuotaExhausted {
            upstream_model: UpstreamModelId::new("grok-4.5").expect("model"),
            retry_after: None,
        }]
    );
}

#[tokio::test]
async fn free_quota_sse_failure_marks_the_account_exhausted() {
    let selector = StubSelector::success();
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_free_quota\",\"model\":\"grok-4.5\"}}\n\n",
        "event: response.failed\n",
        "data: {\"type\":\"response.failed\",\"error\":{\"code\":\"subscription:free-usage-exhausted\",\"message\":\"You have used all your free usage\"}}\n\n",
    )
    .as_bytes()
    .to_vec();
    let transport = StubInferenceTransport::sequence([InferenceMode::SuccessBody(body)]);
    let provider = provider(selector.clone(), transport).await;
    let mut stream = provider
        .execute(
            provider_request("xai"),
            context(CancellationToken::new(), None),
        )
        .await
        .expect("stream");

    let error = next_provider_error(&mut stream).await;

    assert_eq!(error.kind(), ProviderErrorKind::QuotaExhausted);
    assert_eq!(
        selector.feedback.lock().expect("feedback").as_slice(),
        &[GrokCredentialFailure::FreeQuotaExhausted]
    );
}

#[tokio::test]
async fn free_model_quota_sse_failure_is_classified_for_resettable_account_cooldown() {
    let selector = StubSelector::success();
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_model_quota\",\"model\":\"grok-4.5\"}}\n\n",
        "event: response.failed\n",
        "data: {\"type\":\"response.failed\",\"error\":{\"status\":null,\"contentType\":null,\"body\":null,\"code\":\"subscription_free_usage_exhausted\",\"type\":null,\"message\":\"You've used all the included free usage for model grok-4.5-0722 for now. Usage resets over a rolling 24-hour window \u{2014} tokens (actual/limit): 500505/500000. Upgrade to a Grok subscription for higher limits: https://grok.com/supergrok\"}}\n\n",
    )
    .as_bytes()
    .to_vec();
    let transport = StubInferenceTransport::sequence([InferenceMode::SuccessBody(body)]);
    let provider = provider(selector.clone(), transport).await;
    let mut stream = provider
        .execute(
            provider_request("xai"),
            context(CancellationToken::new(), None),
        )
        .await
        .expect("stream");

    let error = next_provider_error(&mut stream).await;

    assert_eq!(error.kind(), ProviderErrorKind::QuotaExhausted);
    assert_eq!(
        error.upstream_code().map(|code| code.as_str()),
        Some("subscription_free_usage_exhausted")
    );
    assert_eq!(
        selector.feedback.lock().expect("feedback").as_slice(),
        &[GrokCredentialFailure::ModelQuotaExhausted {
            upstream_model: UpstreamModelId::new("grok-4.5").expect("model"),
            retry_after: None,
        }]
    );
}

#[tokio::test]
async fn model_access_denial_is_retryable_without_credential_recovery() {
    let selector = StubSelector::success();
    let transport = StubInferenceTransport::error(
        GrokInferenceTransportError::new(
            GrokInferenceTransportErrorKind::ModelAccessDenied,
            UpstreamSendState::Sent,
        )
        .with_status(403),
    );
    let provider = provider(selector.clone(), transport).await;
    let mut stream = provider
        .execute(
            provider_request("xai"),
            context(CancellationToken::new(), None),
        )
        .await
        .expect("stream");

    let error = next_provider_error(&mut stream).await;

    assert_eq!(error.kind(), ProviderErrorKind::PermissionDenied);
    assert!(error.replay_is_safe());
    assert_eq!(
        selector.feedback.lock().expect("feedback").as_slice(),
        &[GrokCredentialFailure::ModelAccessDenied {
            upstream_model: UpstreamModelId::new("grok-4.5").expect("model"),
            retry_after: None,
        }]
    );
}

#[tokio::test]
async fn safety_rejection_does_not_rotate_or_mutate_account_state() {
    let selector = StubSelector::success();
    let transport = StubInferenceTransport::error(
        GrokInferenceTransportError::new(
            GrokInferenceTransportErrorKind::SafetyRejected,
            UpstreamSendState::Sent,
        )
        .with_status(403),
    );
    let provider = provider(selector.clone(), transport).await;
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
    assert!(selector.feedback.lock().expect("feedback").is_empty());
}

#[tokio::test]
async fn transport_error_should_preserve_client_visible_upstream_detail() {
    let detail = ClientVisibleUpstreamError::new(
        "You have run out of credits",
        Some("personal_team_blocked_spending_limit".to_owned()),
        Some("insufficient_quota".to_owned()),
    );
    let error = mapped_transport_error(
        GrokInferenceTransportError::new(
            GrokInferenceTransportErrorKind::QuotaExhausted,
            UpstreamSendState::Sent,
        )
        .with_status(402)
        .with_client_visible_upstream_error(detail),
        false,
    )
    .await;
    let detail = error
        .client_visible_upstream_error()
        .expect("mapped client-visible detail");

    assert_eq!(
        (detail.message(), detail.code(), detail.error_type()),
        (
            "You have run out of credits",
            Some("personal_team_blocked_spending_limit"),
            Some("insufficient_quota"),
        )
    );
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
async fn generic_http_403_cools_account_and_allows_next_account_replay() {
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
    assert!(error.replay_is_safe());
    assert_eq!(selector.calls.load(Ordering::SeqCst), 1);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        selector.feedback.lock().expect("feedback").as_slice(),
        &[GrokCredentialFailure::AccessDenied]
    );
}

#[tokio::test]
async fn generic_http_500_allows_next_account_replay() {
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
    assert!(error.replay_is_safe());
}

#[tokio::test]
async fn rate_limit_wording_on_http_400_stays_request_scoped() {
    let selector = StubSelector::success();
    let transport = StubInferenceTransport::error(
        GrokInferenceTransportError::new(
            GrokInferenceTransportErrorKind::InvalidRequest,
            UpstreamSendState::Sent,
        )
        .with_status(400)
        .with_upstream_code(OpaqueUpstreamValue::new("rate_limit")),
    );
    let provider = provider(selector.clone(), transport).await;
    let mut stream = provider
        .execute(
            provider_request("xai"),
            context(CancellationToken::new(), None),
        )
        .await
        .expect("stream");
    let error = next_provider_error(&mut stream).await;

    assert_eq!(error.kind(), ProviderErrorKind::InvalidRequest);
    assert!(!error.replay_is_safe());
    assert!(selector.feedback.lock().expect("feedback").is_empty());
}

#[tokio::test]
async fn model_capacity_on_http_400_cools_scope_and_allows_replay() {
    let selector = StubSelector::success();
    let transport = StubInferenceTransport::error(
        GrokInferenceTransportError::new(
            GrokInferenceTransportErrorKind::RateLimited,
            UpstreamSendState::Sent,
        )
        .with_status(400)
        .with_upstream_code(OpaqueUpstreamValue::new("model_capacity")),
    );
    let provider = provider(selector.clone(), transport).await;
    let mut stream = provider
        .execute(
            provider_request("xai"),
            context(CancellationToken::new(), None),
        )
        .await
        .expect("stream");
    let error = next_provider_error(&mut stream).await;

    assert_eq!(error.kind(), ProviderErrorKind::RateLimited);
    assert!(error.replay_is_safe());
    assert_eq!(
        selector.feedback.lock().expect("feedback").as_slice(),
        &[GrokCredentialFailure::ModelCapacity {
            upstream_model: UpstreamModelId::new("grok-4.5").expect("model"),
        }]
    );
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
    assert_eq!(error.kind(), ProviderErrorKind::AccountCapacityUnavailable);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn selection_failures_carry_a_distinguishable_client_error_code() {
    let cases = [
        (
            GrokSessionSelectorError::AccountCoolingDown {
                retry_after: Some(Duration::from_secs(12)),
            },
            ProviderErrorKind::NoEligibleAccount,
            "account_cooling_down",
        ),
        (
            GrokSessionSelectorError::CapacityUnavailable {
                retry_after: Some(Duration::from_millis(20)),
            },
            ProviderErrorKind::AccountCapacityUnavailable,
            "account_capacity_busy",
        ),
        (
            GrokSessionSelectorError::NoEligibleSession,
            ProviderErrorKind::NoEligibleAccount,
            "no_eligible_account",
        ),
        (
            GrokSessionSelectorError::Unavailable(None),
            ProviderErrorKind::ProviderInfrastructureUnavailable,
            "account_selector_unavailable",
        ),
        (
            GrokSessionSelectorError::ModelCoolingDown {
                retry_after: Some(Duration::from_secs(12)),
            },
            ProviderErrorKind::NoEligibleAccount,
            "model_cooling_down",
        ),
    ];
    for (failure, expected_kind, expected_code) in cases {
        let transport = StubInferenceTransport::success();
        let provider = provider(StubSelector::failing(failure), transport.clone()).await;
        let error = match provider
            .execute(
                provider_request("xai"),
                context(CancellationToken::new(), None),
            )
            .await
        {
            Ok(_) => panic!("selection must fail"),
            Err(error) => error,
        };
        let detail = error
            .client_visible_upstream_error()
            .expect("client visible detail");
        assert_eq!(error.kind(), expected_kind);
        assert_eq!(detail.code(), Some(expected_code));
        assert_eq!(detail.error_type(), Some("account_unavailable_error"));
        assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
        if expected_code == "account_cooling_down" {
            assert!(
                detail.message().contains("Retry in 13 seconds."),
                "{}",
                detail.message()
            );
        }
    }
}

#[tokio::test]
async fn selection_failure_preserves_native_cause_without_exposing_it_to_clients() {
    use gateway_core::error::{ErrorSource, StoreError, StoreErrorKind};
    let source = StoreError::caused_by(
        StoreErrorKind::Unavailable,
        std::io::Error::other("PRIVATE_XAI_DATABASE_CAUSE"),
    );
    let transport = StubInferenceTransport::success();
    let provider = provider(
        StubSelector::failing(GrokSessionSelectorError::Unavailable(Some(
            ErrorSource::new(source),
        ))),
        transport.clone(),
    )
    .await;
    let error = match provider
        .execute(
            provider_request("xai"),
            context(CancellationToken::new(), None),
        )
        .await
    {
        Ok(_) => panic!("selection must fail"),
        Err(error) => error,
    };
    assert_eq!(
        error.kind(),
        ProviderErrorKind::ProviderInfrastructureUnavailable
    );
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    assert!(
        error
            .error_details()
            .expect("protected details")
            .contains("PRIVATE_XAI_DATABASE_CAUSE")
    );
    let mut cause = std::error::Error::source(&error);
    let mut native = false;
    while let Some(error) = cause {
        native |= error.downcast_ref::<std::io::Error>().is_some();
        cause = error.source();
    }
    assert!(native);
    assert!(!format!("{error:?} {error}").contains("PRIVATE_XAI_DATABASE_CAUSE"));
    assert!(
        !error
            .client_visible_upstream_error()
            .expect("safe detail")
            .message()
            .contains("PRIVATE_XAI_DATABASE_CAUSE")
    );
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn native_previous_response_is_rejected_before_selection() {
    let selector = StubSelector::success();
    let provider = provider(selector.clone(), StubInferenceTransport::success()).await;
    let pin = NativeContinuationPin::new(
        PreviousResponseId::new("resp_previous"),
        PreviousResponseId::new("resp_upstream_previous"),
        gateway_core::policy::ClientApiKeyId::new("key_xai_contract").expect("client key id"),
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
        PreviousResponseId::new("resp_previous"),
        PreviousResponseId::new("resp_upstream_previous"),
        gateway_core::policy::ClientApiKeyId::new("key_xai_contract").expect("client key id"),
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
async fn continuation_should_inherit_unchanged_instructions_and_replay_changed_instructions() {
    for (stored, instructions) in [
        (true, Some("original")),
        (true, Some("replacement")),
        (true, None),
        (false, Some("original")),
    ] {
        let transport =
            StubInferenceTransport::sequence([InferenceMode::Success, InferenceMode::Success]);
        let provider = provider(StubSelector::success(), transport.clone()).await;
        let first_body = json!({
            "model": "client-model", "instructions": "original", "store": stored,
            "input": [{"type": "message", "role": "user", "content": "first"}]
        });
        let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
            ProtocolPayload::json_object("openai", first_body.as_object().expect("body").clone())
                .expect("payload"),
        ));
        let mut first = Arc::clone(&provider)
            .execute(
                provider_request_with_operation("xai", operation),
                context(CancellationToken::new(), None),
            )
            .await
            .expect("first stream");
        let state = collect_provider_state(&mut first)
            .await
            .expect("session state");
        let pin = NativeContinuationPin::new(
            PreviousResponseId::new("resp_previous"),
            PreviousResponseId::new("resp_upstream_previous"),
            gateway_core::policy::ClientApiKeyId::new("key_xai_contract").expect("client key"),
            ProviderKind::new("xai").expect("provider"),
            account_id("provider"),
        );
        let mut continued = provider
            .execute(
                provider_request_with_operation(
                    "xai",
                    operation_with_state(
                        json!({
                            "model": "client-model", "instructions": instructions,
                            "previous_response_id": "resp_previous",
                            "input": [{"type": "message", "role": "user", "content": "second"}]
                        }),
                        state,
                    ),
                ),
                context(
                    CancellationToken::new(),
                    Some(ContinuationBinding::Pinned(pin)),
                ),
            )
            .await
            .expect("continued stream");
        let next_state = collect_provider_state(&mut continued)
            .await
            .expect("next state");
        assert_eq!(
            next_state.payload().get("instructions"),
            Some(&json!(instructions))
        );
        let requests = transport.requests.lock().expect("requests");
        let body: Value = serde_json::from_slice(requests[1].body()).expect("upstream body");
        if stored && instructions == Some("original") {
            assert_eq!(
                body.get("previous_response_id"),
                Some(&json!("resp_upstream_previous"))
            );
            assert!(body.get("instructions").is_none());
            assert_eq!(
                body.get("input").and_then(Value::as_array).map(Vec::len),
                Some(1)
            );
        } else {
            assert!(body.get("previous_response_id").is_none());
            assert_eq!(body.get("instructions"), Some(&json!(instructions)));
            assert_eq!(body.pointer("/input/0/content"), Some(&json!("first")));
            assert_eq!(
                body.get("input")
                    .and_then(Value::as_array)
                    .and_then(|items| items.last())
                    .and_then(|item| item.get("content")),
                Some(&json!("second"))
            );
        }
    }
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
        PreviousResponseId::new("resp_previous"),
        PreviousResponseId::new("resp_upstream_previous"),
        gateway_core::policy::ClientApiKeyId::new("key_xai_contract").expect("client key id"),
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
    let binding =
        ContinuationBinding::External(PreviousResponseId::new("external-provider-response"));
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
            .with_upstream_code(OpaqueUpstreamValue::new("reasoning_decode_failed")),
        ),
        InferenceMode::Success,
    ]);
    let selector = StubSelector::success();
    let provider = provider(selector.clone(), transport.clone()).await;
    let first_operation = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            Map::from_iter([
                ("model".to_owned(), json!("client-model")),
                ("store".to_owned(), json!(true)),
                ("prompt_cache_key".to_owned(), json!("conversation-42")),
                (
                    "input".to_owned(),
                    json!([{"type":"message","role":"user","content":"first"}]),
                ),
            ]),
        )
        .expect("OpenAI payload"),
    ));
    let mut first = Arc::clone(&provider)
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
        PreviousResponseId::new("resp_gateway_first"),
        PreviousResponseId::new("resp_state"),
        gateway_core::policy::ClientApiKeyId::new("key_xai_contract").expect("client key id"),
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
    let mut continued = Arc::clone(&provider)
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
    assert_eq!(
        recovery_body.pointer("/input/1/encrypted_content"),
        Some(&json!(encrypted_content))
    );
    assert_eq!(selector.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn missing_native_response_should_be_replay_safe_for_the_same_account() {
    let transport = StubInferenceTransport::error(
        GrokInferenceTransportError::new(
            GrokInferenceTransportErrorKind::InvalidRequest,
            UpstreamSendState::Sent,
        )
        .with_status(404)
        .with_upstream_code(OpaqueUpstreamValue::new("not_found")),
    );
    let provider = provider(StubSelector::success(), transport).await;
    let state = ProviderSessionState::new(
        "xai",
        Map::from_iter([
            ("account_id".to_owned(), json!("acct_provider")),
            ("session_id".to_owned(), json!("cache-session")),
            ("response_stored".to_owned(), json!(true)),
            ("transcript".to_owned(), json!([])),
        ]),
    )
    .expect("session state");
    let pin = NativeContinuationPin::new(
        PreviousResponseId::new("resp_gateway_first"),
        PreviousResponseId::new("resp_upstream_first"),
        gateway_core::policy::ClientApiKeyId::new("key_xai_contract").expect("client key id"),
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
        .execute(
            provider_request("xai"),
            context_with_middleware_and_cancellation(
                Arc::new(PassThroughMiddleware),
                cancellation.clone(),
            ),
        )
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
    assert_eq!(
        capabilities[0].catalog_accounts(),
        Some(&BTreeSet::from([account_id("catalog-provider")]))
    );
    assert!(
        capabilities[0]
            .capabilities()
            .match_requirements(&gateway_core::operation::CapabilityRequirements::new(
                OperationKind::Generate
            ))
            .is_some()
    );
    let presentation = capabilities[0]
        .presentation()
        .expect("Grok model presentation");
    assert_eq!(presentation.display_name(), Some("Grok 4.5"));
    assert!(presentation.reasoning_summary());
    assert_eq!(
        presentation.supported_reasoning_efforts(),
        ["low", "medium", "high", "xhigh"]
    );
    assert_eq!(presentation.context_window_tokens(), Some(1_000_000));
    assert_eq!(presentation.max_context_window_tokens(), Some(1_000_000));
    assert!(presentation.agent_tools());
}

#[tokio::test]
async fn provider_propagates_catalog_failure_without_inventing_model_facts() {
    let provider = provider_with_catalog_transport(
        StubSelector::success(),
        StubInferenceTransport::success(),
        StubRecovery::new(GrokCredentialRecoveryOutcome::Unavailable),
        Arc::new(UnavailableCatalogTransport),
    )
    .await;
    let error = provider
        .query_model_capabilities()
        .await
        .expect_err("catalog is unavailable");
    assert_eq!(error.kind(), ProviderErrorKind::Unavailable);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
}

#[tokio::test]
async fn provider_publishes_reasoning_efforts_from_current_menu() {
    let provider = provider_with_catalog_transport(
        StubSelector::success(),
        StubInferenceTransport::success(),
        StubRecovery::new(GrokCredentialRecoveryOutcome::Unavailable),
        Arc::new(CatalogWithReasoningMenuTransport),
    )
    .await;
    let capabilities = provider
        .query_model_capabilities()
        .await
        .expect("capabilities");
    let presentation = capabilities[0]
        .presentation()
        .expect("Grok model presentation");

    assert_eq!(presentation.default_reasoning_effort(), Some("high"));
    assert_eq!(
        presentation.supported_reasoning_efforts(),
        ["low", "medium", "high", "xhigh"]
    );
}

#[tokio::test]
async fn provider_keeps_server_reasoning_default_when_catalog_does_not_declare_one() {
    let provider = provider_with_catalog_transport(
        StubSelector::success(),
        StubInferenceTransport::success(),
        StubRecovery::new(GrokCredentialRecoveryOutcome::Unavailable),
        Arc::new(CatalogWithServerReasoningDefaultsTransport),
    )
    .await;
    let capabilities = provider
        .query_model_capabilities()
        .await
        .expect("capabilities");
    assert_eq!(capabilities.len(), 2);
    for model in capabilities {
        let presentation = model.presentation().expect("model presentation");
        assert_eq!(
            presentation.supported_reasoning_efforts(),
            ["low", "high"],
            "{}",
            model.upstream_model()
        );
        assert_eq!(
            presentation.default_reasoning_effort(),
            None,
            "{}",
            model.upstream_model()
        );
        if model.upstream_model().as_str() == "grok-4.7" {
            assert_eq!(presentation.context_window_tokens(), Some(256_000));
            assert_eq!(presentation.max_context_window_tokens(), Some(500_000));
        }
    }
}

#[tokio::test]
async fn missing_catalog_tool_metadata_keeps_build_tools_routable() {
    let provider = provider_with_catalog_transport(
        StubSelector::success(),
        StubInferenceTransport::success(),
        StubRecovery::new(GrokCredentialRecoveryOutcome::Unavailable),
        Arc::new(CatalogWithoutFeatureMetadataTransport),
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

#[tokio::test]
async fn missing_catalog_feature_metadata_keeps_build_responses_routable() {
    let provider = provider_with_catalog_transport(
        StubSelector::success(),
        StubInferenceTransport::success(),
        StubRecovery::new(GrokCredentialRecoveryOutcome::Unavailable),
        Arc::new(CatalogWithoutFeatureMetadataTransport),
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
                &gateway_core::operation::CapabilityRequirements::new(OperationKind::Generate)
                    .require(Feature::Vision)
                    .require(Feature::JsonSchema)
            )
            .is_some()
    );
}

#[tokio::test]
async fn configured_request_profile_reaches_inference_headers() {
    use provider_xai::profile::selection::{GrokClientProfileSelection, VersionMode};
    let transport = StubInferenceTransport::success();
    let provider = provider(StubSelector::success(), transport.clone()).await;
    let selection = GrokClientProfileSelection {
        version_mode: VersionMode::Fixed,
        client_version: Some("9.8.7".to_owned()),
        client_identifier: "profile-contract".to_owned(),
        target_os: "windows".to_owned(),
        target_arch: "arm64".to_owned(),
        ..Default::default()
    };
    let resolved = provider
        .resolve_request_profile(&selection.document().unwrap())
        .unwrap();
    let context = AttemptContext::new(
        gateway_core::engine::RequestAttemptContext::new(
            ModelRequestId::new("req_profile").unwrap(),
            ClientApiKeyId::new("key_profile").unwrap(),
        )
        .with_request_profile(Some(resolved))
        .with_prompt_rewrite_rules(
            gateway_core::settings::prompt_rewrite::CompiledPromptRewrite::new(
                serde_json::from_value(json!([{"pattern":"Old", "replacement":"Updated"}]))
                    .unwrap(),
            )
            .for_provider(&ProviderKind::new("xai").unwrap()),
        ),
        NonZeroU32::new(1).unwrap(),
        SystemTime::now() + Duration::from_secs(30),
        selection_policy(),
        AccountAttemptContext::new(BTreeSet::new(), None, None),
        None,
        CancellationToken::new(),
    );
    let payload = ProtocolPayload::from_json_bytes(
        "openai",
        bytes::Bytes::from_static(
            br#"{"model":"client-model","instructions":"Old rule","input":"hello"}"#,
        ),
    )
    .unwrap();
    let operation = Operation::Generate(GenerateRequest::from_protocol_payload(payload));
    let events = provider
        .execute(provider_request_with_operation("xai", operation), context)
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    assert!(events.iter().all(Result::is_ok));
    let requests = transport.requests.lock().unwrap();
    let sent: Value = serde_json::from_slice(requests[0].body()).unwrap();
    assert_eq!(sent["instructions"], "Updated rule");
    let headers = requests[0].headers();
    let header = |name| {
        std::str::from_utf8(
            headers
                .iter()
                .find(|header| header.name() == name)
                .unwrap()
                .value(),
        )
        .unwrap()
    };
    assert_eq!(header("x-grok-client-version"), "9.8.7");
    assert_eq!(header("x-grok-client-identifier"), "profile-contract");
    assert_eq!(header("x-grok-client-mode"), "headless");
    assert_eq!(header("user-agent"), "grok-shell/9.8.7 (windows; aarch64)");
}

#[derive(Debug)]
struct AdapterProbe {
    polls: Arc<std::sync::atomic::AtomicUsize>,
    account_scoped: bool,
}

impl gateway_core::engine::upstream_adapter::UpstreamAdapterPlan for AdapterProbe {
    fn select(
        &self,
        _: gateway_core::engine::ModelRouteScope<'_>,
        provider: &ProviderKind,
        account: Option<&ProviderAccountId>,
    ) -> Result<UpstreamAdapterSelection, gateway_core::error::ProviderError> {
        assert_eq!(provider.as_str(), "xai");
        if self.account_scoped && account.is_none() {
            return Ok(UpstreamAdapterSelection::PendingAccount);
        }
        if let Some(account) = account {
            assert_eq!(account.as_str(), "acct_provider");
        }
        Ok(UpstreamAdapterSelection::Adapter(Arc::new(Self {
            polls: self.polls.clone(),
            account_scoped: self.account_scoped,
        })))
    }
}

impl gateway_core::engine::upstream_adapter::UpstreamAdapter for AdapterProbe {
    fn transport(&self) -> &str {
        "http_sse"
    }

    fn supports_authentication(&self, kind: &str) -> bool {
        kind == "oauth"
    }

    fn execute(
        self: Arc<Self>,
        invocation: gateway_core::engine::upstream_adapter::UpstreamAdapterInvocation,
    ) -> gateway_core::engine::provider::EventStream {
        Box::pin(futures::stream::once(async move {
            self.polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            assert_eq!(invocation.account.account_id().as_str(), "acct_provider");
            assert_eq!(
                invocation.metadata.provider_account_id(),
                invocation.account.account_id()
            );
            assert_eq!(invocation.account.authentication_kind(), "oauth");
            let headers = invocation.account.authorization().unwrap();
            let header = |name: &str| {
                headers
                    .iter()
                    .find(|h| h.name() == name)
                    .map(|h| h.value().to_vec())
            };
            assert_eq!(
                header("authorization"),
                Some("Bearer oauth-access".as_bytes().to_vec())
            );
            assert_eq!(
                header("x-grok-user-id"),
                Some("verified-user".as_bytes().to_vec())
            );
            assert!(header("cookie").is_none());
            assert!(
                invocation
                    .headers
                    .iter()
                    .any(|h| h.name() == "x-adapter-onion")
            );
            assert!(
                invocation
                    .account
                    .calculate_cost(
                        None,
                        &gateway_core::metering::Usage {
                            input_tokens: Some(7),
                            output_tokens: Some(2),
                            ..Default::default()
                        }
                    )
                    .is_some()
            );
            Err(gateway_core::error::ProviderError::new(
                ProviderErrorKind::Cancelled,
                UpstreamSendState::NotSent,
            ))
        }))
    }
}

#[tokio::test]
async fn upstream_adapter_reuses_selected_native_account_inside_attempt_onion_and_stays_cold() {
    for account_scoped in [false, true] {
        assert_upstream_adapter_stays_cold(operation(), account_scoped).await;
        for protocol in ["openai", "example-source"] {
            let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
                ProtocolPayload::json_object(
                    protocol,
                    object(json!({"model": MODEL, "input": "hello", "generate": false})),
                )
                .unwrap(),
            ));
            assert_upstream_adapter_stays_cold(operation, account_scoped).await;
        }
    }
}

async fn assert_upstream_adapter_stays_cold(operation: Operation, account_scoped: bool) {
    let selector = StubSelector::success();
    let transport = StubInferenceTransport::success();
    let provider = provider(selector.clone(), transport.clone()).await;
    let polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let lease = ExtensionSetReference::new(
        ExtensionSetId::new("adapter-native-test".to_owned()).unwrap(),
        Arc::new(TestExtensionLease),
    );
    let middleware = Arc::new(RecordingMiddleware {
        observed: Default::default(),
        replacement: Some(("input".into(), json!("rewritten"))),
        request_headers: vec![MiddlewareHeader::new(
            "x-adapter-onion",
            Bytes::from_static(b"present"),
        )],
    });
    let context = AttemptContext::new(
        gateway_core::engine::RequestAttemptContext::new(
            ModelRequestId::new("req_adapter_native").unwrap(),
            ClientApiKeyId::new("key_xai_contract").unwrap(),
        )
        .with_upstream_adapters(Some(
            gateway_core::engine::upstream_adapter::FrozenUpstreamAdapterPlan::new(
                Arc::new(AdapterProbe {
                    polls: polls.clone(),
                    account_scoped,
                }),
                lease.clone(),
            ),
        ))
        .with_middleware(
            Some(FrozenMiddlewarePlan::new(middleware, lease)),
            Arc::from([]),
            "/v1/responses".to_owned(),
            ClientTransport::HttpSse,
        ),
        NonZeroU32::MIN,
        SystemTime::now() + Duration::from_secs(5),
        selection_policy(),
        AccountAttemptContext::new(BTreeSet::new(), None, None),
        None,
        CancellationToken::new(),
    );
    let mut stream = provider
        .execute(provider_request_with_operation("xai", operation), context)
        .await
        .unwrap();
    assert_eq!(polls.load(std::sync::atomic::Ordering::SeqCst), 0);
    let error = stream.next().await.unwrap().unwrap_err();
    assert_eq!(error.kind(), ProviderErrorKind::Cancelled);
    assert_eq!(polls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(selector.calls.load(Ordering::SeqCst), 1);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn native_same_account_replay_preserves_complete_output_items() {
    let output = json!([
        {"id":"native_reasoning","type":"reasoning","status":"completed","encrypted_content":"opaque-not-validated","summary":[],"future_flag":true},
        {"id":"native_function","type":"function_call","status":"completed","call_id":"call_1","name":"functions.native-tool","arguments":"{ \"value\": 1.0 }","future_flag":true},
        {"id":"native_extension","type":"future_native_item","native_payload":{"n":42}}
    ]);
    let wire = [
        json!({"type":"response.created","response":{"id":"resp_full","model":MODEL}}),
        json!({"type":"response.completed","response":{"id":"resp_full","model":MODEL,"status":"completed","output":output}})
    ].into_iter().map(|event| format!("event: {}\ndata: {}\n\n",event["type"].as_str().unwrap(),event)).collect::<String>();
    let transport = StubInferenceTransport::sequence([
        InferenceMode::SuccessBody(wire.into_bytes()),
        InferenceMode::Success,
    ]);
    let provider = provider(StubSelector::success(), transport.clone()).await;
    let first = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", object(json!({"input":"first","store":false})))
            .unwrap(),
    ));
    let mut stream = Arc::clone(&provider)
        .execute(
            provider_request_with_operation("xai", first),
            context(CancellationToken::new(), None),
        )
        .await
        .unwrap();
    let state = collect_provider_state(&mut stream).await.unwrap();
    let captured: Vec<_> = state.payload()["transcript"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item.pointer("/account_output/item"))
        .cloned()
        .collect();
    assert_eq!(captured, output.as_array().unwrap().clone());
    let pin = NativeContinuationPin::new(
        PreviousResponseId::new("gateway_previous"),
        PreviousResponseId::new("resp_full"),
        ClientApiKeyId::new("key_xai_contract").unwrap(),
        ProviderKind::new("xai").unwrap(),
        account_id("provider"),
    );
    let second = operation_with_state(
        json!({"previous_response_id":"gateway_previous","input":"second"}),
        state,
    );
    let mut stream = provider
        .execute(
            provider_request_with_operation("xai", second),
            context(
                CancellationToken::new(),
                Some(ContinuationBinding::Pinned(pin)),
            ),
        )
        .await
        .unwrap();
    while let Some(event) = stream.next().await {
        event.unwrap();
    }
    let requests = transport.requests.lock().unwrap();
    let sent: Value = serde_json::from_slice(requests[1].body()).unwrap();
    assert!(sent.get("previous_response_id").is_none());
    assert_eq!(
        &sent["input"].as_array().unwrap()[1..4],
        output.as_array().unwrap()
    );
    assert_eq!(sent["input"][4]["content"], "second");
}
