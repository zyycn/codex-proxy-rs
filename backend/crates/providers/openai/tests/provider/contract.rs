use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::Utc;
use futures::{SinkExt, StreamExt};
use gateway_core::engine::continuation::{
    ContinuationBinding, NativeContinuationPin, PreviousResponseId,
};
use gateway_core::engine::credential::{
    AccountAvailability, ProviderAccountId, ProviderAccountStore,
};
use gateway_core::engine::provider::{Provider, ProviderRequest};
use gateway_core::engine::{
    AttemptContext, CancellationToken, ContinuationAttempt, ModelRequestId,
    ProviderAccountStateOwner, UpstreamSendState,
};
use gateway_core::error::{ProviderError, ProviderErrorKind, SafeUpstreamValue};
use gateway_core::event::{GatewayEvent, UpstreamHttpVersion};
use gateway_core::operation::{
    CompactConversationRequest, ContentPart, ContinuationMode, Feature, GenerateRequest, Message,
    MessageRole, Operation, OperationKind, ProtocolPayload, ProviderOptions,
};
use gateway_core::routing::{
    ConfigRevision, InstanceHealth, ModelCapabilities, ProviderInstance, ProviderKind,
    ProviderModel, PublicModelId, RoutingContext, RuntimeSnapshot, UpstreamModelId,
};
use provider_openai::CodexProvider;
use provider_openai::credential::{
    CodexCookie, CodexCookiePolicy, CodexCredentialCatalogService, CodexCredentialQuotaService,
    CodexCredentialSelector, ImportCodexOAuthCredential, RotateCodexCredential,
};
use provider_openai::transport::CodexWebSocketPool;
use provider_openai::transport::profile::{CodexWireProfile, CodexWireProfileState};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message as WebSocketMessage;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::support::{
    MemoryAccountStore, TestLeaseCoordinator, account_policy, instance_id, loopback_origin_policy,
    profile, secret,
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
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: "acct_provider".to_owned(),
            provider_instance_id: instance_id().to_string(),
            name: "provider".to_owned(),
            secret: secret("provider-access"),
            verified_account: profile("chatgpt-acct_provider"),
            next_refresh_at: Some(chrono::Utc::now() + chrono::Duration::minutes(30)),
            enabled: true,
        })
        .await;
}

async fn create_other_account(store: &Arc<MemoryAccountStore>) {
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: "acct_provider_other".to_owned(),
            provider_instance_id: instance_id().to_string(),
            name: "provider-other".to_owned(),
            secret: secret("provider-other-access"),
            verified_account: profile("chatgpt-acct_provider_other"),
            next_refresh_at: Some(chrono::Utc::now() + chrono::Duration::minutes(30)),
            enabled: true,
        })
        .await;
}

async fn attach_cookie(store: &Arc<MemoryAccountStore>, account_id: &str, value: &str) {
    let repository = store.repository();
    let account = store.account(account_id).expect("cookie account");
    let mut data = repository
        .load_complete_data(&account)
        .await
        .expect("load credential data");
    data.cookies.push(CodexCookie {
        name: "cf_clearance".to_owned(),
        value: value.to_owned(),
        domain: "127.0.0.1".to_owned(),
        path: "/backend-api".to_owned(),
        host_only: true,
        secure: false,
        expires_at: None,
    });
    repository
        .compare_and_swap_data(&account, data)
        .await
        .expect("persist test cookie");
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
    provider_with_pool(store, Arc::new(CodexWebSocketPool::default()))
}

fn provider_with_pool(
    store: &Arc<MemoryAccountStore>,
    websocket_pool: Arc<CodexWebSocketPool>,
) -> CodexProvider {
    provider_with_pool_and_cookie_policy(
        store,
        websocket_pool,
        CodexCookiePolicy::official().expect("cookie policy"),
    )
}

fn provider_with_pool_and_cookie_policy(
    store: &Arc<MemoryAccountStore>,
    websocket_pool: Arc<CodexWebSocketPool>,
    cookie_policy: CodexCookiePolicy,
) -> CodexProvider {
    let profile = wire_profile();
    let http = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("client");
    let origin = loopback_origin_policy();
    let catalog = Arc::new(CodexCredentialCatalogService::new(
        store.repository(),
        profile.clone(),
        http.clone(),
        Arc::clone(&origin),
    ));
    let quota = Arc::new(CodexCredentialQuotaService::new(
        store.repository(),
        profile.clone(),
        http.clone(),
        Arc::clone(&origin),
    ));
    let selector = Arc::new(CodexCredentialSelector::new(
        store.repository(),
        Arc::new(TestLeaseCoordinator::default()),
        Arc::clone(&catalog),
        Arc::clone(&quota),
        cookie_policy,
    ));
    CodexProvider::new(
        selector,
        catalog,
        quota,
        http,
        profile,
        websocket_pool,
        origin,
    )
}

#[tokio::test]
async fn provider_uses_the_injected_account_lifecycle_pool() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let base_url = format!(
        "http://{}/backend-api",
        listener.local_addr().expect("address")
    );
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("websocket connection");
        let mut websocket = crate::transport::accept_codex_test_websocket(stream).await;
        let _ = websocket.next().await.expect("request").expect("frame");
        websocket
            .send(WebSocketMessage::Text(created("shared-pool").into()))
            .await
            .expect("created event");
        websocket
            .send(WebSocketMessage::Text(completed("shared-pool").into()))
            .await
            .expect("completed event");
        let close = tokio::time::timeout(Duration::from_secs(2), websocket.next())
            .await
            .expect("shared lifecycle pool should close the idle socket")
            .expect("close frame")
            .expect("valid close frame");
        std::assert_matches!(close, WebSocketMessage::Close(_));
    });
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store).await;
    let pool = Arc::new(CodexWebSocketPool::default());
    let provider = provider_with_pool(&store, Arc::clone(&pool));
    let operation = operation("shared-lifecycle-pool");
    let mut stream = provider
        .execute(
            planned_request(&base_url, &operation),
            context("req_shared_lifecycle_pool", None, CancellationToken::new()),
        )
        .await
        .expect("provider stream");
    let first = stream
        .next()
        .await
        .expect("transport observation")
        .expect("transport observation");
    let observation = first.response_observation().expect("response facts");
    assert_eq!(observation.transport().as_str(), "websocket");
    assert_eq!(
        observation.http_version(),
        Some(UpstreamHttpVersion::Http11)
    );
    assert_eq!(observation.status_code(), Some(101));
    assert!(observation.timings().connect_ms.is_some());
    while let Some(event) = stream.next().await {
        event.expect("websocket event");
    }

    pool.evict_account("acct_provider").await;
    server.await.expect("server task");
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

fn pool_identity_operation(
    prompt_cache_key: &str,
    session_id: &str,
    thread_id: &str,
    conversation_id: &str,
) -> Operation {
    let payload = ProtocolPayload::json_object(
        "openai",
        json!({
            "model": "gpt-5.4",
            "input": [{"role": "user", "content": "pool identity"}],
            "prompt_cache_key": prompt_cache_key,
            "session_id": session_id,
            "thread_id": thread_id,
            "conversation_id": conversation_id
        })
        .as_object()
        .cloned()
        .expect("pool identity request"),
    )
    .expect("OpenAI payload");
    let mut options = ProviderOptions::new();
    options
        .insert(
            "openai",
            Map::from_iter([
                ("schema_version".to_owned(), Value::from(1)),
                (
                    "transport".to_owned(),
                    Value::String("websocket".to_owned()),
                ),
            ]),
        )
        .expect("provider options");
    Operation::Generate(
        GenerateRequest::from_protocol_payload(Vec::new(), payload).with_provider_options(options),
    )
}

