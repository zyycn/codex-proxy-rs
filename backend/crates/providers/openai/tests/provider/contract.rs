use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::Utc;
use futures::{SinkExt, StreamExt};
use gateway_core::engine::credential::{
    AccountAvailability, AccountFeedbackStats, OpaqueProviderData, ProviderAccountId,
    ProviderAccountStore as _, QuotaObservation,
};
use gateway_core::engine::provider::{Provider as _, ProviderRequest};
use gateway_core::engine::{
    AccountAttemptContext, AttemptContext, CancellationToken, ContinuationAttempt, ModelRequestId,
    ProviderAccountStateOwner, RequestAttemptContext, UpstreamSendState,
};
use gateway_core::error::ProviderErrorKind;
use gateway_core::event::GatewayEvent;
use gateway_core::operation::{GenerateRequest, Operation, ProtocolPayload, ProviderSessionState};
use gateway_core::policy::ClientApiKeyId;
use gateway_core::routing::{
    ConfigRevision, ModelCapabilities, ProviderKind, ProviderModel, PublicModelId, RoutingContext,
    RuntimeSnapshot, UpstreamModelId,
};
use provider_openai::credential::{
    CodexCookiePolicy, CodexCredentialCatalogService, CodexCredentialQuotaService,
    CodexCredentialSelector, ImportCodexOAuthCredential,
};
use provider_openai::transport::CodexWebSocketPool;
use provider_openai::transport::profile::{CodexWireProfile, CodexWireProfileState};
use provider_openai::{CodexProvider, OFFICIAL_CODEX_BASE_URL};
use serde_json::{Map, Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    time::timeout,
};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, frame::coding::CloseCode};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::support::{
    MemoryAccountStore, MemorySessionAffinity, MemorySessionExclusions, TestLeaseCoordinator,
    account_policy, agent_identity_service_with_pool, catalog_cache, profile, secret,
};
use crate::transport::accept_codex_test_websocket;

const OFFICIAL_FIXTURE: &[u8] =
    include_bytes!("../transport/fixtures/official_models_snapshot.json");
const CAPTURE_COMPLETED_SSE: &str = concat!(
    "event: response.completed\n",
    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_scope_capture\",\"model\":\"gpt-5.4\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n"
);

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

fn provider(store: &Arc<MemoryAccountStore>) -> CodexProvider {
    provider_with_affinity(store, Arc::new(MemorySessionAffinity::default()))
}

fn provider_with_affinity(
    store: &Arc<MemoryAccountStore>,
    session_affinity: Arc<MemorySessionAffinity>,
) -> CodexProvider {
    provider_with_affinity_and_base_url(store, session_affinity, OFFICIAL_CODEX_BASE_URL.to_owned())
}

fn provider_with_base_url(store: &Arc<MemoryAccountStore>, base_url: String) -> CodexProvider {
    provider_with_affinity_and_base_url(store, Arc::new(MemorySessionAffinity::default()), base_url)
}

fn provider_with_leases(
    store: &Arc<MemoryAccountStore>,
    leases: Arc<TestLeaseCoordinator>,
) -> CodexProvider {
    provider_with_affinity_and_base_url_and_leases(
        store,
        Arc::new(MemorySessionAffinity::default()),
        OFFICIAL_CODEX_BASE_URL.to_owned(),
        leases,
    )
}

fn provider_with_affinity_and_base_url(
    store: &Arc<MemoryAccountStore>,
    session_affinity: Arc<MemorySessionAffinity>,
    base_url: String,
) -> CodexProvider {
    provider_with_affinity_and_base_url_and_leases(
        store,
        session_affinity,
        base_url,
        Arc::new(TestLeaseCoordinator::default()),
    )
}

fn provider_with_affinity_and_base_url_and_leases(
    store: &Arc<MemoryAccountStore>,
    session_affinity: Arc<MemorySessionAffinity>,
    base_url: String,
    leases: Arc<TestLeaseCoordinator>,
) -> CodexProvider {
    let profile = wire_profile();
    let http = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("client");
    let websocket_pool = Arc::new(CodexWebSocketPool::default());
    let agent_identity = agent_identity_service_with_pool(store, Arc::clone(&websocket_pool));
    let catalog = Arc::new(CodexCredentialCatalogService::new(
        store.repository(),
        profile.clone(),
        http.clone(),
        base_url.clone(),
        Arc::clone(&agent_identity),
        catalog_cache(),
    ));
    let quota = Arc::new(CodexCredentialQuotaService::new(
        store.repository(),
        profile.clone(),
        http.clone(),
        base_url.clone(),
        Arc::clone(&agent_identity),
    ));
    let account_feedback = Arc::new(AccountFeedbackStats::default());
    let selector = Arc::new(CodexCredentialSelector::new(
        ProviderKind::new("openai").expect("provider"),
        store.repository(),
        leases,
        session_affinity,
        Arc::new(MemorySessionExclusions::default()),
        Arc::clone(&catalog),
        Arc::clone(&quota),
        Arc::clone(&agent_identity),
        Arc::clone(&account_feedback),
        CodexCookiePolicy::official().expect("cookie policy"),
        true,
    ));

    CodexProvider::new(
        selector,
        catalog,
        quota,
        agent_identity,
        account_feedback,
        http,
        profile,
        base_url,
        websocket_pool,
    )
    .expect("official OpenAI provider")
}

async fn create_account(store: &Arc<MemoryAccountStore>, id: &str) {
    create_account_with_enabled(store, id, true).await;
}

async fn create_account_with_enabled(store: &Arc<MemoryAccountStore>, id: &str, enabled: bool) {
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: id.to_owned(),
            name: id.to_owned(),
            secret: secret(&format!("at-{id}")),
            verified_account: profile(&format!("chatgpt-{id}")),
            next_refresh_at: Some(Utc::now() + chrono::Duration::minutes(30)),
            enabled,
        })
        .await;
}

fn generate_operation() -> Operation {
    Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            Map::from_iter([
                ("model".to_owned(), json!("gpt-5.4")),
                ("input".to_owned(), json!("hello")),
            ]),
        )
        .expect("OpenAI payload"),
    ))
}

fn http_generate_operation() -> Operation {
    let payload = ProtocolPayload::json_object(
        "openai",
        Map::from_iter([
            ("model".to_owned(), json!("gpt-5.4")),
            ("input".to_owned(), json!("hello")),
        ]),
    )
    .expect("OpenAI payload")
    .with_context(Map::from_iter([("use_websocket".to_owned(), json!(false))]));
    Operation::Generate(GenerateRequest::from_protocol_payload(payload))
}

fn planned_request(provider_name: &str, operation: Operation) -> ProviderRequest {
    let provider = ProviderKind::new(provider_name).expect("provider");
    let upstream_model = UpstreamModelId::new("gpt-5.4").expect("upstream model");
    let public_model = PublicModelId::new(upstream_model.as_str()).expect("public model");
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).expect("revision"),
        account_policy(),
        vec![provider.clone()],
        vec![ProviderModel::new(
            provider,
            upstream_model,
            ModelCapabilities::new(BTreeSet::from([operation.kind()]), Some(32_000)),
        )],
        Vec::new(),
    )
    .expect("snapshot");
    let plan = snapshot
        .plan(&public_model, &operation, &RoutingContext::default())
        .expect("routing plan");

    ProviderRequest::new(operation, plan.candidates()[0].clone())
}

