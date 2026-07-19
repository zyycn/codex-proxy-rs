use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::Utc;
use futures::{SinkExt, StreamExt};
use gateway_core::engine::continuation::{
    NativeContinuationPin, NativeContinuationReuse, PreviousResponseId,
};
use gateway_core::engine::credential::{AccountAvailability, ProviderAccountId};
use gateway_core::engine::provider::{Provider, ProviderRequest};
use gateway_core::engine::{AttemptContext, CancellationToken, ModelRequestId};
use gateway_core::error::{ProviderError, ProviderErrorKind, SafeUpstreamValue};
use gateway_core::operation::{
    ContentPart, ContinuationMode, GenerateRequest, Message, MessageRole, Operation, OperationKind,
    ProviderOptions,
};
use gateway_core::routing::{
    ConfigRevision, InstanceHealth, ModelCapabilities, ProviderInstance, ProviderKind,
    ProviderModel, PublicModelId, RoutingContext, RuntimeSnapshot, UpstreamModelId,
};
use provider_openai::credential::{
    CodexCookiePolicy, CodexCredentialCatalogService, CodexCredentialSelector,
    CreateCodexCredential,
};
use provider_openai::transport::profile::{CodexWireProfile, CodexWireProfileState};
use provider_openai::{CodexEndpointPolicy, CodexProvider};
use serde_json::{Map, Value, json};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message as WebSocketMessage;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::support::{
    MemoryAccountStore, TestLeaseCoordinator, account_policy, instance_id, profile, secret,
};

fn wire_profile() -> CodexWireProfileState {
    CodexWireProfileState::new(CodexWireProfile {
        originator: "codex_cli_rs".to_owned(),
        codex_version: "0.144.0".to_owned(),
        desktop_version: "1.0.0".to_owned(),
        desktop_build: "1".to_owned(),
        os_type: "linux".to_owned(),
        os_version: "6.8".to_owned(),
        arch: "x86_64".to_owned(),
        terminal: "provider-contract".to_owned(),
        verified_at: Utc::now(),
    })
}

async fn create_account(store: &Arc<MemoryAccountStore>) {
    store
        .repository()
        .create_oauth_credential(CreateCodexCredential {
            account_id: "acct_provider".to_owned(),
            provider_instance_id: instance_id().to_string(),
            name: "provider".to_owned(),
            secret: secret("provider-access"),
            account: profile("chatgpt-acct_provider"),
            enabled: true,
        })
        .await
        .expect("create account");
}

fn instance(base_url: &str) -> ProviderInstance {
    ProviderInstance::new(
        instance_id(),
        ProviderKind::new("openai").expect("provider"),
        base_url.to_owned(),
        true,
        InstanceHealth::Healthy,
    )
}

fn provider(store: &Arc<MemoryAccountStore>) -> CodexProvider {
    let profile = wire_profile();
    let selector = Arc::new(CodexCredentialSelector::new(
        store.repository(),
        Arc::new(TestLeaseCoordinator::default()),
        CodexCookiePolicy::official().expect("cookie policy"),
    ));
    let catalog = Arc::new(CodexCredentialCatalogService::new_with_endpoint_policy(
        store.repository(),
        profile.clone(),
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client"),
        CodexEndpointPolicy::Loopback,
        Duration::from_secs(60),
    ));
    CodexProvider::new_with_policy(selector, catalog, profile, CodexEndpointPolicy::Loopback)
        .expect("Codex provider")
}

fn operation(conversation_id: &str) -> Operation {
    let message = Message::new(
        MessageRole::User,
        vec![ContentPart::Text("hello".to_owned())],
    )
    .expect("message");
    let mut options = ProviderOptions::new();
    options
        .insert(
            "openai",
            Map::from_iter([
                ("schema_version".to_owned(), Value::from(1)),
                (
                    "conversation_id".to_owned(),
                    Value::String(conversation_id.to_owned()),
                ),
                (
                    "transport".to_owned(),
                    Value::String("websocket".to_owned()),
                ),
            ]),
        )
        .expect("provider options");
    Operation::Generate(
        GenerateRequest::new(vec![message])
            .expect("generate request")
            .with_provider_options(options),
    )
}

fn http_operation() -> Operation {
    let message = Message::new(
        MessageRole::User,
        vec![ContentPart::Text("hello".to_owned())],
    )
    .expect("message");
    Operation::Generate(GenerateRequest::new(vec![message]).expect("generate request"))
}