#[tokio::test]
async fn account_scoped_pool_identity_is_stable_isolated_and_uses_documented_priority() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let base_url = format!(
        "http://{}/backend-api",
        listener.local_addr().expect("address")
    );
    let (observed_tx, mut observed_rx) = tokio::sync::mpsc::unbounded_channel();
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut handlers = tokio::task::JoinSet::new();
        let mut next_connection_id = 0_u64;
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                accepted = listener.accept() => {
                    let (stream, _) = accepted.expect("websocket connection");
                    next_connection_id += 1;
                    let connection_id = next_connection_id;
                    let observed_tx = observed_tx.clone();
                    handlers.spawn(async move {
                        let mut websocket = crate::transport::accept_codex_test_websocket(stream).await;
                        let mut response_sequence = 0_u64;
                        while let Some(frame) = websocket.next().await {
                            let frame = frame.expect("valid websocket frame");
                            let WebSocketMessage::Text(payload) = frame else {
                                continue;
                            };
                            response_sequence += 1;
                            let payload: Value = serde_json::from_str(&payload)
                                .expect("response.create payload");
                            observed_tx
                                .send((connection_id, payload))
                                .expect("pool observation receiver");
                            let response_id =
                                format!("resp_pool_{connection_id}_{response_sequence}");
                            websocket
                                .send(WebSocketMessage::Text(created(&response_id).into()))
                                .await
                                .expect("created event");
                            websocket
                                .send(WebSocketMessage::Text(completed(&response_id).into()))
                                .await
                                .expect("completed event");
                        }
                    });
                }
                joined = handlers.join_next(), if !handlers.is_empty() => {
                    joined.expect("pool connection task").expect("pool connection handler");
                }
            }
        }
        handlers.abort_all();
        while handlers.join_next().await.is_some() {}
    });

    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store).await;
    create_other_account(&store).await;
    let pool = Arc::new(CodexWebSocketPool::default());
    let provider = provider_with_pool(&store, Arc::clone(&pool));
    let primary = ProviderAccountId::new("acct_provider").expect("primary account");
    let secondary = ProviderAccountId::new("acct_provider_other").expect("secondary account");
    let requests = [
        (
            pool_identity_operation("cache-a", "session-a", "thread-a", "conversation-a"),
            BTreeSet::from([secondary.clone()]),
        ),
        (
            pool_identity_operation("cache-a", "session-a", "thread-a", "conversation-a"),
            BTreeSet::from([secondary.clone()]),
        ),
        (
            pool_identity_operation("cache-a", "session-b", "thread-b", "conversation-b"),
            BTreeSet::from([secondary.clone()]),
        ),
        (
            pool_identity_operation("cache-b", "session-a", "thread-a", "conversation-a"),
            BTreeSet::from([secondary.clone()]),
        ),
        (
            pool_identity_operation("cache-a", "session-a", "thread-a", "conversation-a"),
            BTreeSet::from([primary]),
        ),
    ];
    let mut observations = Vec::new();
    for (index, (operation, excluded)) in requests.into_iter().enumerate() {
        let mut stream = provider
            .execute(
                planned_request(&base_url, &operation),
                context_with_exclusions(
                    &format!("req_pool_identity_{index}"),
                    NonZeroU32::new(1).expect("first attempt"),
                    excluded,
                    None,
                    None,
                    CancellationToken::new(),
                ),
            )
            .await
            .expect("provider stream");
        while let Some(event) = stream.next().await {
            event.expect("provider event");
        }
        observations.push(
            tokio::time::timeout(Duration::from_secs(2), observed_rx.recv())
                .await
                .expect("pool observation timeout")
                .expect("pool observation"),
        );
    }

    assert_eq!(observations[0].0, observations[1].0);
    assert_eq!(observations[0].0, observations[2].0);
    assert_ne!(observations[0].0, observations[3].0);
    assert_ne!(observations[0].0, observations[4].0);
    assert_eq!(observations[0].1["prompt_cache_key"], "cache-a");
    assert_eq!(observations[2].1["session_id"], "session-b");

    pool.shutdown().await;
    let _ = shutdown_tx.send(());
    server.await.expect("pool identity server");
}

fn http_operation() -> Operation {
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
                ("transport".to_owned(), Value::String("http_sse".to_owned())),
            ]),
        )
        .expect("provider options");
    Operation::Generate(
        GenerateRequest::new(vec![message])
            .expect("generate request")
            .with_provider_options(options),
    )
}

fn preferred_operation() -> Operation {
    let message = Message::new(
        MessageRole::User,
        vec![ContentPart::Text("hello".to_owned())],
    )
    .expect("message");
    Operation::Generate(GenerateRequest::new(vec![message]).expect("generate request"))
}

fn identity_tainted_operation() -> Operation {
    let body = json!({
        "model": "gpt-5.4",
        "input": [{"role": "user", "content": "continue"}],
        "accountId": "attacker-account",
        "access_token": "attacker-token",
        "installationId": "attacker-installation",
        "conversation_id": "different-client-conversation-after",
        "previous_response_id": "attacker-response",
        "turnState": "attacker-turn-state",
        "turnMetadata": "{\"accountId\":\"attacker-account\",\"installationId\":\"attacker-installation\",\"request_kind\":\"review\"}",
        "client_metadata": {
            "accountId": "attacker-account",
            "refreshToken": "attacker-refresh",
            "installationId": "attacker-installation",
            "turnMetadata": "{\"accountId\":\"attacker-account\",\"installationId\":\"attacker-installation\",\"subagent_kind\":\"review\"}"
        },
        "future_official_field": {"preserved": true}
    });
    let payload =
        ProtocolPayload::json_object("openai", body.as_object().cloned().expect("request body"))
            .expect("OpenAI payload");
    let mut options = ProviderOptions::new();
    options
        .insert(
            "openai",
            Map::from_iter([
                ("schema_version".to_owned(), Value::from(1)),
                (
                    "transport".to_owned(),
                    Value::String("websocket".to_owned()),
                ),
            ]),
        )
        .expect("provider options");
    Operation::Generate(
        GenerateRequest::from_protocol_payload(Vec::new(), payload).with_provider_options(options),
    )
}

fn failover_identity_operation() -> Operation {
    let body = json!({
        "model": "gpt-5.4",
        "input": [
            {
                "type": "message",
                "role": "user",
                "id": "user_input_id",
                "encrypted_content": "user_account_state",
                "content": [{
                    "type": "input_text",
                    "id": "nested_user_content_id",
                    "encrypted_content": "nested_user_semantics",
                    "text": "same client turn"
                }]
            },
            {
                "type": "message",
                "role": "assistant",
                "id": "source_message_id",
                "encrypted_content": "source_message_encrypted",
                "content": [{
                    "type": "output_text",
                    "id": "nested_output_id",
                    "encrypted_content": "nested_output_semantics",
                    "text": "source answer"
                }]
            },
            {
                "type": "reasoning",
                "id": "source_reasoning_id",
                "encrypted_content": "source_reasoning_encrypted",
                "summary": [{"type": "summary_text", "id": "nested_summary_id", "text": "summary"}]
            },
            {
                "type": "function_call",
                "id": "source_function_id",
                "call_id": "call_source",
                "name": "lookup",
                "arguments": "{\"query\":\"preserved\"}"
            },
            {"type": "compaction", "id": "source_compaction_id", "encrypted_content": "source_compaction"},
            {"type": "compaction_summary", "id": "source_compaction_summary_id"},
            {"type": "context_compaction", "id": "source_context_compaction_id"}
        ],
        "accountId": "attacker-account",
        "access_token": "attacker-token",
        "cookie": "attacker-cookie",
        "installationId": "attacker-installation",
        "conversation_id": "account-bound-client-conversation",
        "session_id": "client-session-stable",
        "turnState": "client-turn-state",
        "turnMetadata": "{\"installationId\":\"attacker-installation\",\"accountId\":\"attacker-account\",\"opaque\":\"preserved\"}",
        "client_metadata": {
            "accountId": "attacker-account",
            "cookie": "attacker-cookie",
            "installationId": "attacker-installation",
            "session_id": "client-session-stable",
            "x-codex-turn-state": "client-metadata-turn-state",
            "x-codex-turn-metadata": "{\"installationId\":\"attacker-installation\",\"accountId\":\"attacker-account\",\"opaque\":\"metadata-preserved\"}"
        }
    });
    let payload = ProtocolPayload::json_object(
        "openai",
        body.as_object().cloned().expect("failover request body"),
    )
    .expect("OpenAI payload");
    let mut options = ProviderOptions::new();
    options
        .insert(
            "openai",
            Map::from_iter([
                ("schema_version".to_owned(), Value::from(1)),
                ("transport".to_owned(), Value::String("http_sse".to_owned())),
            ]),
        )
        .expect("provider options");
    Operation::Generate(
        GenerateRequest::from_protocol_payload(Vec::new(), payload).with_provider_options(options),
    )
}