fn context(request_id: &str, cancellation: CancellationToken) -> AttemptContext {
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new(request_id).expect("request id"),
            ClientApiKeyId::new("key_openai_contract").expect("client key id"),
        ),
        NonZeroU32::new(1).expect("attempt"),
        SystemTime::now() + Duration::from_secs(30),
        account_policy(),
        AccountAttemptContext::new(BTreeSet::<ProviderAccountId>::new(), None, None),
        None,
        cancellation,
    )
}

fn diagnostic_context(request_id: &str, account_id: &str) -> AttemptContext {
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new(request_id).expect("request id"),
            ClientApiKeyId::new("key_openai_contract").expect("client key id"),
        ),
        NonZeroU32::new(1).expect("attempt"),
        SystemTime::now() + Duration::from_secs(30),
        account_policy(),
        AccountAttemptContext::diagnostic(
            BTreeSet::new(),
            ProviderAccountId::new(account_id).expect("account id"),
            None,
        ),
        None,
        CancellationToken::new(),
    )
}

fn context_with_state_owner(request_id: &str, owner_account_id: &str) -> AttemptContext {
    let owner = ProviderAccountStateOwner::new(
        ProviderKind::new("openai").expect("provider"),
        ProviderAccountId::new(owner_account_id).expect("owner account id"),
    );
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new(request_id).expect("request id"),
            ClientApiKeyId::new("key_openai_contract").expect("client key id"),
        ),
        NonZeroU32::new(1).expect("attempt"),
        SystemTime::now() + Duration::from_secs(30),
        account_policy(),
        AccountAttemptContext::new(BTreeSet::new(), None, Some(owner)),
        None,
        CancellationToken::new(),
    )
}

async fn capture_scoped_http_request(
    request_id: &str,
    selected_account_id: &str,
    owner_account_id: &str,
    body: Map<String, serde_json::Value>,
    mut protocol_context: Map<String, serde_json::Value>,
) -> wiremock::Request {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, selected_account_id).await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CAPTURE_COMPLETED_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;
    protocol_context.insert("use_websocket".to_owned(), json!(false));
    let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", body)
            .expect("OpenAI payload")
            .with_context(protocol_context),
    ));
    let mut stream = provider_with_base_url(&store, server.uri())
        .execute(
            planned_request("openai", operation),
            context_with_state_owner(request_id, owner_account_id),
        )
        .await
        .expect("prepare scoped provider stream");
    while let Some(event) = stream.next().await {
        event.expect("scoped provider response");
    }
    let mut requests = server
        .received_requests()
        .await
        .expect("captured scoped request");
    assert_eq!(requests.len(), 1);
    requests.pop().expect("single scoped request")
}

fn captured_header_values(request: &wiremock::Request, name: &str) -> Vec<Vec<u8>> {
    request
        .headers
        .get_all(name)
        .iter()
        .map(|value| value.as_bytes().to_vec())
        .collect()
}

fn captured_request_body(request: &wiremock::Request) -> serde_json::Value {
    let body = if request
        .headers
        .get("content-encoding")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("zstd"))
    {
        zstd::stream::decode_all(std::io::Cursor::<&[u8]>::new(request.body.as_ref()))
            .expect("zstd body should decode")
    } else {
        request.body.to_vec()
    };
    serde_json::from_slice(&body).expect("captured JSON body")
}

async fn paused_chunked_sse_server(
    first_chunk: String,
    second_chunk: String,
) -> (
    String,
    oneshot::Sender<()>,
    oneshot::Receiver<()>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind chunked SSE listener");
    let base_url = format!(
        "http://{}",
        listener.local_addr().expect("listener address")
    );
    let (release_sender, release_receiver) = oneshot::channel();
    let (first_chunk_sender, first_chunk_sent) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept chunked SSE request");
        read_http_request(&mut stream).await;
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n",
            )
            .await
            .expect("write chunked SSE headers");
        write_http_chunk(&mut stream, &first_chunk).await;
        let _ = first_chunk_sender.send(());
        let _ = release_receiver.await;
        if !second_chunk.is_empty() {
            write_http_chunk(&mut stream, &second_chunk).await;
        }
        stream
            .write_all(b"0\r\n\r\n")
            .await
            .expect("terminate chunked SSE response");
    });
    (base_url, release_sender, first_chunk_sent, server)
}

async fn truncated_chunked_sse_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind truncated SSE listener");
    let base_url = format!(
        "http://{}",
        listener.local_addr().expect("listener address")
    );
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept SSE request");
        read_http_request(&mut stream).await;
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n20\r\npartial",
            )
            .await
            .expect("write truncated chunked response");
        stream.flush().await.expect("flush truncated response");
    });
    (base_url, server)
}

async fn read_http_request(stream: &mut TcpStream) {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let read = stream.read(&mut buffer).await.expect("read HTTP request");
        if read == 0 {
            return;
        }
        request.extend_from_slice(&buffer[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            return;
        }
    }
}

async fn write_http_chunk(stream: &mut TcpStream, body: &str) {
    stream
        .write_all(format!("{:X}\r\n", body.len()).as_bytes())
        .await
        .expect("write HTTP chunk size");
    stream
        .write_all(body.as_bytes())
        .await
        .expect("write HTTP chunk body");
    stream
        .write_all(b"\r\n")
        .await
        .expect("terminate HTTP chunk");
    stream.flush().await.expect("flush HTTP chunk");
}

#[tokio::test]
async fn openai_provider_rejects_a_foreign_provider_candidate_before_account_selection() {
    let store = Arc::new(MemoryAccountStore::default());
    let result = provider(&store)
        .execute(
            planned_request("xai", generate_operation()),
            context("req_foreign_provider", CancellationToken::new()),
        )
        .await;
    let Err(error) = result else {
        panic!("foreign provider candidate must fail")
    };

    assert_eq!(error.kind(), ProviderErrorKind::InvalidRequest);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
}

#[tokio::test]
async fn cancelled_attempt_fails_before_account_selection_or_upstream_send() {
    let store = Arc::new(MemoryAccountStore::default());
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let result = provider(&store)
        .execute(
            planned_request("openai", generate_operation()),
            context("req_cancelled", cancellation),
        )
        .await;
    let Err(error) = result else {
        panic!("cancelled attempt must fail")
    };

    assert_eq!(error.kind(), ProviderErrorKind::Cancelled);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
}

#[tokio::test]
async fn truncated_http_stream_allows_account_rotation_only_before_client_delivery() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_truncated_stream").await;
    let (base_url, server) = truncated_chunked_sse_server().await;
    let mut stream = provider_with_base_url(&store, base_url)
        .execute(
            planned_request("openai", http_generate_operation()),
            context("req_truncated_stream", CancellationToken::new()),
        )
        .await
        .expect("prepare HTTP stream");

    let error = loop {
        match stream.next().await {
            Some(Ok(_)) => {}
            Some(Err(error)) => break error,
            None => panic!("truncated response must surface a transport error"),
        }
    };
    server.await.expect("truncated SSE server");

    assert_eq!(error.kind(), ProviderErrorKind::Transport);
    assert_eq!(error.send_state(), UpstreamSendState::Sent);
    assert!(error.allows_pre_delivery_retry());
    assert!(!error.replay_is_safe());
}

