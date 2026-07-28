use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::Utc;
use futures::StreamExt;
use gateway_core::engine::credential::{
    AccountAvailability, AccountFeedbackStats, ProviderAccountId,
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
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::support::{
    MemoryAccountStore, MemorySessionAffinity, MemorySessionExclusions, TestLeaseCoordinator,
    account_policy, agent_identity_service_with_pool, catalog_cache, profile, secret,
};

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

fn provider_with_affinity_and_base_url(
    store: &Arc<MemoryAccountStore>,
    session_affinity: Arc<MemorySessionAffinity>,
    base_url: String,
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
        Arc::new(TestLeaseCoordinator::default()),
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
    let body: serde_json::Value =
        serde_json::from_slice(&request.body).expect("captured JSON body");

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
    let body: serde_json::Value =
        serde_json::from_slice(&request.body).expect("captured JSON body");

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
    let body: serde_json::Value =
        serde_json::from_slice(&request.body).expect("captured JSON body");
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
async fn response_failed_before_semantic_output_is_atomic_and_persists_cooldown() {
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
    assert_eq!(account.availability(), AccountAvailability::Cooldown);
    assert!(
        account
            .cooldown_until()
            .is_some_and(|until| until > SystemTime::now())
    );
    let _ = release.send(());
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn ordinary_request_should_forward_created_before_later_failed_chunk() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_first_event_latency").await;
    let (base_url, release, _first_chunk_sent, server) = paused_chunked_sse_server(
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

    let first_event = loop {
        let next = timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("first chunk must not wait for the withheld failure")
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

    release.send(()).expect("release second upstream chunk");
    let mut later_wire_types = Vec::new();
    let failure = loop {
        let next = timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("later failure must arrive after release")
            .expect("provider stream must return typed failure");
        match next {
            Ok(event) => {
                if let Some(event_type) = event.wire_event().and_then(|wire| wire.event_type()) {
                    later_wire_types.push(event_type.to_owned());
                }
            }
            Err(error) => break error,
        }
    };

    assert_eq!(later_wire_types, vec!["response.failed"]);
    assert!(!failure.replay_is_safe());
    assert!(!failure.has_atomic_client_events());
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
    create_account_with_enabled(&store, account_id, false).await;
    let account = store.account(account_id).expect("disabled test account");
    store
        .repository()
        .apply_state(
            &account,
            AccountAvailability::QuotaExhausted,
            Some("quota_exhausted".to_owned()),
            None,
            SystemTime::now(),
        )
        .await
        .expect("seed quota-exhausted state");

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
        AccountAvailability::QuotaExhausted
    );
    assert!(store.has_quota(account_id));
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