async fn rejected_http_operation(
    response: ResponseTemplate,
) -> (ProviderError, [AccountAvailability; 2]) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/backend-api/codex/responses"))
        .respond_with(response)
        .expect(1)
        .mount(&server)
        .await;
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store).await;
    create_other_account(&store).await;
    let provider = provider(&store);
    let operation = http_operation();
    let mut stream = provider
        .execute(
            planned_request(&format!("{}/backend-api", server.uri()), &operation),
            context("req_http_rejection", None, CancellationToken::new()),
        )
        .await
        .expect("provider stream");
    let error = next_provider_error(&mut stream).await;
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

async fn in_band_failure(
    code: &str,
    status: Option<u16>,
    message: &str,
) -> (ProviderError, [AccountAvailability; 2]) {
    let server = MockServer::start().await;
    let mut event = json!({
        "type": "response.failed",
        "response": {
            "id": "resp_in_band_failure",
            "status": "failed",
            "error": {"code": code, "message": message}
        }
    });
    if let Some(status) = status {
        event
            .as_object_mut()
            .expect("failure event")
            .insert("status_code".to_owned(), Value::from(status));
    }
    Mock::given(method("POST"))
        .and(path("/backend-api/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(format!("event: response.failed\ndata: {event}\n\n")),
        )
        .expect(1)
        .mount(&server)
        .await;

    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store).await;
    create_other_account(&store).await;
    let provider = provider(&store);
    let secondary = ProviderAccountId::new("acct_provider_other").expect("secondary account");
    let operation = http_operation();
    let mut stream = provider
        .execute(
            planned_request(&format!("{}/backend-api", server.uri()), &operation),
            context_with_exclusions(
                "req_in_band_failure",
                NonZeroU32::new(1).expect("first attempt"),
                BTreeSet::from([secondary]),
                None,
                None,
                CancellationToken::new(),
            ),
        )
        .await
        .expect("provider stream");
    let error = next_provider_error(&mut stream).await;
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

async fn rejected_websocket_opening(
    status: u16,
    body: &str,
) -> (ProviderError, [AccountAvailability; 2]) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let base_url = format!(
        "http://{}/backend-api",
        listener.local_addr().expect("address")
    );
    let body = body.to_owned();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("websocket opening");
        let _ = read_http_headers(&mut stream).await;
        let response = format!(
            "HTTP/1.1 {status} Rejected\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .await
            .expect("opening rejection");
    });
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store).await;
    create_other_account(&store).await;
    let provider = provider(&store);
    let operation = operation("opening-rejection");
    let mut stream = provider
        .execute(
            planned_request(&base_url, &operation),
            context(
                "req_websocket_opening_rejection",
                None,
                CancellationToken::new(),
            ),
        )
        .await
        .expect("provider stream");
    let error = next_provider_error(&mut stream).await;
    server.await.expect("server task");
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

async fn next_provider_error(
    stream: &mut gateway_core::engine::provider::ProviderStream,
) -> ProviderError {
    loop {
        match stream.next().await.expect("provider failure event") {
            Ok(event) => assert!(!event.has_client_event()),
            Err(error) => return error,
        }
    }
}

fn context(
    request_id: &str,
    continuation: Option<ContinuationBinding>,
    cancellation: CancellationToken,
) -> AttemptContext {
    let account_state_owner = continuation
        .as_ref()
        .and_then(ContinuationBinding::pinned)
        .map(ProviderAccountStateOwner::from_continuation);
    context_with_exclusions(
        request_id,
        NonZeroU32::new(1).expect("attempt"),
        BTreeSet::new(),
        account_state_owner,
        continuation,
        cancellation,
    )
}