#[tokio::test]
async fn openai_provider_keeps_a_compaction_trigger_as_a_regular_generate_request() {
    let store = Arc::new(MemoryAccountStore::default());
    let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            Map::from_iter([
                ("model".to_owned(), json!("gpt-5.4")),
                (
                    "input".to_owned(),
                    json!([
                        {"type": "message", "role": "user", "content": "hello"},
                        {"type": "compaction_trigger"}
                    ]),
                ),
            ]),
        )
        .expect("OpenAI payload"),
    ));
    let result = provider(&store)
        .execute(
            planned_request("openai", operation),
            context("req_compaction", CancellationToken::new()),
        )
        .await;
    let Err(error) = result else {
        panic!("missing OpenAI account must fail")
    };

    assert_eq!(error.kind(), ProviderErrorKind::NoEligibleAccount);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
}

#[tokio::test]
async fn generate_without_an_eligible_openai_account_fails_before_network_io() {
    let store = Arc::new(MemoryAccountStore::default());
    let result = provider(&store)
        .execute(
            planned_request("openai", generate_operation()),
            context("req_no_account", CancellationToken::new()),
        )
        .await;
    let Err(error) = result else {
        panic!("missing OpenAI account must fail")
    };

    assert_eq!(error.kind(), ProviderErrorKind::NoEligibleAccount);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
}

#[tokio::test]
async fn capacity_selection_error_preserves_classification_and_retry_after() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_capacity_busy").await;
    let leases = Arc::new(TestLeaseCoordinator::default());
    *leases.busy.lock().expect("lease busy lock") = true;

    let error = match provider_with_leases(&store, leases)
        .execute(
            planned_request("openai", generate_operation()),
            context("req_capacity_busy", CancellationToken::new()),
        )
        .await
    {
        Ok(_) => panic!("busy account selection must fail"),
        Err(error) => error,
    };

    assert_eq!(
        (error.kind(), error.send_state(), error.retry_after()),
        (
            ProviderErrorKind::AccountCapacityUnavailable,
            UpstreamSendState::NotSent,
            Some(Duration::from_millis(25)),
        )
    );
}

#[tokio::test]
async fn selection_infrastructure_errors_have_a_distinct_classification() {
    let store = Arc::new(MemoryAccountStore::default());
    store.fail_provider_listing();

    let error = match provider(&store)
        .execute(
            planned_request("openai", generate_operation()),
            context("req_selection_store_failure", CancellationToken::new()),
        )
        .await
    {
        Ok(_) => panic!("account store failure must fail selection"),
        Err(error) => error,
    };

    assert_eq!(
        (error.kind(), error.send_state()),
        (
            ProviderErrorKind::ProviderInfrastructureUnavailable,
            UpstreamSendState::NotSent,
        )
    );
}

#[tokio::test]
async fn websocket_close_details_are_only_exposed_through_the_client_error() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_websocket_close").await;
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind WebSocket listener");
    let base_url = format!(
        "http://{}",
        listener.local_addr().expect("listener address")
    );
    let server = tokio::spawn(async move {
        let (stream, _) = listener
            .accept()
            .await
            .expect("accept WebSocket connection");
        let mut websocket = accept_codex_test_websocket(stream).await;
        let _request = websocket
            .next()
            .await
            .expect("WebSocket request")
            .expect("valid WebSocket request");
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.output_text.delta",
                    "delta": "partial output"
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("send semantic WebSocket event");
        websocket
            .close(Some(CloseFrame {
                code: CloseCode::Size,
                reason: "message too big".into(),
            }))
            .await
            .expect("close WebSocket");
    });

    let mut stream = provider_with_base_url(&store, base_url)
        .execute(
            planned_request("openai", generate_operation()),
            context("req_websocket_close", CancellationToken::new()),
        )
        .await
        .expect("prepare WebSocket provider stream");
    let error = loop {
        match stream.next().await {
            Some(Ok(_)) => {}
            Some(Err(error)) => break error,
            None => panic!("WebSocket close must surface a provider error"),
        }
    };
    server.await.expect("WebSocket server");

    assert!(!format!("{error:?}").contains("message too big"));
    assert!(!error.to_string().contains("message too big"));
    assert_eq!(error.kind(), ProviderErrorKind::Transport);
    assert_eq!(error.send_state(), UpstreamSendState::Ambiguous);
    assert!(error.allows_pre_delivery_retry());
    let detail = error
        .client_visible_upstream_error()
        .expect("WebSocket close detail");
    assert_eq!(detail.message(), "message too big");
    assert_eq!(detail.code(), Some("1009"));
    assert_eq!(detail.error_type(), Some("websocket_close_error"));
}

#[tokio::test]
async fn disabled_account_is_excluded_from_normal_scheduling() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account_with_enabled(&store, "acct_disabled_scheduling", false).await;

    let result = provider(&store)
        .execute(
            planned_request("openai", generate_operation()),
            context("req_disabled_scheduling", CancellationToken::new()),
        )
        .await;
    let Err(error) = result else {
        panic!("disabled account must not be scheduled normally")
    };

    assert_eq!(error.kind(), ProviderErrorKind::NoEligibleAccount);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
}

#[tokio::test]
async fn opaque_provider_options_do_not_change_openai_account_selection() {
    let store = Arc::new(MemoryAccountStore::default());
    let payload = ProtocolPayload::json_object(
        "openai",
        Map::from_iter([
            ("model".to_owned(), json!("gpt-5.4")),
            ("input".to_owned(), json!("hello")),
            (
                "provider_options".to_owned(),
                json!({"providers": {"openai": {"transport": "unsupported"}}}),
            ),
        ]),
    )
    .expect("OpenAI payload");
    let generation = GenerateRequest::from_protocol_payload(payload);
    let result = provider(&store)
        .execute(
            planned_request("openai", Operation::Generate(generation)),
            context("req_bad_transport", CancellationToken::new()),
        )
        .await;
    let Err(error) = result else {
        panic!("missing account must fail")
    };

    assert_eq!(error.kind(), ProviderErrorKind::NoEligibleAccount);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
}

#[tokio::test]
async fn same_account_scope_preserves_future_protocol_shapes() {
    let request = capture_scoped_http_request(
        "req_scope_same",
        "acct_scope_same",
        "acct_scope_same",
        json!({
            "model": "gpt-5.4",
            "input": "hello",
            "authorization": "client-extension-value",
            "installation_id": "client-installation",
            "turnState": {"future": true},
            "turnMetadata": ["future", "shape"],
            "x-codex-turn-state": 17,
            "x-codex-turn-metadata": false,
            "client_metadata": {
                "token": "client-metadata-extension",
                "x-codex-turn-state": {"future": "metadata"},
                "x-codex-turn-metadata": [1, 2, 3],
                "turnMetadata": {"opaque": true}
            }
        })
        .as_object()
        .expect("request object")
        .clone(),
        Map::new(),
    )
    .await;
    let body: serde_json::Value = captured_request_body(&request);

    assert_eq!(
        body.get("authorization"),
        Some(&json!("client-extension-value"))
    );
    assert_eq!(body.get("turnState"), Some(&json!({"future": true})));
    assert_eq!(body.get("turnMetadata"), Some(&json!(["future", "shape"])));
    assert_eq!(body.get("x-codex-turn-state"), Some(&json!(17)));
    assert_eq!(body.get("x-codex-turn-metadata"), Some(&json!(false)));
    assert_eq!(
        body.pointer("/client_metadata/token"),
        Some(&json!("client-metadata-extension"))
    );
    assert_eq!(
        body.pointer("/client_metadata/x-codex-turn-state"),
        Some(&json!({"future": "metadata"}))
    );
    assert_ne!(
        body.get("installation_id"),
        Some(&json!("client-installation"))
    );
}