async fn rejected_http_operation(status: u16) -> (ProviderError, [AccountAvailability; 2]) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/backend-api/codex/responses"))
        .respond_with(ResponseTemplate::new(status))
        .expect(1)
        .mount(&server)
        .await;
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store).await;
    store
        .repository()
        .create_oauth_credential(CreateCodexCredential {
            account_id: "acct_provider_other".to_owned(),
            provider_instance_id: instance_id().to_string(),
            name: "provider-other".to_owned(),
            secret: secret("provider-other-access"),
            account: profile("chatgpt-acct_provider_other"),
            enabled: true,
        })
        .await
        .expect("create other account");
    let provider = provider(&store);
    let operation = http_operation();
    let mut stream = provider
        .execute(
            planned_request(&format!("{}/backend-api", server.uri()), &operation),
            context("req_http_rejection", None, CancellationToken::new()),
        )
        .await
        .expect("provider stream");
    let error = stream
        .next()
        .await
        .expect("error event")
        .expect_err("HTTP rejection");
    let availability = [
        store
            .account("acct_provider")
            .expect("provider account")
            .availability(),
        store
            .account("acct_provider_other")
            .expect("other provider account")
            .availability(),
    ];
    (error, availability)
}

fn planned_request(base_url: &str, operation: &Operation) -> ProviderRequest {
    let public_model = PublicModelId::new("gpt-5.4").expect("public model");
    let capabilities = ModelCapabilities::new(
        BTreeSet::from([OperationKind::Generate]),
        128_000,
        Some(32_000),
    );
    let provider_model = ProviderModel::new(
        instance_id(),
        UpstreamModelId::new("gpt-5.4").expect("upstream model"),
        capabilities,
    );
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).expect("revision"),
        account_policy(),
        vec![instance(base_url)],
        vec![provider_model],
        Vec::new(),
    )
    .expect("snapshot");
    let plan = snapshot
        .plan(&public_model, operation, &RoutingContext::default())
        .expect("routing plan");
    ProviderRequest::new(operation.clone(), plan.candidates()[0].clone())
}

fn context(
    request_id: &str,
    continuation: Option<NativeContinuationPin>,
    cancellation: CancellationToken,
) -> AttemptContext {
    AttemptContext::new(
        ModelRequestId::new(request_id).expect("request id"),
        NonZeroU32::new(1).expect("attempt"),
        SystemTime::now() + Duration::from_secs(30),
        account_policy(),
        gateway_core::engine::AccountSelectionConstraints::default(),
        continuation,
        cancellation,
    )
}

fn completed(response_id: &str) -> String {
    json!({
        "type": "response.completed",
        "response": {
            "id": response_id,
            "model": "gpt-5.4",
            "status": "completed",
            "output": [],
            "usage": {"input_tokens": 2, "output_tokens": 1, "total_tokens": 3}
        }
    })
    .to_string()
}

fn created(response_id: &str) -> String {
    json!({
        "type": "response.created",
        "response": {"id": response_id, "model": "gpt-5.4", "status": "in_progress"}
    })
    .to_string()
}

#[tokio::test]
async fn native_continuation_sends_upstream_handle_never_gateway_client_id() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let base_url = format!(
        "http://{}/backend-api",
        listener.local_addr().expect("address")
    );
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let mut websocket = crate::transport::accept_codex_test_websocket(stream).await;
        let first = websocket.next().await.expect("first frame").expect("frame");
        let WebSocketMessage::Text(first) = first else {
            panic!("first request must be text");
        };
        let first: Value = serde_json::from_str(&first).expect("first request JSON");
        assert!(first.get("previous_response_id").is_none());
        websocket
            .send(WebSocketMessage::Text(
                created("native-upstream-handle").into(),
            ))
            .await
            .expect("first response start");
        websocket
            .send(WebSocketMessage::Text(
                completed("native-upstream-handle").into(),
            ))
            .await
            .expect("first response");

        let second = websocket
            .next()
            .await
            .expect("second frame")
            .expect("frame");
        let WebSocketMessage::Text(second) = second else {
            panic!("second request must be text");
        };
        let second: Value = serde_json::from_str(&second).expect("second request JSON");
        assert_eq!(second["previous_response_id"], "native-upstream-handle");
        assert_ne!(second["previous_response_id"], "resp_gateway_client_id");
        websocket
            .send(WebSocketMessage::Text(created("native-second").into()))
            .await
            .expect("second response start");
        websocket
            .send(WebSocketMessage::Text(completed("native-second").into()))
            .await
            .expect("second response");
    });

    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store).await;
    let provider = provider(&store);
    let first_operation = operation("client-conversation-before");
    let mut first = provider
        .execute(
            planned_request(&base_url, &first_operation),
            context("req_provider_first", None, CancellationToken::new()),
        )
        .await
        .expect("first provider stream");
    while let Some(event) = first.next().await {
        event.expect("first canonical event");
    }
    drop(first);

    let pin = NativeContinuationPin::new(
        PreviousResponseId::new("resp_gateway_client_id").expect("gateway response id"),
        SafeUpstreamValue::new("native-upstream-handle").expect("native handle"),
        ProviderKind::new("openai").expect("provider"),
        instance_id(),
        ProviderAccountId::new("acct_provider").expect("account id"),
        NativeContinuationReuse::Reusable,
    );
    let second_operation = operation("different-client-conversation-after");
    let mut second = provider
        .execute(
            planned_request(&base_url, &second_operation),
            context("req_provider_second", Some(pin), CancellationToken::new()),
        )
        .await
        .expect("second provider stream");
    while let Some(event) = second.next().await {
        event.expect("second canonical event");
    }
    server.await.expect("server task");
}