fn context_with_exclusions(
    request_id: &str,
    attempt_index: NonZeroU32,
    excluded_accounts: BTreeSet<ProviderAccountId>,
    account_state_owner: Option<ProviderAccountStateOwner>,
    continuation: Option<ContinuationBinding>,
    cancellation: CancellationToken,
) -> AttemptContext {
    AttemptContext::new(
        gateway_core::engine::RequestAttemptContext::new(
            ModelRequestId::new(request_id).expect("request id"),
            gateway_core::policy::ClientApiKeyId::new("key_openai_contract")
                .expect("client key id"),
        ),
        attempt_index,
        SystemTime::now() + Duration::from_secs(30),
        account_policy(),
        gateway_core::engine::AccountAttemptContext::new(
            excluded_accounts,
            None,
            account_state_owner,
        ),
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

async fn serve_http_after_dropped_websocket(listener: TcpListener) {
    let (websocket, _) = listener.accept().await.expect("websocket connection");
    drop(websocket);

    let (mut http, _) = listener.accept().await.expect("HTTP connection");
    let request = read_http_headers(&mut http).await;
    assert!(request.starts_with(b"POST /backend-api/codex/responses HTTP/1.1\r\n"));

    let body = format!(
        "data: {}\n\ndata: {}\n\n",
        created("native-http-fallback"),
        completed("native-http-fallback")
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nx-codex-active-limit: codex\r\nx-codex-primary-used-percent: 25\r\nx-codex-primary-window-minutes: 300\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    http.write_all(response.as_bytes())
        .await
        .expect("HTTP response");
}

async fn read_http_headers(stream: &mut TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4_096];
    loop {
        let read = stream.read(&mut buffer).await.expect("HTTP request");
        assert!(read > 0, "HTTP request must contain headers");
        request.extend_from_slice(&buffer[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            return request;
        }
        assert!(
            request.len() <= 32 * 1_024,
            "HTTP headers are unexpectedly large"
        );
    }
}

#[tokio::test]
async fn default_transport_prefers_websocket_and_accepts_same_account_http_fallback() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let base_url = format!(
        "http://{}/backend-api",
        listener.local_addr().expect("address")
    );
    let server = tokio::spawn(serve_http_after_dropped_websocket(listener));
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store).await;
    let provider = provider(&store);
    let operation = preferred_operation();
    let mut stream = provider
        .execute(
            planned_request(&base_url, &operation),
            context(
                "req_provider_preferred_fallback",
                None,
                CancellationToken::new(),
            ),
        )
        .await
        .expect("provider stream");

    let first = stream
        .next()
        .await
        .expect("transport observation")
        .expect("transport observation");
    let observation = first.response_observation().expect("response facts");
    assert_eq!(observation.transport().as_str(), "http_sse");
    assert_eq!(
        observation.http_version(),
        Some(UpstreamHttpVersion::Http11)
    );
    assert_eq!(observation.status_code(), Some(200));
    assert!(observation.timings().transport_decision_wait_ms.is_some());
    while let Some(event) = stream.next().await {
        event.expect("canonical HTTP fallback event");
    }
    let account = store.account("acct_provider").expect("account");
    let quota = store
        .get_quotas(std::slice::from_ref(account.id()))
        .await
        .expect("passive quota")
        .pop()
        .and_then(|observation| observation.quota)
        .expect("quota JSON");
    assert_eq!(
        quota
            .expose_to_provider()
            .get("rate_limit")
            .and_then(Value::as_object)
            .and_then(|limit| limit.get("primary_window"))
            .and_then(Value::as_object)
            .and_then(|window| window.get("used_percent"))
            .and_then(Value::as_f64),
        Some(25.0)
    );
    server.await.expect("server task");
}

#[tokio::test]
async fn external_previous_response_id_is_forwarded_without_a_local_account_pin() {
    let server = MockServer::start().await;
    let body = format!(
        "event: response.created\ndata: {}\n\nevent: response.completed\ndata: {}\n\n",
        created("resp_external_result"),
        completed("resp_external_result")
    );
    Mock::given(method("POST"))
        .and(path("/backend-api/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .expect(1)
        .mount(&server)
        .await;

    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store).await;
    create_other_account(&store).await;
    let provider = provider(&store);
    let operation = http_operation();
    let external = PreviousResponseId::new("external-provider-response").expect("external ID");
    let mut stream = provider
        .execute(
            planned_request(&format!("{}/backend-api", server.uri()), &operation),
            context(
                "req_external_continuation",
                Some(ContinuationBinding::External(external)),
                CancellationToken::new(),
            ),
        )
        .await
        .expect("provider stream");

    while let Some(event) = stream.next().await {
        event.expect("external continuation response");
    }
    let requests = server
        .received_requests()
        .await
        .expect("external continuation request");
    let body: Value = serde_json::from_slice(&requests[0].body).expect("request JSON");
    assert_eq!(body["previous_response_id"], "external-provider-response");
}

#[tokio::test]
async fn account_retry_rebinds_every_provider_identity_and_preserves_client_session_semantics() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/backend-api/codex/responses"))
        .and(header("authorization", "Bearer provider-access"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "error": {"code": "rate_limit_exceeded", "message": "retry another account"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let completed_body = format!(
        "event: response.created\ndata: {}\n\nevent: response.completed\ndata: {}\n\n",
        created("resp_second_account"),
        completed("resp_second_account")
    );
    Mock::given(method("POST"))
        .and(path("/backend-api/codex/responses"))
        .and(header("authorization", "Bearer provider-other-access"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(completed_body),
        )
        .expect(1)
        .mount(&server)
        .await;

    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store).await;
    create_other_account(&store).await;
    attach_cookie(&store, "acct_provider", "cookie-primary").await;
    attach_cookie(&store, "acct_provider_other", "cookie-secondary").await;
    let provider = provider_with_pool_and_cookie_policy(
        &store,
        Arc::new(CodexWebSocketPool::default()),
        CodexCookiePolicy::new(["cf_clearance"], ["127.0.0.1"]).expect("loopback cookie policy"),
    );
    let operation = failover_identity_operation();
    let primary = ProviderAccountId::new("acct_provider").expect("primary account");
    let secondary = ProviderAccountId::new("acct_provider_other").expect("secondary account");

    let mut first = provider
        .execute(
            planned_request(&format!("{}/backend-api", server.uri()), &operation),
            context_with_exclusions(
                "req_identity_retry_first",
                NonZeroU32::new(1).expect("first attempt"),
                BTreeSet::from([secondary.clone()]),
                None,
                None,
                CancellationToken::new(),
            ),
        )
        .await
        .expect("first provider stream");
    assert_eq!(first.metadata().provider_account_id(), Some(&primary));
    let first_error = next_provider_error(&mut first).await;
    assert_eq!(first_error.kind(), ProviderErrorKind::RateLimited);
    assert!(first_error.replay_is_safe());

    let mut second = provider
        .execute(
            planned_request(&format!("{}/backend-api", server.uri()), &operation),
            context_with_exclusions(
                "req_identity_retry_second",
                NonZeroU32::new(2).expect("second attempt"),
                BTreeSet::from([primary.clone()]),
                Some(ProviderAccountStateOwner::new(
                    ProviderKind::new("openai").expect("provider"),
                    instance_id(),
                    primary.clone(),
                )),
                None,
                CancellationToken::new(),
            ),
        )
        .await
        .expect("second provider stream");
    assert_eq!(second.metadata().provider_account_id(), Some(&secondary));
    while let Some(event) = second.next().await {
        event.expect("second account response");
    }

    let requests = server
        .received_requests()
        .await
        .expect("received failover requests");
    assert_eq!(requests.len(), 2);
    for (index, (request, account_id, upstream_account_id, access_token, cookie)) in [
        (
            &requests[0],
            "acct_provider",
            "chatgpt-acct_provider",
            "provider-access",
            "cookie-primary",
        ),
        (
            &requests[1],
            "acct_provider_other",
            "chatgpt-acct_provider_other",
            "provider-other-access",
            "cookie-secondary",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let account = store.account(account_id).expect("selected account");
        let credential = store
            .repository()
            .load_complete_data(&account)
            .await
            .expect("selected credential data");
        let header_value = |name: &str| {
            request
                .headers
                .get(name)
                .and_then(|value| value.to_str().ok())
        };
        assert_eq!(
            header_value("authorization"),
            Some(format!("Bearer {access_token}").as_str())
        );
        assert_eq!(
            header_value("chatgpt-account-id"),
            Some(upstream_account_id)
        );
        assert_eq!(
            header_value("x-codex-installation-id"),
            Some(credential.installation_id.as_str())
        );
        assert_eq!(
            header_value("cookie"),
            Some(format!("cf_clearance={cookie}").as_str())
        );
        assert_eq!(header_value("session-id"), Some("client-session-stable"));

        let body: Value = serde_json::from_slice(&request.body).expect("upstream request JSON");
        let input = body["input"].as_array().expect("input array");
        assert_eq!(
            input[0].pointer("/content/0/text").and_then(Value::as_str),
            Some("same client turn")
        );
        assert_eq!(
            input[0]
                .pointer("/content/0/encrypted_content")
                .and_then(Value::as_str),
            Some("nested_user_semantics")
        );
        if index == 0 {
            assert_eq!(input.len(), 7);
            assert_eq!(input[0].get("id"), Some(&json!("user_input_id")));
            assert_eq!(
                input[1].get("encrypted_content"),
                Some(&json!("source_message_encrypted"))
            );
            assert!(input.iter().any(|item| item["type"] == "compaction"));
        } else {
            assert_eq!(input.len(), 4);
            assert!(input.iter().all(|item| item.get("id").is_none()));
            assert!(
                input
                    .iter()
                    .all(|item| item.get("encrypted_content").is_none())
            );
            assert!(input.iter().all(|item| {
                !matches!(
                    item.get("type").and_then(Value::as_str),
                    Some("compaction" | "compaction_summary" | "context_compaction")
                )
            }));
            assert_eq!(
                input[1].pointer("/content/0/id").and_then(Value::as_str),
                Some("nested_output_id")
            );
            assert_eq!(
                input[1]
                    .pointer("/content/0/encrypted_content")
                    .and_then(Value::as_str),
                Some("nested_output_semantics")
            );
            assert_eq!(
                input[2].pointer("/summary/0/id").and_then(Value::as_str),
                Some("nested_summary_id")
            );
            assert_eq!(
                input[3].get("call_id").and_then(Value::as_str),
                Some("call_source")
            );
            assert_eq!(
                input[3].get("arguments").and_then(Value::as_str),
                Some("{\"query\":\"preserved\"}")
            );
        }
        assert!(body.get("accountId").is_none());
        assert!(body.get("access_token").is_none());
        assert!(body.get("cookie").is_none());
        if index == 0 {
            assert_eq!(
                body.get("conversation_id").and_then(Value::as_str),
                Some("account-bound-client-conversation")
            );
            assert_eq!(
                header_value("x-codex-turn-state"),
                Some("client-turn-state")
            );
            let turn_metadata: Value = serde_json::from_str(
                header_value("x-codex-turn-metadata").expect("first attempt turn metadata"),
            )
            .expect("valid turn metadata");
            assert!(turn_metadata.get("accountId").is_none());
            assert_eq!(
                turn_metadata.get("installationId").and_then(Value::as_str),
                Some(credential.installation_id.as_str())
            );
            assert_eq!(
                turn_metadata.get("opaque").and_then(Value::as_str),
                Some("preserved")
            );
        } else {
            assert!(body.get("conversation_id").is_none());
            assert!(header_value("x-codex-turn-state").is_none());
            let turn_metadata: Value = serde_json::from_str(
                header_value("x-codex-turn-metadata").expect("cross-account metadata"),
            )
            .expect("valid cross-account turn metadata");
            assert!(turn_metadata.get("accountId").is_none());
            assert_eq!(
                turn_metadata.get("installationId").and_then(Value::as_str),
                Some(credential.installation_id.as_str())
            );
            assert_eq!(
                turn_metadata.get("opaque").and_then(Value::as_str),
                Some("preserved")
            );
        }
        assert_eq!(
            body.get("installationId").and_then(Value::as_str),
            Some(credential.installation_id.as_str())
        );
        assert_eq!(
            body.get("session_id").and_then(Value::as_str),
            Some("client-session-stable")
        );
        assert_eq!(
            body.pointer("/client_metadata/installationId")
                .and_then(Value::as_str),
            Some(credential.installation_id.as_str())
        );
        assert_eq!(
            body.pointer("/client_metadata/x-codex-installation-id")
                .and_then(Value::as_str),
            Some(credential.installation_id.as_str())
        );
        assert_eq!(
            body.pointer("/client_metadata/session_id")
                .and_then(Value::as_str),
            Some("client-session-stable")
        );
        assert!(body.pointer("/client_metadata/accountId").is_none());
        assert!(body.pointer("/client_metadata/cookie").is_none());
        if index == 0 {
            assert_eq!(
                body.pointer("/client_metadata/x-codex-turn-state")
                    .and_then(Value::as_str),
                Some("client-metadata-turn-state")
            );
        } else {
            assert!(
                body.pointer("/client_metadata/x-codex-turn-state")
                    .is_none()
            );
            let metadata_turn: Value = serde_json::from_str(
                body.pointer("/client_metadata/x-codex-turn-metadata")
                    .and_then(Value::as_str)
                    .expect("cross-account client metadata turn metadata"),
            )
            .expect("valid cross-account client metadata turn metadata");
            assert!(metadata_turn.get("accountId").is_none());
            assert_eq!(
                metadata_turn.get("installationId").and_then(Value::as_str),
                Some(credential.installation_id.as_str())
            );
            assert_eq!(
                metadata_turn.get("opaque").and_then(Value::as_str),
                Some("metadata-preserved")
            );
        }
    }
    assert_ne!(
        requests[0]
            .headers
            .get("x-codex-installation-id")
            .expect("primary installation"),
        requests[1]
            .headers
            .get("x-codex-installation-id")
            .expect("secondary installation")
    );
}

#[tokio::test]
async fn explicit_websocket_connect_failure_is_not_sent_and_does_not_fallback() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let base_url = format!(
        "http://{}/backend-api",
        listener.local_addr().expect("address")
    );
    let server = tokio::spawn(async move {
        let (websocket, _) = listener.accept().await.expect("websocket connection");
        drop(websocket);
    });
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store).await;
    let provider = provider(&store);
    let operation = operation("required-websocket");
    let mut stream = provider
        .execute(
            planned_request(&base_url, &operation),
            context(
                "req_provider_required_websocket",
                None,
                CancellationToken::new(),
            ),
        )
        .await
        .expect("provider stream");

    let error = next_provider_error(&mut stream).await;
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    server.await.expect("server task");
}

#[tokio::test]
async fn explicit_websocket_post_send_disconnect_remains_ambiguous() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let base_url = format!(
        "http://{}/backend-api",
        listener.local_addr().expect("address")
    );
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("websocket connection");
        let mut websocket = crate::transport::accept_codex_test_websocket(stream).await;
        let _ = websocket.next().await.expect("request").expect("frame");
        drop(websocket);
    });
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store).await;
    let provider = provider(&store);
    let operation = operation("ambiguous-websocket");
    let mut stream = provider
        .execute(
            planned_request(&base_url, &operation),
            context(
                "req_provider_ambiguous_websocket",
                None,
                CancellationToken::new(),
            ),
        )
        .await
        .expect("provider stream");

    let error = next_provider_error(&mut stream).await;
    assert_eq!(error.send_state(), UpstreamSendState::Ambiguous);
    server.await.expect("server task");
}