#[tokio::test]
async fn cross_account_scope_removes_only_account_bound_body_fields() {
    let request = capture_scoped_http_request(
        "req_scope_switch",
        "acct_scope_new",
        "acct_scope_old",
        json!({
            "model": "gpt-5.4",
            "input": "hello",
            "authorization": "client-extension-value",
            "conversation": "upstream-account-handle",
            "conversation_id": "client-correlation",
            "installation_id": "client-installation",
            "client_metadata": ["future", "shape"],
            "future_field": {"keep": true}
        })
        .as_object()
        .expect("request object")
        .clone(),
        Map::new(),
    )
    .await;
    let body: serde_json::Value = captured_request_body(&request);

    assert!(body.get("authorization").is_none());
    assert!(body.get("conversation").is_none());
    assert_eq!(
        body.get("conversation_id"),
        Some(&json!("client-correlation"))
    );
    assert_eq!(
        body.get("client_metadata"),
        Some(&json!(["future", "shape"]))
    );
    assert_eq!(body.get("future_field"), Some(&json!({"keep": true})));
    assert_ne!(
        body.get("installation_id"),
        Some(&json!("client-installation"))
    );
}

#[tokio::test]
async fn cross_account_scope_sanitizes_only_known_turn_metadata_fields() {
    let request = capture_scoped_http_request(
        "req_scope_metadata",
        "acct_metadata_new",
        "acct_metadata_old",
        json!({
            "model": "gpt-5.4",
            "input": "hello",
            "turnMetadata": r#"{"account_id":"old-account","future":{"keep":true}}"#,
            "turn_metadata": "future-opaque-shape",
            "x-codex-turn-metadata": r#"{"conversation":"old-conversation","safe":17}"#
        })
        .as_object()
        .expect("request object")
        .clone(),
        Map::new(),
    )
    .await;
    let body: serde_json::Value = captured_request_body(&request);
    let turn_metadata = body
        .get("turnMetadata")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
        .expect("sanitized turnMetadata");
    let codex_turn_metadata = body
        .get("x-codex-turn-metadata")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
        .expect("sanitized x-codex-turn-metadata");

    assert_eq!(turn_metadata, json!({"future": {"keep": true}}));
    assert_eq!(codex_turn_metadata, json!({"safe": 17}));
    assert_eq!(
        body.get("turn_metadata"),
        Some(&json!("future-opaque-shape"))
    );
}

#[tokio::test]
async fn continuation_replay_should_use_the_client_full_input_without_legacy_transcript() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_client_history").await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CAPTURE_COMPLETED_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;
    let client_input = json!([
        {"role": "user", "content": "complete client history"},
        {"role": "assistant", "content": "previous answer"},
        {"role": "user", "content": "current turn"}
    ]);
    let generation = GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            Map::from_iter([
                ("model".to_owned(), json!("gpt-5.4")),
                ("input".to_owned(), client_input.clone()),
            ]),
        )
        .expect("OpenAI payload")
        .with_context(Map::from_iter([("use_websocket".to_owned(), json!(false))])),
    )
    .with_provider_session_state(
        ProviderSessionState::new(
            "openai",
            Map::from_iter([
                ("account_id".to_owned(), json!("acct_client_history")),
                ("conversation_id".to_owned(), json!("conversation")),
                ("continuation_scope".to_owned(), json!("replay_required")),
                (
                    "transcript".to_owned(),
                    json!([{"client_input": {"role": "user", "content": "stale proxy copy"}}]),
                ),
            ]),
        )
        .expect("provider session state"),
    );
    let mut stream = provider_with_base_url(&store, server.uri())
        .execute(
            planned_request("openai", Operation::Generate(generation)),
            context("req_client_history", CancellationToken::new())
                .with_continuation_attempt(ContinuationAttempt::ReplayAny),
        )
        .await
        .expect("prepare replay stream");
    while let Some(event) = stream.next().await {
        event.expect("replay response");
    }
    let requests = server
        .received_requests()
        .await
        .expect("captured replay request");
    let body: Value = captured_request_body(&requests[0]);

    assert_eq!(body.get("input"), Some(&client_input));
}

#[tokio::test]
async fn completed_response_session_state_should_not_copy_the_conversation_transcript() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_bounded_session_state").await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CAPTURE_COMPLETED_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;
    let mut stream = provider_with_base_url(&store, server.uri())
        .execute(
            planned_request("openai", http_generate_operation()),
            context("req_bounded_session_state", CancellationToken::new()),
        )
        .await
        .expect("prepare provider stream");
    let mut session_payload = None;
    while let Some(event) = stream.next().await {
        let event = event.expect("provider response");
        if let Some(update) = event.session_update() {
            session_payload = Some(update.payload().clone());
        }
    }

    assert!(
        !session_payload
            .expect("completed response session update")
            .contains_key("transcript")
    );
}

#[tokio::test]
async fn account_change_drops_only_account_bound_opaque_headers() {
    let protocol_context = Map::from_iter([(
        "opaque_request_headers".to_owned(),
        json!([
            ["x-codex-turn-state", STANDARD.encode(b"turn-first")],
            ["x-codex-turn-state", STANDARD.encode(b"turn-\x80")],
            [
                "x-codex-turn-metadata",
                STANDARD.encode(br#"{"installation_id":"client-installation","safe":true}"#)
            ],
            ["x-openai-future", STANDARD.encode(b"keep-on-switch")]
        ]),
    )]);
    let body = json!({"model": "gpt-5.4", "input": "hello"})
        .as_object()
        .expect("request object")
        .clone();
    let same_account = capture_scoped_http_request(
        "req_header_same",
        "acct_header_same",
        "acct_header_same",
        body.clone(),
        protocol_context.clone(),
    )
    .await;
    let cross_account = capture_scoped_http_request(
        "req_header_switch",
        "acct_header_new",
        "acct_header_old",
        body,
        protocol_context,
    )
    .await;

    assert_eq!(
        captured_header_values(&same_account, "x-codex-turn-state"),
        vec![b"turn-first".to_vec(), b"turn-\x80".to_vec()]
    );
    assert_eq!(
        captured_header_values(&same_account, "x-openai-future"),
        vec![b"keep-on-switch".to_vec()]
    );
    assert!(captured_header_values(&cross_account, "x-codex-turn-state").is_empty());
    assert!(captured_header_values(&cross_account, "x-codex-turn-metadata").is_empty());
    assert_eq!(
        captured_header_values(&cross_account, "x-openai-future"),
        vec![b"keep-on-switch".to_vec()]
    );
}

#[tokio::test]
async fn prompt_cache_key_should_become_an_opaque_session_affinity_lookup_key() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_affinity").await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let generation = GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            Map::from_iter([
                ("model".to_owned(), json!("gpt-5.4")),
                ("input".to_owned(), json!("hello")),
                ("prompt_cache_key".to_owned(), json!("raw-prompt-cache-key")),
            ]),
        )
        .expect("OpenAI payload"),
    );

    let stream = provider_with_affinity(&store, Arc::clone(&affinity))
        .execute(
            planned_request("openai", Operation::Generate(generation)),
            context("req_affinity_key", CancellationToken::new()),
        )
        .await
        .expect("prepare provider stream");
    drop(stream);

    let keys = affinity.lookup_keys();
    assert_eq!(keys.len(), 1);
    assert_ne!(keys[0], "raw-prompt-cache-key");
    assert_eq!(keys[0].len(), 64);
    assert!(keys[0].bytes().all(|byte| byte.is_ascii_hexdigit()));
}