#[tokio::test]
async fn provider_metadata_uses_unified_account_resource_and_real_transport() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let base_url = format!(
        "http://{}/backend-api",
        listener.local_addr().expect("address")
    );
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let mut websocket = crate::transport::accept_codex_test_websocket(stream).await;
        let _ = websocket.next().await.expect("request").expect("frame");
        websocket
            .send(WebSocketMessage::Text(created("native-metadata").into()))
            .await
            .expect("response start");
        websocket
            .send(WebSocketMessage::Text(completed("native-metadata").into()))
            .await
            .expect("response");
    });
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store).await;
    let provider = provider(&store);
    let operation = operation("metadata-conversation");
    let stream = provider
        .execute(
            planned_request(&base_url, &operation),
            context("req_provider_metadata", None, CancellationToken::new()),
        )
        .await
        .expect("provider stream");
    assert_eq!(
        stream
            .metadata()
            .provider_account_id()
            .map(ProviderAccountId::as_str),
        Some("acct_provider")
    );
    assert_eq!(stream.metadata().transport().as_str(), "websocket");
    drop(stream);
    server.abort();
}

#[tokio::test]
async fn cancelled_attempt_fails_before_selecting_or_sending() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store).await;
    let provider = provider(&store);
    let operation = operation("cancelled");
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let result = provider
        .execute(
            planned_request("http://127.0.0.1:9/backend-api", &operation),
            context("req_provider_cancelled", None, cancellation),
        )
        .await;
    let Err(error) = result else {
        panic!("cancelled attempt must fail");
    };
    assert_eq!(error.kind(), ProviderErrorKind::Cancelled);
}

#[tokio::test]
async fn explicit_http_429_marks_provider_error_replay_safe() {
    let (error, _) = rejected_http_operation(429).await;

    assert_eq!(error.kind(), ProviderErrorKind::RateLimited);
    assert_eq!(error.upstream_status(), Some(429));
    assert!(error.replay_is_safe());
}

#[tokio::test]
async fn explicit_http_408_does_not_mark_provider_error_replay_safe() {
    let (error, _) = rejected_http_operation(408).await;

    assert_eq!(error.kind(), ProviderErrorKind::Timeout);
    assert!(!error.replay_is_safe());
}

#[tokio::test]
async fn generic_http_403_and_500_do_not_mark_provider_error_replay_safe() {
    for (status, expected_kind) in [
        (403, ProviderErrorKind::PermissionDenied),
        (500, ProviderErrorKind::Unavailable),
    ] {
        let (error, availability) = rejected_http_operation(status).await;

        assert_eq!(error.kind(), expected_kind);
        assert!(!error.replay_is_safe());
        assert_eq!(
            availability,
            [AccountAvailability::Ready, AccountAvailability::Ready]
        );
    }
}

#[tokio::test]
async fn capability_query_compiles_only_explicit_catalog_evidence() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/codex/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "models": [{
                "slug": "gpt-5.4",
                "display_name": "GPT-5.4",
                "supported_in_api": true,
                "supported_reasoning_levels": [{"effort": "medium"}],
                "supports_parallel_tool_calls": false,
                "input_modalities": ["text"],
                "context_window": 128000
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store).await;
    let provider = provider(&store);
    let capabilities = provider
        .query_model_capabilities(&instance(&format!("{}/backend-api", server.uri())))
        .await
        .expect("capability query");
    assert_eq!(capabilities.len(), 1);
    assert_eq!(capabilities[0].upstream_model().as_str(), "gpt-5.4");
    assert!(
        capabilities[0]
            .capabilities()
            .match_requirements(&operation("capability-query").capability_requirements())
            .is_some()
    );
    let continuation = Operation::Generate(
        GenerateRequest::new(vec![
            Message::new(
                MessageRole::User,
                vec![ContentPart::Text("continue".to_owned())],
            )
            .expect("message"),
        ])
        .expect("generate request")
        .with_continuation(ContinuationMode::Native),
    );
    assert!(
        capabilities[0]
            .capabilities()
            .match_requirements(&continuation.capability_requirements())
            .is_some()
    );
}