#[tokio::test]
async fn account_invalidation_closes_its_idle_websocket_connections() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let base_url = format!(
        "http://{}/backend-api",
        listener.local_addr().expect("address")
    );
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("websocket connection");
        let mut websocket = crate::transport::accept_codex_test_websocket(stream).await;
        let _ = websocket.next().await.expect("request").expect("frame");
        websocket
            .send(WebSocketMessage::Text(created("evict-me").into()))
            .await
            .expect("created event");
        websocket
            .send(WebSocketMessage::Text(completed("evict-me").into()))
            .await
            .expect("completed event");

        let (mut http, _) = listener.accept().await.expect("HTTP connection");
        let _ = read_http_headers(&mut http).await;
        let body = r#"{"error":{"code":"token_expired","message":"access token expired"}}"#;
        let response = format!(
            "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        http.write_all(response.as_bytes())
            .await
            .expect("HTTP rejection");

        loop {
            match tokio::time::timeout(Duration::from_secs(2), websocket.next()).await {
                Ok(Some(Ok(WebSocketMessage::Close(_)))) | Ok(None) => break,
                Ok(Some(Ok(_))) => {}
                Ok(Some(Err(error))) => panic!("websocket close failed: {error}"),
                Err(_) => panic!("account invalidation did not close pooled websocket"),
            }
        }
    });
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store).await;
    let provider = provider(&store);

    let websocket_operation = operation("eviction-lifecycle");
    let mut first = provider
        .execute(
            planned_request(&base_url, &websocket_operation),
            context(
                "req_pool_before_invalidation",
                None,
                CancellationToken::new(),
            ),
        )
        .await
        .expect("websocket provider stream");
    while let Some(event) = first.next().await {
        event.expect("websocket event");
    }

    let http_operation = http_operation();
    let mut second = provider
        .execute(
            planned_request(&base_url, &http_operation),
            context("req_pool_invalidation", None, CancellationToken::new()),
        )
        .await
        .expect("HTTP provider stream");
    let error = next_provider_error(&mut second).await;

    assert_eq!(error.kind(), ProviderErrorKind::Unauthorized);
    assert_eq!(
        store
            .account("acct_provider")
            .expect("provider account")
            .availability(),
        AccountAvailability::Expired
    );
    server.await.expect("server task");
}

#[tokio::test]
async fn credential_revision_change_does_not_split_established_logical_websocket() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let base_url = format!(
        "http://{}/backend-api",
        listener.local_addr().expect("address")
    );
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.expect("first websocket");
        let mut first = crate::transport::accept_codex_test_websocket(first_stream).await;
        let _ = first.next().await.expect("first request").expect("frame");
        first
            .send(WebSocketMessage::Text(created("before-rotation").into()))
            .await
            .expect("first created");
        first
            .send(WebSocketMessage::Text(completed("before-rotation").into()))
            .await
            .expect("first completed");

        let _ = first.next().await.expect("second request").expect("frame");
        first
            .send(WebSocketMessage::Text(created("after-rotation").into()))
            .await
            .expect("second created");
        first
            .send(WebSocketMessage::Text(completed("after-rotation").into()))
            .await
            .expect("second completed");
        assert!(
            tokio::time::timeout(Duration::from_millis(300), listener.accept())
                .await
                .is_err(),
            "credential revision must not split an established logical connection"
        );
    });
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store).await;
    let provider = provider(&store);
    let operation = operation("credential-rotation");

    let mut first = provider
        .execute(
            planned_request(&base_url, &operation),
            context("req_before_rotation", None, CancellationToken::new()),
        )
        .await
        .expect("first provider stream");
    while let Some(event) = first.next().await {
        event.expect("first websocket event");
    }

    store
        .repository()
        .rotate_oauth_secret(RotateCodexCredential {
            account_id: "acct_provider".to_owned(),
            expected_credential_revision: 1,
            secret: secret("rotated-provider-access"),
            verified_account: profile("chatgpt-acct_provider"),
            next_refresh_at: Some(chrono::Utc::now() + chrono::Duration::minutes(30)),
        })
        .await
        .expect("rotate credential");

    let mut second = provider
        .execute(
            planned_request(&base_url, &operation),
            context("req_after_rotation", None, CancellationToken::new()),
        )
        .await
        .expect("second provider stream");
    while let Some(event) = second.next().await {
        event.expect("second websocket event");
    }
    server.await.expect("server task");
}