#[tokio::test]
async fn explicit_session_id_should_override_turn_specific_prompt_cache_keys_for_affinity() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_session_affinity").await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let provider = provider_with_affinity(&store, Arc::clone(&affinity));

    for (request_id, prompt_cache_key) in [
        ("req_session_affinity_first", "turn-cache-first"),
        ("req_session_affinity_second", "turn-cache-second"),
    ] {
        let generation = GenerateRequest::from_protocol_payload(
            ProtocolPayload::json_object(
                "openai",
                Map::from_iter([
                    ("model".to_owned(), json!("gpt-5.4")),
                    ("input".to_owned(), json!("hello")),
                    ("prompt_cache_key".to_owned(), json!(prompt_cache_key)),
                ]),
            )
            .expect("OpenAI payload")
            .with_context(Map::from_iter([(
                "session_id".to_owned(),
                json!("stable-client-session"),
            )])),
        );

        let stream = provider
            .execute(
                planned_request("openai", Operation::Generate(generation)),
                context(request_id, CancellationToken::new()),
            )
            .await
            .expect("prepare provider stream");
        drop(stream);
    }

    let keys = affinity.lookup_keys();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0], keys[1]);
}

#[tokio::test]
async fn invalid_local_conversation_id_should_fall_back_to_an_opaque_affinity_key() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_local_affinity").await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let generation = GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            Map::from_iter([
                ("model".to_owned(), json!("gpt-5.4")),
                ("input".to_owned(), json!("hello")),
            ]),
        )
        .expect("OpenAI payload"),
    )
    .with_provider_session_state(
        ProviderSessionState::new(
            "openai",
            Map::from_iter([
                ("account_id".to_owned(), json!("acct_local_affinity")),
                (
                    "conversation_id".to_owned(),
                    json!("lc_UppercaseBase64ConversationId"),
                ),
                ("continuation_scope".to_owned(), json!("replay_required")),
                ("transcript".to_owned(), json!([])),
            ]),
        )
        .expect("provider session state"),
    );

    let stream = provider_with_affinity(&store, Arc::clone(&affinity))
        .execute(
            planned_request("openai", Operation::Generate(generation)),
            context("req_local_affinity", CancellationToken::new()),
        )
        .await
        .expect("prepare provider stream");
    drop(stream);

    let keys = affinity.lookup_keys();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].len(), 64);
    assert!(keys[0].bytes().all(|byte| byte.is_ascii_hexdigit()));
}