#[tokio::test]
async fn provider_instance_reuses_the_same_websocket_origin_breaker() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let base_url = format!(
        "http://{}/backend-api",
        listener.local_addr().expect("address")
    );
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut accepted = 0;
        loop {
            tokio::select! {
                _ = &mut stop_rx => break,
                accepted_connection = listener.accept() => {
                    let (connection, _) = accepted_connection.expect("websocket connection");
                    accepted += 1;
                    drop(connection);
                }
            }
        }
        accepted
    });
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store).await;
    let provider = provider(&store);

    for attempt in 0..4 {
        let payload = ProtocolPayload::json_object(
            "openai",
            Map::from_iter([
                ("model".to_owned(), json!("gpt-public")),
                (
                    "input".to_owned(),
                    json!([{"role": "user", "content": "warm connection"}]),
                ),
                ("stream".to_owned(), json!(true)),
                ("store".to_owned(), json!(false)),
                ("generate".to_owned(), json!(false)),
                ("conversation_id".to_owned(), json!("breaker-origin")),
            ]),
        )
        .expect("warmup payload");
        let operation =
            Operation::Generate(GenerateRequest::from_protocol_payload(Vec::new(), payload));
        let mut stream = provider
            .execute(
                planned_request(&base_url, &operation),
                context(
                    &format!("req_breaker_{attempt}"),
                    None,
                    CancellationToken::new(),
                ),
            )
            .await
            .expect("provider stream");
        let error = next_provider_error(&mut stream).await;
        assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    }

    stop_tx.send(()).expect("stop server");
    assert_eq!(server.await.expect("server task"), 3);
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
        assert!(first.get("accountId").is_none());
        assert!(first.get("access_token").is_none());
        assert_eq!(
            first.get("conversation_id").and_then(Value::as_str),
            Some("different-client-conversation-after")
        );
        assert_eq!(
            first.get("turnState").and_then(Value::as_str),
            Some("attacker-turn-state")
        );
        assert_ne!(first["installationId"], "attacker-installation");
        let first_turn_metadata: Value =
            serde_json::from_str(first["turnMetadata"].as_str().expect("first turn metadata"))
                .expect("first turn metadata JSON");
        assert!(first_turn_metadata.get("accountId").is_none());
        assert_ne!(
            first_turn_metadata["installationId"],
            "attacker-installation"
        );
        assert_eq!(first_turn_metadata["request_kind"], "review");
        let first_metadata = first["client_metadata"]
            .as_object()
            .expect("first client metadata");
        assert!(first_metadata.get("accountId").is_none());
        assert!(first_metadata.get("refreshToken").is_none());
        let first_client_turn_metadata: Value = serde_json::from_str(
            first_metadata
                .get("turnMetadata")
                .and_then(Value::as_str)
                .expect("first client turn metadata"),
        )
        .expect("first client turn metadata JSON");
        assert!(first_client_turn_metadata.get("accountId").is_none());
        assert_ne!(
            first_client_turn_metadata["installationId"],
            "attacker-installation"
        );
        assert_eq!(first_client_turn_metadata["subagent_kind"], "review");
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
        assert!(second.get("accountId").is_none());
        assert!(second.get("access_token").is_none());
        assert_ne!(second["installationId"], "attacker-installation");
        assert_eq!(
            second.pointer("/future_official_field/preserved"),
            Some(&json!(true))
        );
        let turn_metadata: Value = serde_json::from_str(
            second["turnMetadata"]
                .as_str()
                .expect("sanitized turn metadata"),
        )
        .expect("turn metadata JSON");
        assert!(turn_metadata.get("accountId").is_none());
        assert_ne!(turn_metadata["installationId"], "attacker-installation");
        assert_eq!(turn_metadata["request_kind"], "review");
        let client_metadata = second["client_metadata"]
            .as_object()
            .expect("client metadata");
        assert!(client_metadata.get("accountId").is_none());
        assert!(client_metadata.get("refreshToken").is_none());
        assert_ne!(
            client_metadata.get("installationId"),
            Some(&json!("attacker-installation"))
        );
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
    let first_operation = identity_tainted_operation();
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
    );
    let second_operation = identity_tainted_operation();
    let mut second = provider
        .execute(
            planned_request(&base_url, &second_operation),
            context(
                "req_provider_second",
                Some(ContinuationBinding::Pinned(pin)),
                CancellationToken::new(),
            ),
        )
        .await
        .expect("second provider stream");
    while let Some(event) = second.next().await {
        event.expect("second canonical event");
    }
    server.await.expect("server task");
}

#[tokio::test]
async fn connection_replay_restores_full_input_without_reusing_native_handle() {
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
        let first_input = first["input"].as_array().expect("first input").to_vec();
        websocket
            .send(WebSocketMessage::Text(
                created("native-replay-source").into(),
            ))
            .await
            .expect("first response start");
        websocket
            .send(WebSocketMessage::Text(
                completed("native-replay-source").into(),
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
        assert!(second.get("previous_response_id").is_none());
        let second_input = second["input"].as_array().expect("second input");
        assert!(second_input.starts_with(&first_input));
        assert!(second_input.len() > first_input.len());
        websocket
            .send(WebSocketMessage::Text(created("native-replayed").into()))
            .await
            .expect("second response start");
        websocket
            .send(WebSocketMessage::Text(completed("native-replayed").into()))
            .await
            .expect("second response");
    });

    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store).await;
    let provider = provider(&store);
    let first_operation = identity_tainted_operation();
    let mut first = provider
        .execute(
            planned_request(&base_url, &first_operation),
            context("req_replay_first", None, CancellationToken::new()),
        )
        .await
        .expect("first provider stream");
    let mut provider_state = None;
    while let Some(event) = first.next().await {
        let mut event = event.expect("first canonical event");
        provider_state = event.take_session_update().or(provider_state);
    }
    drop(first);

    let pin = NativeContinuationPin::new(
        PreviousResponseId::new("resp_gateway_replay").expect("gateway response id"),
        SafeUpstreamValue::new("native-replay-source").expect("native handle"),
        ProviderKind::new("openai").expect("provider"),
        instance_id(),
        ProviderAccountId::new("acct_provider").expect("account id"),
    );
    let second_operation = identity_tainted_operation()
        .with_provider_session_state(provider_state.expect("connection replay state"));
    let replay_context = context(
        "req_replay_second",
        Some(ContinuationBinding::Pinned(pin)),
        CancellationToken::new(),
    )
    .with_continuation_attempt(ContinuationAttempt::ReplayOwner);
    let mut second = provider
        .execute(
            planned_request(&base_url, &second_operation),
            replay_context,
        )
        .await
        .expect("second provider stream");
    while let Some(event) = second.next().await {
        event.expect("second canonical event");
    }
    server.await.expect("server task");
}

#[tokio::test]
async fn connection_replay_preserves_openai_observability_semantics() {
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
            .send(WebSocketMessage::Text(created("semantic-source").into()))
            .await
            .expect("response start");
        websocket
            .send(WebSocketMessage::Text(completed("semantic-source").into()))
            .await
            .expect("response");
    });
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store).await;
    let provider = provider(&store);
    let first_payload = ProtocolPayload::json_object(
        "openai",
        json!({
            "model": "gpt-5.4",
            "reasoning": {"effort": "max"},
            "input": [
                {
                    "type": "message",
                    "role": "developer",
                    "content": [{
                        "type": "input_text",
                        "text": "<multi_agent_mode>Proactive multi-agent delegation is active.</multi_agent_mode>"
                    }]
                },
                {"role": "user", "content": "remember this mode"}
            ],
            "conversation_id": "semantic-conversation",
            "stream": true,
            "store": false
        })
        .as_object()
        .expect("first request object")
        .clone(),
    )
    .expect("first payload");
    let first_operation = Operation::Generate(GenerateRequest::from_protocol_payload(
        Vec::new(),
        first_payload,
    ));
    let mut first = provider
        .execute(
            planned_request(&base_url, &first_operation),
            context("req_semantics_first", None, CancellationToken::new()),
        )
        .await
        .expect("first provider stream");
    let mut provider_state = None;
    while let Some(event) = first.next().await {
        let mut event = event.expect("first canonical event");
        provider_state = event.take_session_update().or(provider_state);
    }
    server.await.expect("server task");

    let next_payload = ProtocolPayload::json_object(
        "openai",
        json!({
            "model": "gpt-5.4",
            "reasoning": {"effort": "max"},
            "input": [{"role": "user", "content": "continue"}],
            "stream": true,
            "store": false
        })
        .as_object()
        .expect("next request object")
        .clone(),
    )
    .expect("next payload");
    let next_operation = Operation::Generate(
        GenerateRequest::from_protocol_payload(Vec::new(), next_payload)
            .with_provider_session_state(provider_state.expect("connection state")),
    );

    let observation = provider.request_observation(&next_operation);
    assert_eq!(observation.reasoning_preset.as_deref(), Some("ultra"));
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
    let (error, availability) =
        rejected_http_operation(ResponseTemplate::new(429).set_body_json(json!({
            "error": {
                "code": "rate_limit_exceeded",
                "message": "Rate limit exceeded, retry in 12 seconds"
            }
        })))
        .await;

    assert_eq!(error.kind(), ProviderErrorKind::RateLimited);
    assert_eq!(error.upstream_status(), Some(429));
    assert_eq!(error.send_state(), UpstreamSendState::Sent);
    assert!(error.replay_is_safe());
    assert_eq!(
        availability,
        [AccountAvailability::Cooldown, AccountAvailability::Ready]
    );
}