#[tokio::test]
async fn completed_response_persists_session_affinity_before_stream_consumer_stops_polling() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_completed_affinity").await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(concat!(
                    "event: response.created\n",
                    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_affinity\",\"model\":\"gpt-5.4\",\"service_tier\":\"default\"}}\n\n",
                    "event: response.completed\n",
                    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_affinity\",\"model\":\"gpt-5.4\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n"
                )),
        )
        .mount(&server)
        .await;
    let generation = GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            Map::from_iter([
                ("model".to_owned(), json!("gpt-5.4")),
                ("input".to_owned(), json!("hello")),
                ("prompt_cache_key".to_owned(), json!("affinity-key")),
                ("service_tier".to_owned(), json!("priority")),
            ]),
        )
        .expect("OpenAI payload")
        .with_context(Map::from_iter([("use_websocket".to_owned(), json!(false))])),
    );
    let mut stream =
        provider_with_affinity_and_base_url(&store, Arc::clone(&affinity), server.uri())
            .execute(
                planned_request("openai", Operation::Generate(generation)),
                context("req_completed_affinity", CancellationToken::new()),
            )
            .await
            .expect("prepare provider stream");

    let mut observed_service_tier = None;
    let mut upstream_service_tier = None;
    while let Some(event) = stream.next().await {
        let event = event.expect("provider event");
        if let Some(observation) = event.response_observation() {
            if let Some(service_tier) = observation.service_tier() {
                observed_service_tier = Some(service_tier.to_owned());
            }
            upstream_service_tier = observation
                .provider_metadata()
                .and_then(|metadata| serde_json::from_str::<Value>(metadata.as_json()).ok())
                .and_then(|metadata| {
                    metadata
                        .get("upstreamServiceTier")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .or(upstream_service_tier);
        }
        if event
            .canonical_facts()
            .iter()
            .any(|event| matches!(event, GatewayEvent::Completed(_)))
        {
            break;
        }
    }
    drop(stream);

    assert_eq!(affinity.binding_count(), 1);
    assert_eq!(observed_service_tier.as_deref(), Some("priority"));
    assert_eq!(upstream_service_tier.as_deref(), Some("default"));
}

#[tokio::test]
async fn response_failed_before_semantic_output_is_atomic_and_persists_quota_lock() {
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_atomic_failure";
    create_account(&store, account_id).await;
    let (base_url, release, _first_chunk_sent, server) = paused_chunked_sse_server(
        concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_atomic_failure\",\"model\":\"gpt-5.4\",\"status\":\"in_progress\"}}\n\n",
            "event: response.failed\n",
            "data: {\"type\":\"response.failed\",\"status_code\":429,\"retry_after_seconds\":17,\"response\":{\"id\":\"resp_atomic_failure\",\"status\":\"failed\",\"error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"atomic upstream failure\"}}}\n\n"
        )
        .to_owned(),
        String::new(),
    )
    .await;
    let mut stream = provider_with_base_url(&store, base_url)
        .execute(
            planned_request("openai", http_generate_operation()),
            context("req_atomic_failure", CancellationToken::new()),
        )
        .await
        .expect("prepare provider stream");
    let mut visible_before_error = 0;
    let mut failure = loop {
        match stream.next().await {
            Some(Ok(event)) => visible_before_error += usize::from(event.has_client_event()),
            Some(Err(error)) => break error,
            None => panic!("response.failed must produce a typed failure"),
        }
    };

    assert_eq!(visible_before_error, 0);
    assert_eq!(failure.kind(), ProviderErrorKind::RateLimited);
    assert_eq!(failure.send_state(), UpstreamSendState::Sent);
    assert_eq!(failure.upstream_status(), Some(429));
    assert!(failure.replay_is_safe());
    let events = failure.take_atomic_client_events();
    assert_eq!(
        events
            .iter()
            .filter_map(|event| event.wire_event()?.event_type())
            .collect::<Vec<_>>(),
        vec!["response.created", "response.failed"]
    );
    let account = store.account(account_id).expect("rate-limited account");
    // 429 只记录限流窗口，不改变 availability（限流不改变账号可用性）。
    assert_eq!(account.availability(), AccountAvailability::Ready);
    let _ = release.send(());
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn usage_limit_failure_returns_promptly_and_refreshes_the_authoritative_quota_snapshot() {
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_usage_limit_request_path";
    create_account(&store, account_id).await;
    let account = store.account(account_id).expect("created account");
    store
        .compare_and_swap_quota(QuotaObservation {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            quota: Some(OpaqueProviderData::new(
                json!({
                    "rate_limit": {
                        "allowed": true,
                        "limit_reached": false,
                        "primary_window": {"used_percent": 99, "reset_at": 1_900_000_000}
                    }
                })
                .as_object()
                .expect("stale quota object")
                .clone(),
            )),
            observed_at: Some(SystemTime::now()),
            limit_reached: None,
        })
        .await
        .expect("seed stale passive quota");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header("authorization", format!("Bearer at-{account_id}")))
        // 生产环境会短暂返回互相矛盾的快照：数字已到 100%，但布尔位仍声称可用。
        // 后台同步应覆盖展示数据，不能据此撤销真实请求已经确认的额度耗尽。
        .respond_with(
            ResponseTemplate::new(200)
                // 验证后台 usage 同步不能把原始的额度错误响应拖到查询完成之后。
                .set_delay(Duration::from_millis(750))
                .set_body_json(json!({
                    "rate_limit": {
                        "allowed": true,
                        "limit_reached": false,
                        "primary_window": {"used_percent": 100, "reset_at": 1_900_000_000}
                    }
                })),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(concat!(
                    "event: response.created\n",
                    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_usage_limit_confirm\",\"model\":\"gpt-5.4\",\"status\":\"in_progress\"}}\n\n",
                    "event: response.failed\n",
                    "data: {\"type\":\"response.failed\",\"status_code\":429,\"retry_after_seconds\":86400,\"response\":{\"id\":\"resp_usage_limit_confirm\",\"status\":\"failed\",\"error\":{\"code\":\"usage_limit_reached\",\"message\":\"usage limit reached\"}}}\n\n"
                )),
        )
        .mount(&server)
        .await;

    let mut stream = provider_with_base_url(&store, server.uri())
        .execute(
            planned_request("openai", http_generate_operation()),
            context("req_usage_limit_confirm", CancellationToken::new()),
        )
        .await
        .expect("prepare provider stream");
    let failure = timeout(Duration::from_millis(300), async {
        loop {
            match stream.next().await {
                Some(Ok(_)) => {}
                Some(Err(error)) => break error,
                None => panic!("usage-limit failure must surface a typed failure"),
            }
        }
    })
    .await
    .expect("usage refresh must not block the original quota failure");

    assert_eq!(failure.kind(), ProviderErrorKind::QuotaExhausted);
    let account = store.account(account_id).expect("usage-limit account");
    assert_eq!(account.availability(), AccountAvailability::QuotaExhausted);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let requests = server.received_requests().await.expect("received requests");
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.url.path() == "/api/codex/usage")
            .count(),
        0,
        "usage refresh must wait for the upstream quota settlement delay"
    );

    let quota = timeout(Duration::from_secs(5), async {
        loop {
            let quota = store
                .get_quotas(&[account.id().clone()])
                .await
                .expect("read refreshed quota")
                .into_iter()
                .next()
                .and_then(|observation| observation.quota)
                .expect("authoritative quota snapshot");
            let quota = Value::Object(quota.into_inner());
            if quota
                .pointer("/rate_limit/primary_window/used_percent")
                .and_then(Value::as_u64)
                == Some(100)
            {
                break quota;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("background usage refresh must replace the stale quota snapshot");
    assert_eq!(
        quota
            .pointer("/rate_limit/primary_window/used_percent")
            .and_then(Value::as_u64),
        Some(100)
    );
    let requests = server.received_requests().await.expect("received requests");
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.url.path() == "/api/codex/usage")
            .count(),
        1
    );
    assert_eq!(
        store
            .account(account_id)
            .expect("account after stale full usage refresh")
            .availability(),
        AccountAvailability::QuotaExhausted,
        "a contradictory full usage snapshot must not unlock a confirmed exhausted account"
    );
}

#[tokio::test]
async fn ordinary_request_should_hold_created_until_later_failure_can_rotate() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_first_event_latency").await;
    let (base_url, release, first_chunk_sent, server) = paused_chunked_sse_server(
        concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_first_event\",\"model\":\"gpt-5.4\",\"status\":\"in_progress\"}}\n\n"
        )
        .to_owned(),
        concat!(
            "event: response.failed\n",
            "data: {\"type\":\"response.failed\",\"status_code\":429,\"response\":{\"id\":\"resp_first_event\",\"status\":\"failed\",\"error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"later failure\"}}}\n\n"
        )
        .to_owned(),
    )
    .await;
    let mut stream = provider_with_base_url(&store, base_url)
        .execute(
            planned_request("openai", http_generate_operation()),
            context("req_first_event_latency", CancellationToken::new()),
        )
        .await
        .expect("prepare provider stream");

    let mut first_chunk_sent = Box::pin(first_chunk_sent);
    loop {
        tokio::select! {
            sent = &mut first_chunk_sent => {
                sent.expect("first upstream chunk");
                break;
            }
            next = stream.next() => {
                let event = next
                    .expect("provider stream must stay open")
                    .expect("provider event");
                assert!(!event.has_client_event(), "response.created must remain replayable");
            }
        }
    }

    let exposed = timeout(Duration::from_millis(100), async {
        loop {
            let next = stream
                .next()
                .await
                .expect("provider stream must stay open")
                .expect("provider event");
            if next.has_client_event() {
                return next;
            }
        }
    })
    .await;
    assert!(
        exposed.is_err(),
        "a structural event must not commit the downstream before a later 429"
    );

    release.send(()).expect("release second upstream chunk");
    let mut failure = loop {
        let next = timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("later failure must arrive after release")
            .expect("provider stream must return typed failure");
        match next {
            Ok(event) => assert!(
                !event.has_client_event(),
                "failure attempt leaked downstream"
            ),
            Err(error) => break error,
        }
    };

    assert!(failure.replay_is_safe());
    assert_eq!(
        failure
            .take_atomic_client_events()
            .iter()
            .filter_map(|event| event.wire_event()?.event_type())
            .collect::<Vec<_>>(),
        vec!["response.created", "response.failed"]
    );
    server.await.expect("chunked SSE server");
}

#[tokio::test]
async fn ordinary_request_should_bound_structural_event_replay_grace() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_bounded_replay_grace").await;
    let (base_url, release, _first_chunk_sent, server) = paused_chunked_sse_server(
        concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_bounded_grace\",\"model\":\"gpt-5.4\",\"status\":\"in_progress\"}}\n\n"
        )
        .to_owned(),
        String::new(),
    )
    .await;
    let mut stream = provider_with_base_url(&store, base_url)
        .execute(
            planned_request("openai", http_generate_operation()),
            context("req_bounded_replay_grace", CancellationToken::new()),
        )
        .await
        .expect("prepare provider stream");

    let first_event = timeout(Duration::from_secs(2), async {
        loop {
            let event = stream
                .next()
                .await
                .expect("provider stream must stay open")
                .expect("provider event");
            if event.has_client_event() {
                return event;
            }
        }
    })
    .await
    .expect("response.created must be released after the bounded grace period");

    assert_eq!(
        first_event.wire_event().and_then(|wire| wire.event_type()),
        Some("response.created")
    );
    release.send(()).expect("finish upstream response");
    while let Some(event) = stream.next().await {
        event.expect("clean upstream EOF");
    }
    server.await.expect("chunked SSE server");
}

#[tokio::test]
async fn continuation_should_hold_created_until_semantic_output() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_continuation_prefetch").await;
    let (base_url, release, first_chunk_sent, server) = paused_chunked_sse_server(
        concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_continuation\",\"model\":\"gpt-5.4\",\"status\":\"in_progress\"}}\n\n"
        )
        .to_owned(),
        concat!(
            "event: response.content_part.added\n",
            "data: {\"type\":\"response.content_part.added\",\"output_index\":0,\"content_index\":0,\"part\":{\"type\":\"output_text\"}}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"hello\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_continuation\",\"model\":\"gpt-5.4\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n"
        )
        .to_owned(),
    )
    .await;
    let mut stream = provider_with_base_url(&store, base_url)
        .execute(
            planned_request("openai", http_generate_operation()),
            context("req_continuation_prefetch", CancellationToken::new())
                .with_continuation_attempt(ContinuationAttempt::Native),
        )
        .await
        .expect("prepare provider stream");

    let mut first_chunk_sent = Box::pin(first_chunk_sent);
    loop {
        tokio::select! {
            sent = &mut first_chunk_sent => {
                sent.expect("first upstream chunk");
                break;
            }
            next = stream.next() => {
                let event = next
                    .expect("provider stream must stay open")
                    .expect("provider event");
                assert!(!event.has_client_event(), "created was exposed before the first chunk barrier");
            }
        }
    }

    let blocked = timeout(Duration::from_millis(100), async {
        loop {
            let next = stream
                .next()
                .await
                .expect("provider stream must stay open")
                .expect("provider event");
            if next.has_client_event() {
                return next;
            }
        }
    })
    .await;
    assert!(blocked.is_err());

    release.send(()).expect("release semantic output chunk");
    let first_event = loop {
        let next = timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("semantic output must release the prefetched batch")
            .expect("provider stream must stay open")
            .expect("provider event");
        if next.has_client_event() {
            break next;
        }
    };
    assert_eq!(
        first_event.wire_event().and_then(|wire| wire.event_type()),
        Some("response.created")
    );
    while let Some(event) = stream.next().await {
        event.expect("continuation response must complete");
    }
    server.await.expect("chunked SSE server");
}

#[tokio::test]
async fn bare_response_failed_should_remain_an_atomic_replay_safe_failure() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_bare_atomic_failure").await;
    let (base_url, release, _first_chunk_sent, server) = paused_chunked_sse_server(
        concat!(
            "event: response.failed\n",
            "data: {\"type\":\"response.failed\",\"status_code\":429,\"response\":{\"id\":\"resp_bare_failure\",\"status\":\"failed\",\"error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"bare failure\"}}}\n\n"
        )
        .to_owned(),
        String::new(),
    )
    .await;
    let mut stream = provider_with_base_url(&store, base_url)
        .execute(
            planned_request("openai", http_generate_operation()),
            context("req_bare_atomic_failure", CancellationToken::new()),
        )
        .await
        .expect("prepare provider stream");
    let mut visible_before_error = 0;
    let mut failure = loop {
        match stream.next().await {
            Some(Ok(event)) => visible_before_error += usize::from(event.has_client_event()),
            Some(Err(error)) => break error,
            None => panic!("bare response.failed must produce a typed failure"),
        }
    };

    assert_eq!(visible_before_error, 0);
    assert!(failure.replay_is_safe());
    let events = failure.take_atomic_client_events();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].wire_event().and_then(|wire| wire.event_type()),
        Some("response.failed")
    );
    assert!(events[0]
        .canonical_facts()
        .iter()
        .any(|event| matches!(event, GatewayEvent::Started(meta) if meta.response_id() == "resp_bare_failure")));
    let _ = release.send(());
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn continuation_prefetch_over_64_kib_should_commit_wire_without_protocol_failure() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_prefetch_limit").await;
    let padding = "x".repeat(64 * 1024);
    let body = format!(
        "event: response.created\ndata: {}\n\n",
        json!({
            "type": "response.created",
            "response": {
                "id": "resp_prefetch_limit",
                "model": "gpt-5.4",
                "status": "in_progress",
                "padding": padding,
            }
        })
    );
    assert!(body.len() > 64 * 1024);
    let (base_url, release, _first_chunk_sent, server) =
        paused_chunked_sse_server(body, String::new()).await;
    let mut stream = provider_with_base_url(&store, base_url)
        .execute(
            planned_request("openai", http_generate_operation()),
            context("req_prefetch_limit", CancellationToken::new())
                .with_continuation_attempt(ContinuationAttempt::Native),
        )
        .await
        .expect("prepare provider stream");
    let visible = loop {
        let event = timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("prefetch threshold must release buffered wire")
            .expect("provider stream remains open")
            .expect("threshold cannot create a protocol failure");
        if event.has_client_event() {
            break event;
        }
    };

    assert_eq!(
        visible.wire_event().and_then(|wire| wire.event_type()),
        Some("response.created")
    );
    release.send(()).expect("finish upstream response");
    while let Some(event) = stream.next().await {
        event.expect("clean upstream EOF cannot become a protocol failure");
    }
    server.await.expect("chunked SSE server");
}

#[tokio::test]
async fn response_failed_after_semantic_output_is_exposed_and_not_replay_safe() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_semantic_failure").await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(concat!(
                    "event: response.created\n",
                    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_semantic_failure\",\"model\":\"gpt-5.4\",\"status\":\"in_progress\"}}\n\n",
                    "event: response.content_part.added\n",
                    "data: {\"type\":\"response.content_part.added\",\"output_index\":0,\"content_index\":0,\"part\":{\"type\":\"output_text\"}}\n\n",
                    "event: response.output_text.delta\n",
                    "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"hello\"}\n\n",
                    "event: response.failed\n",
                    "data: {\"type\":\"response.failed\",\"status_code\":429,\"response\":{\"id\":\"resp_semantic_failure\",\"status\":\"failed\",\"error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"failure after output\"}}}\n\n"
                )),
        )
        .mount(&server)
        .await;
    let mut stream = provider_with_base_url(&store, server.uri())
        .execute(
            planned_request("openai", http_generate_operation()),
            context("req_semantic_failure", CancellationToken::new()),
        )
        .await
        .expect("prepare provider stream");
    let mut wire_types = Vec::new();
    let failure = loop {
        match stream.next().await {
            Some(Ok(event)) => {
                if let Some(event_type) = event.wire_event().and_then(|wire| wire.event_type()) {
                    wire_types.push(event_type.to_owned());
                }
            }
            Some(Err(error)) => break error,
            None => panic!("response.failed must produce a typed failure"),
        }
    };

    assert_eq!(
        wire_types,
        vec![
            "response.created",
            "response.content_part.added",
            "response.output_text.delta",
            "response.failed"
        ]
    );
    assert!(!failure.replay_is_safe());
    assert!(!failure.has_atomic_client_events());
}

#[tokio::test]
async fn disabled_account_diagnostic_uses_upstream_without_persisting_account_state() {
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_disabled_diagnostic";
    create_account(&store, account_id).await;
    let account = store.account(account_id).expect("test account");
    store
        .repository()
        .apply_state(
            &account,
            AccountAvailability::QuotaExhausted,
            SystemTime::now(),
        )
        .await
        .expect("seed quota-exhausted state");
    store
        .set_enabled(account.id(), false)
        .await
        .expect("disable test account");

    let affinity = Arc::new(MemorySessionAffinity::default());
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .insert_header("x-codex-active-limit", "codex")
                .insert_header("x-codex-primary-used-percent", "100")
                .insert_header("x-codex-primary-window-minutes", "300")
                .insert_header("x-codex-primary-reset-at", "1900000000")
                .insert_header("x-codex-limit-reached", "true")
                .set_body_string(concat!(
                    "event: response.created\n",
                    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_disabled_diagnostic\",\"model\":\"gpt-5.4\"}}\n\n",
                    "event: response.completed\n",
                    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_disabled_diagnostic\",\"model\":\"gpt-5.4\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n"
                )),
        )
        .mount(&server)
        .await;

    let mut stream =
        provider_with_affinity_and_base_url(&store, Arc::clone(&affinity), server.uri())
            .execute(
                planned_request("openai", http_generate_operation()),
                diagnostic_context("req_disabled_diagnostic", account_id),
            )
            .await
            .expect("disabled diagnostic should prepare a fixed-account stream");
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
        .account(account_id)
        .expect("disabled test account after test");
    assert!(!account.enabled());
    assert_eq!(account.availability(), AccountAvailability::QuotaExhausted);
    assert!(!store.has_quota(account_id));
    assert_eq!(affinity.binding_count(), 0);
}