#[tokio::test]
async fn in_band_rate_limit_failure_updates_account_state_and_remains_replay_safe() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: response.failed\n",
        "data: {\"type\":\"response.failed\",\"status_code\":429,\"response\":{\"id\":\"resp_rate_limited\",\"status\":\"failed\",\"error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"sensitive upstream detail\"}}}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/backend-api/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .expect(1)
        .mount(&server)
        .await;

    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store).await;
    create_other_account(&store).await;
    let provider = provider(&store);
    let secondary = ProviderAccountId::new("acct_provider_other").expect("secondary account");
    let operation = http_operation();
    let mut stream = provider
        .execute(
            planned_request(&format!("{}/backend-api", server.uri()), &operation),
            context_with_exclusions(
                "req_in_band_rate_limit",
                NonZeroU32::new(1).expect("first attempt"),
                BTreeSet::from([secondary]),
                None,
                None,
                CancellationToken::new(),
            ),
        )
        .await
        .expect("provider stream");

    let error = next_provider_error(&mut stream).await;
    assert_eq!(error.kind(), ProviderErrorKind::RateLimited);
    assert_eq!(error.send_state(), UpstreamSendState::Sent);
    assert_eq!(error.upstream_status(), Some(429));
    assert_eq!(
        error.upstream_code().map(SafeUpstreamValue::as_str),
        Some("rate_limit_exceeded")
    );
    assert!(error.replay_is_safe());
    assert!(error.sensitive_context_was_redacted());
    assert_eq!(
        [
            store
                .account("acct_provider")
                .expect("primary account")
                .availability(),
            store
                .account("acct_provider_other")
                .expect("secondary account")
                .availability(),
        ],
        [AccountAvailability::Cooldown, AccountAvailability::Ready]
    );
}

#[tokio::test]
async fn in_band_failure_after_same_chunk_output_preserves_output_and_forbids_replay() {
    let marker = "same-chunk-provider-secret";
    let server = MockServer::start().await;
    let body = format!(
        concat!(
            "event: response.created\n",
            "data: {{\"type\":\"response.created\",\"response\":{{\"id\":\"resp_partial\",\"model\":\"gpt-5.4\"}}}}\n\n",
            "event: response.content_part.added\n",
            "data: {{\"type\":\"response.content_part.added\",\"output_index\":0,\"content_index\":0,\"part\":{{\"type\":\"output_text\"}}}}\n\n",
            "event: response.output_text.delta\n",
            "data: {{\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"hello\"}}\n\n",
            "event: response.failed\n",
            "data: {{\"type\":\"response.failed\",\"status_code\":429,\"response\":{{\"id\":\"resp_partial\",\"status\":\"failed\",\"error\":{{\"code\":\"rate_limit_exceeded\",\"message\":\"{}\"}}}}}}\n\n"
        ),
        marker
    );
    Mock::given(method("POST"))
        .and(path("/backend-api/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .expect(1)
        .mount(&server)
        .await;

    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store).await;
    create_other_account(&store).await;
    let provider = provider(&store);
    let secondary = ProviderAccountId::new("acct_provider_other").expect("secondary account");
    let operation = http_operation();
    let mut stream = provider
        .execute(
            planned_request(&format!("{}/backend-api", server.uri()), &operation),
            context_with_exclusions(
                "req_in_band_output_failure",
                NonZeroU32::new(1).expect("first attempt"),
                BTreeSet::from([secondary]),
                None,
                None,
                CancellationToken::new(),
            ),
        )
        .await
        .expect("provider stream");

    let mut saw_text = false;
    let error = loop {
        match stream.next().await.expect("typed failure terminal") {
            Ok(event) => {
                assert_ne!(
                    event.wire_event().and_then(|wire| wire.event_type()),
                    Some("response.failed"),
                    "raw failed event must not cross the Provider boundary"
                );
                saw_text |= event.canonical_facts().iter().any(
                    |fact| matches!(fact, GatewayEvent::TextDelta(delta) if delta.text == "hello"),
                );
                assert!(!format!("{event:?}").contains(marker));
            }
            Err(error) => break error,
        }
    };

    assert!(
        saw_text,
        "semantic output must precede the terminal failure"
    );
    assert_eq!(error.kind(), ProviderErrorKind::RateLimited);
    assert!(!error.replay_is_safe());
    assert_eq!(
        error.upstream_code().map(SafeUpstreamValue::as_str),
        Some("rate_limit_exceeded")
    );
    assert!(!format!("{error:?} {error}").contains(marker));
    assert_eq!(
        store
            .account("acct_provider")
            .expect("primary account")
            .availability(),
        AccountAvailability::Cooldown
    );
    assert_eq!(
        store
            .account("acct_provider_other")
            .expect("secondary account")
            .availability(),
        AccountAvailability::Ready
    );
}

#[tokio::test]
async fn in_band_token_invalid_expires_only_the_selected_account() {
    let (error, availability) =
        in_band_failure("token_invalid", Some(401), "credential rejected").await;

    assert_eq!(error.kind(), ProviderErrorKind::Unauthorized);
    assert!(error.replay_is_safe());
    assert_eq!(
        error.upstream_code().map(SafeUpstreamValue::as_str),
        Some("token_invalid")
    );
    assert_eq!(
        availability,
        [AccountAvailability::Expired, AccountAvailability::Ready]
    );
}

#[tokio::test]
async fn in_band_model_not_supported_preserves_account_health() {
    let (error, availability) =
        in_band_failure("model_not_supported", Some(404), "model is unavailable").await;

    assert_eq!(error.kind(), ProviderErrorKind::Unsupported);
    assert!(error.replay_is_safe());
    assert_eq!(
        availability,
        [AccountAvailability::Ready, AccountAvailability::Ready]
    );
}

#[tokio::test]
async fn in_band_quota_exhaustion_updates_only_the_selected_account() {
    let (error, availability) =
        in_band_failure("insufficient_quota", Some(402), "quota exhausted").await;

    assert_eq!(error.kind(), ProviderErrorKind::QuotaExhausted);
    assert!(error.replay_is_safe());
    assert_eq!(
        availability,
        [
            AccountAvailability::QuotaExhausted,
            AccountAvailability::Ready
        ]
    );
}

#[tokio::test]
async fn in_band_cyber_policy_is_not_replayed_or_applied_to_account_health() {
    let (error, availability) =
        in_band_failure("cyber_policy", Some(400), "request rejected").await;

    assert_eq!(error.kind(), ProviderErrorKind::InvalidRequest);
    assert!(!error.replay_is_safe());
    assert_eq!(
        availability,
        [AccountAvailability::Ready, AccountAvailability::Ready]
    );
}

#[tokio::test]
async fn in_band_overload_is_not_replayed_or_applied_to_account_health() {
    let (error, availability) =
        in_band_failure("server_is_overloaded", None, "upstream overloaded").await;

    assert_eq!(error.kind(), ProviderErrorKind::Unavailable);
    assert!(!error.replay_is_safe());
    assert_eq!(
        availability,
        [AccountAvailability::Ready, AccountAvailability::Ready]
    );
}

#[tokio::test]
async fn in_band_unknown_code_is_classified_without_persisting_opaque_text() {
    let marker = "unknown-secret-account-marker";
    let (error, availability) = in_band_failure(marker, None, marker).await;

    assert_eq!(error.kind(), ProviderErrorKind::Unavailable);
    assert!(error.upstream_code().is_none());
    assert!(!error.replay_is_safe());
    assert!(error.sensitive_context_was_redacted());
    assert!(!format!("{error:?}").contains(marker));
    assert!(!error.to_string().contains(marker));
    assert_eq!(
        availability,
        [AccountAvailability::Ready, AccountAvailability::Ready]
    );
}

#[tokio::test]
async fn rejected_response_passively_persists_rate_limit_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/backend-api/codex/responses"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("x-codex-active-limit", "codex")
                .insert_header("x-codex-primary-used-percent", "100")
                .insert_header("x-codex-primary-window-minutes", "300")
                .insert_header("x-codex-limit-reached", "true")
                .set_body_json(json!({
                    "error": {"code": "rate_limit_exceeded", "message": "rate limited"}
                })),
        )
        .expect(1)
        .mount(&server)
        .await;
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store).await;
    let provider = provider(&store);
    let operation = http_operation();
    let mut stream = provider
        .execute(
            planned_request(&format!("{}/backend-api", server.uri()), &operation),
            context("req_passive_rejection", None, CancellationToken::new()),
        )
        .await
        .expect("provider stream");

    let error = next_provider_error(&mut stream).await;
    assert_eq!(error.kind(), ProviderErrorKind::RateLimited);
    let account = store.account("acct_provider").expect("account");
    let quota = store
        .get_quotas(std::slice::from_ref(account.id()))
        .await
        .expect("quota")
        .pop()
        .and_then(|observation| observation.quota)
        .expect("quota JSON");
    assert_eq!(
        quota
            .expose_to_provider()
            .get("rate_limit")
            .and_then(Value::as_object)
            .and_then(|limit| limit.get("limit_reached"))
            .and_then(Value::as_bool),
        Some(true)
    );
}