#[tokio::test]
async fn successful_response_preserves_quota_exhaustion_from_rate_limit_headers() {
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_success_exhausted";
    create_account(&store, account_id).await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .insert_header("x-codex-active-limit", "codex")
                .insert_header("x-codex-primary-used-percent", "100")
                .insert_header("x-codex-primary-window-minutes", "300")
                .insert_header("x-codex-primary-reset-at", "1900000000")
                .insert_header("x-codex-limit-reached", "true")
                .set_body_string(concat!(
                    "event: response.created\n",
                    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_success_exhausted\",\"model\":\"gpt-5.4\"}}\n\n",
                    "event: response.completed\n",
                    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_success_exhausted\",\"model\":\"gpt-5.4\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n"
                )),
        )
        .mount(&server)
        .await;

    let mut stream = provider_with_base_url(&store, server.uri())
        .execute(
            planned_request("openai", http_generate_operation()),
            context("req_success_exhausted", CancellationToken::new()),
        )
        .await
        .expect("prepare provider stream");
    let mut completed = false;
    while let Some(event) = stream.next().await {
        let event = event.expect("successful upstream response");
        completed |= event
            .canonical_facts()
            .iter()
            .any(|event| matches!(event, GatewayEvent::Completed(_)));
    }

    assert!(completed);
    assert_eq!(
        store
            .account(account_id)
            .expect("account after successful response")
            .availability(),
        AccountAvailability::Ready
    );
    assert!(store.has_quota(account_id));
}

#[tokio::test]
async fn successful_http_sse_rate_limit_event_persists_structured_exhaustion() {
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_http_sse_exhausted";
    create_account(&store, account_id).await;
    let reset_at = 1_900_000_000_u64;
    let rate_limit_event = json!({
        "type": "codex.rate_limits",
        "rate_limits": {
            "allowed": false,
            "limit_reached": true,
            "primary": {
                "used_percent": 42,
                "window_minutes": 300,
                "reset_at": reset_at,
            },
        },
    });
    let completed_event = json!({
        "type": "response.completed",
        "response": {
            "id": "resp_http_sse_exhausted",
            "model": "gpt-5.4",
            "status": "completed",
            "output": [],
            "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2},
        },
    });
    let body = format!(
        "event: codex.rate_limits\ndata: {rate_limit_event}\n\nevent: response.completed\ndata: {completed_event}\n\n"
    );
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let mut stream = provider_with_base_url(&store, server.uri())
        .execute(
            planned_request("openai", http_generate_operation()),
            context("req_http_sse_exhausted", CancellationToken::new()),
        )
        .await
        .expect("prepare provider stream");
    while let Some(event) = stream.next().await {
        event.expect("successful upstream response");
    }

    let account = store
        .account(account_id)
        .expect("account after HTTP SSE response");
    assert_eq!(
        (account.availability(), store.has_quota(account_id),),
        (AccountAvailability::Ready, true,)
    );
}

#[test]
fn request_observation_reads_openai_metadata_without_changing_the_operation() {
    let store = Arc::new(MemoryAccountStore::default());
    let payload = ProtocolPayload::json_object(
        "openai",
        Map::from_iter([
            ("model".to_owned(), json!("gpt-5.4")),
            ("input".to_owned(), json!("hello")),
        ]),
    )
    .expect("OpenAI payload")
    .with_context(Map::from_iter([(
        "turn_metadata".to_owned(),
        serde_json::Value::String(
            r#"{"request_kind":"review","subagent_kind":"worker"}"#.to_owned(),
        ),
    )]));
    let generation = GenerateRequest::from_protocol_payload(payload);
    let operation = Operation::Generate(generation);

    let observation = provider(&store).request_observation(&operation);

    assert_eq!(observation.request_kind.as_deref(), Some("review"));
    assert_eq!(observation.subagent_kind.as_deref(), Some("worker"));
    assert!(!observation.compact);
    assert!(matches!(operation, Operation::Generate(_)));
}

#[test]
fn request_observation_preserves_the_raw_reasoning_effort() {
    let store = Arc::new(MemoryAccountStore::default());
    let payload = ProtocolPayload::json_object(
        "openai",
        Map::from_iter([("reasoning".to_owned(), json!({"effort": "future-value"}))]),
    )
    .expect("protocol payload");
    let operation = Operation::Generate(GenerateRequest::from_protocol_payload(payload));

    let observation = provider(&store).request_observation(&operation);

    assert_eq!(
        observation.reasoning_effort.as_deref(),
        Some("future-value")
    );
}

#[test]
fn request_observation_ignores_future_session_state_without_rewriting_the_protocol_body() {
    let store = Arc::new(MemoryAccountStore::default());
    let generation = GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            Map::from_iter([
                ("model".to_owned(), json!("gpt-test")),
                ("previous_response_id".to_owned(), json!("resp_opaque")),
            ]),
        )
        .expect("OpenAI payload"),
    )
    .with_provider_session_state(
        ProviderSessionState::new(
            "openai",
            Map::from_iter([("future_session_shape".to_owned(), json!([1, 2, 3]))]),
        )
        .expect("provider session state"),
    );
    let operation = Operation::Generate(generation);

    let _observation = provider(&store).request_observation(&operation);

    let Operation::Generate(generation) = &operation else {
        panic!("operation should remain a generate request");
    };
    assert_eq!(
        generation
            .protocol_payload()
            .body()
            .get("previous_response_id"),
        Some(&json!("resp_opaque"))
    );
}

#[tokio::test]
async fn provider_compiles_catalog_presentation_for_codex_models() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_presentation").await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_raw(OFFICIAL_FIXTURE.to_vec(), "application/json"),
        )
        .mount(&server)
        .await;
    let provider = provider_with_base_url(&store, server.uri());

    let capabilities = provider
        .query_model_capabilities()
        .await
        .expect("capabilities");

    assert_eq!(capabilities.len(), 1);
    assert_eq!(capabilities[0].upstream_model().as_str(), "gpt-5.4");
    let presentation = capabilities[0]
        .presentation()
        .expect("Codex model presentation");
    assert_eq!(presentation.display_name(), Some("GPT-5.4"));
    assert_eq!(
        presentation.description(),
        Some("Frontier agentic coding model.")
    );
    assert_eq!(presentation.supported_reasoning_efforts(), ["low", "high"]);
    assert_eq!(presentation.default_reasoning_effort(), Some("low"));
    assert_eq!(presentation.context_window_tokens(), Some(272_000));
    assert!(presentation.image_input());
    assert!(presentation.agent_tools());
    assert!(presentation.parallel_tool_calls());
    assert!(presentation.search_tool());
    assert!(presentation.image_detail_original());
    assert!(presentation.verbosity());
    assert!(!presentation.hidden());
}