#[tokio::test]
async fn websocket_opening_429_is_proven_not_sent_and_replay_safe() {
    let (error, availability) = rejected_websocket_opening(
        429,
        r#"{"error":{"code":"rate_limit_exceeded","message":"Rate limit exceeded"}}"#,
    )
    .await;

    assert_eq!(error.kind(), ProviderErrorKind::RateLimited);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    assert!(error.replay_is_safe());
    assert_eq!(
        availability,
        [AccountAvailability::Cooldown, AccountAvailability::Ready]
    );
}

#[tokio::test]
async fn websocket_opening_cloudflare_challenge_is_replay_safe_before_payload() {
    let (error, availability) =
        rejected_websocket_opening(403, "<html><title>Just a moment...</title></html>").await;

    assert_eq!(error.kind(), ProviderErrorKind::Unavailable);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    assert!(error.replay_is_safe());
    assert_eq!(
        availability,
        [AccountAvailability::Cooldown, AccountAvailability::Ready]
    );
}

#[tokio::test]
async fn http_cloudflare_challenge_is_replay_safe_after_explicit_rejection() {
    let (error, availability) = rejected_http_operation(
        ResponseTemplate::new(403).set_body_string("<html><title>Just a moment...</title></html>"),
    )
    .await;

    assert_eq!(error.kind(), ProviderErrorKind::Unavailable);
    assert_eq!(error.send_state(), UpstreamSendState::Sent);
    assert!(error.replay_is_safe());
    assert_eq!(
        availability,
        [AccountAvailability::Cooldown, AccountAvailability::Ready]
    );
}

#[tokio::test]
async fn empty_http_404_path_block_is_replay_safe_after_explicit_rejection() {
    let (error, availability) = rejected_http_operation(ResponseTemplate::new(404)).await;

    assert_eq!(error.kind(), ProviderErrorKind::Unavailable);
    assert_eq!(error.send_state(), UpstreamSendState::Sent);
    assert!(error.replay_is_safe());
    assert_eq!(
        availability,
        [AccountAvailability::Cooldown, AccountAvailability::Ready]
    );
}

#[tokio::test]
async fn history_failures_keep_only_their_allowlisted_machine_code() {
    for code in [
        "previous_response_not_found",
        "invalid_encrypted_content",
        "missing_tool_output",
        "no_tool_output",
    ] {
        let marker = format!("{code}-sensitive-marker");
        let (error, availability) =
            rejected_http_operation(ResponseTemplate::new(400).set_body_json(json!({
                "error": {"code": code, "message": marker}
            })))
            .await;

        assert_eq!(error.kind(), ProviderErrorKind::InvalidRequest);
        assert_eq!(
            error.upstream_code().map(SafeUpstreamValue::as_str),
            Some(code)
        );
        assert!(error.replay_is_safe());
        assert_eq!(
            error.continuation_failure(),
            Some(gateway_core::error::ContinuationFailure::HistoryUnavailable)
        );
        assert_eq!(
            availability,
            [AccountAvailability::Ready, AccountAvailability::Ready]
        );
        assert!(!format!("{error:?} {error}").contains(&marker));
    }
}

#[tokio::test]
async fn deactivated_workspace_is_banned_before_payment_classification() {
    let (error, availability) =
        rejected_http_operation(ResponseTemplate::new(402).set_body_json(json!({
            "detail": {"code": "deactivated_workspace", "message": "workspace deactivated"}
        })))
        .await;

    assert_eq!(error.kind(), ProviderErrorKind::PermissionDenied);
    assert!(error.replay_is_safe());
    assert_eq!(
        error.upstream_code().map(SafeUpstreamValue::as_str),
        Some("deactivated_workspace")
    );
    assert_eq!(
        availability,
        [AccountAvailability::Banned, AccountAvailability::Ready]
    );
}

#[tokio::test]
async fn unauthorized_expires_only_the_rejected_account() {
    let (error, availability) =
        rejected_http_operation(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"code": "token_expired", "message": "access token expired"}
        })))
        .await;

    assert_eq!(error.kind(), ProviderErrorKind::Unauthorized);
    assert!(error.replay_is_safe());
    assert_eq!(
        availability,
        [AccountAvailability::Expired, AccountAvailability::Ready]
    );
}

#[tokio::test]
async fn payment_required_exhausts_only_the_rejected_account() {
    let (error, availability) =
        rejected_http_operation(ResponseTemplate::new(402).set_body_json(json!({
            "error": {"code": "insufficient_quota", "message": "quota exhausted"}
        })))
        .await;

    assert_eq!(error.kind(), ProviderErrorKind::QuotaExhausted);
    assert!(error.replay_is_safe());
    assert_eq!(
        availability,
        [
            AccountAvailability::QuotaExhausted,
            AccountAvailability::Ready
        ]
    );
}

#[tokio::test]
async fn model_not_supported_is_instance_fallback_fact_without_account_poisoning() {
    let (error, availability) =
        rejected_http_operation(ResponseTemplate::new(404).set_body_json(json!({
            "error": {"code": "model_not_supported", "message": "model not supported"}
        })))
        .await;

    assert_eq!(error.kind(), ProviderErrorKind::Unsupported);
    assert!(error.replay_is_safe());
    assert_eq!(
        availability,
        [AccountAvailability::Ready, AccountAvailability::Ready]
    );
}

#[tokio::test]
async fn explicit_http_408_does_not_mark_provider_error_replay_safe() {
    let (error, _) = rejected_http_operation(ResponseTemplate::new(408)).await;

    assert_eq!(error.kind(), ProviderErrorKind::Timeout);
    assert!(!error.replay_is_safe());
}

#[tokio::test]
async fn generic_http_403_and_500_do_not_mark_provider_error_replay_safe() {
    for (status, expected_kind) in [
        (403, ProviderErrorKind::PermissionDenied),
        (500, ProviderErrorKind::Unavailable),
    ] {
        let (error, availability) = rejected_http_operation(ResponseTemplate::new(status)).await;

        assert_eq!(error.kind(), expected_kind);
        assert!(!error.replay_is_safe());
        assert_eq!(
            availability,
            [AccountAvailability::Ready, AccountAvailability::Ready]
        );
    }
}

#[tokio::test]
async fn permission_denied_failure_keeps_every_account_ready() {
    let (error, availability) = rejected_http_operation(ResponseTemplate::new(403)).await;

    assert_eq!(error.kind(), ProviderErrorKind::PermissionDenied);
    assert!(!error.replay_is_safe());
    assert_eq!(
        availability,
        [AccountAvailability::Ready, AccountAvailability::Ready]
    );
}

#[tokio::test]
async fn capability_query_keeps_model_gates_while_delegating_wire_features() {
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
    let wire_features =
        gateway_core::operation::CapabilityRequirements::new(OperationKind::Generate)
            .require(Feature::Tools)
            .require(Feature::Vision)
            .require(Feature::Reasoning)
            .require(Feature::JsonSchema);
    assert!(
        capabilities[0]
            .capabilities()
            .match_requirements(&wire_features)
            .is_some()
    );
    let oversized = gateway_core::operation::CapabilityRequirements::new(OperationKind::Generate)
        .with_minimum_context_tokens(128_001);
    assert!(
        capabilities[0]
            .capabilities()
            .match_requirements(&oversized)
            .is_none()
    );
    let compact = Operation::CompactConversation(CompactConversationRequest::new(
        GenerateRequest::new(vec![
            Message::new(
                MessageRole::User,
                vec![ContentPart::Text("compact".to_owned())],
            )
            .expect("message"),
        ])
        .expect("generate request"),
    ));
    assert!(
        capabilities[0]
            .capabilities()
            .match_requirements(&compact.capability_requirements())
            .is_none()
    );
}
