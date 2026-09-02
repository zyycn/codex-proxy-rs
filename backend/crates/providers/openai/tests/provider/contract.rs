use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use bytes::Bytes;
use chrono::Utc;
use futures::{SinkExt, StreamExt};
use gateway_core::account::{
    AccountFeedbackStats, CredentialState, OpaqueProviderData, ProviderAccountId,
    ProviderAccountStore as _, QuotaAccessChange, QuotaAccessState, QuotaEvidence,
    QuotaObservation, QuotaState,
};
use gateway_core::engine::continuation::{
    ContinuationBinding, NativeContinuationPin, PreviousResponseId,
};
use gateway_core::engine::provider::{Provider as _, ProviderRequest};
use gateway_core::engine::{
    AccountAttemptContext, AttemptContext, AttemptTransport, ContinuationAttempt, ModelRequestId,
    ProviderAccountStateOwner, RequestAttemptContext,
};
use gateway_core::error::{
    ContinuationFailure, ContinuationRecoveryDisposition, PreDeliveryRetry, ProviderErrorKind,
};
use gateway_core::event::GatewayEvent;
use gateway_core::lifecycle::CancellationToken;
use gateway_core::operation::{
    CapabilityRequirements, GenerateRequest, ImageRequest, ImageRequestKind, Operation,
    OperationKind, ProtocolPayload, ProviderSessionState, RawJsonPayload,
};
use gateway_core::policy::ClientApiKeyId;
use gateway_core::routing::{
    ClientRoutingScope, ConfigRevision, FrozenAccountScope, ModelCapabilities, ProviderKind,
    ProviderModel, PublicModelId, RoutingContext, RuntimeAccount, RuntimeAccountDirectory,
    RuntimeSnapshot, UpstreamModelId,
};
use gateway_core::upstream::UpstreamSendState;
use provider_openai::config::DEFAULT_STREAM_MAX_RETRIES;
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
use tracing_subscriber::fmt::MakeWriter;
use wiremock::matchers::{body_bytes, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::support::{
    MemoryAccountStore, MemoryCooldownPort, MemorySessionAffinity, MemorySessionExclusions,
    TestLeaseCoordinator, account_policy, catalog_cache, profile, secret,
};
use crate::transport::accept_codex_test_websocket;

const OFFICIAL_FIXTURE: &[u8] =
    include_bytes!("../transport/fixtures/official_models_snapshot.json");
const CAPTURE_COMPLETED_SSE: &str = concat!(
    "event: response.completed\n",
    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_scope_capture\",\"model\":\"gpt-5.4\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n"
);

#[derive(Clone, Default)]
struct CapturedLogs {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl CapturedLogs {
    fn json_events(&self) -> Vec<Value> {
        let bytes = self.bytes.lock().expect("captured logs lock").clone();
        String::from_utf8(bytes)
            .expect("captured logs are UTF-8")
            .lines()
            .map(|line| serde_json::from_str(line).expect("captured log is JSON"))
            .collect()
    }
}

impl<'writer> MakeWriter<'writer> for CapturedLogs {
    type Writer = Self;

    fn make_writer(&'writer self) -> Self::Writer {
        self.clone()
    }
}

impl Write for CapturedLogs {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes
            .lock()
            .map_err(|_| io::Error::other("captured logs lock poisoned"))?
            .extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn selected_account_log_fields<'events>(
    events: &'events [Value],
    request_id: &str,
) -> &'events Map<String, Value> {
    events
        .iter()
        .find_map(|event| {
            let fields = event.get("fields")?.as_object()?;
            (fields.get("message").and_then(Value::as_str) == Some("OpenAI account selected")
                && fields.get("request_id").and_then(Value::as_str) == Some(request_id))
            .then_some(fields)
        })
        .unwrap_or_else(|| {
            panic!("OpenAI account selection log for {request_id}; captured events: {events:#?}")
        })
}

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
    provider_with_base_url_and_retry_budget(
        store,
        base_url,
        u32::try_from(DEFAULT_STREAM_MAX_RETRIES).expect("default retry budget fits u32"),
    )
}

fn provider_with_base_url_and_retry_budget(
    store: &Arc<MemoryAccountStore>,
    base_url: String,
    stream_max_retries: u32,
) -> CodexProvider {
    provider_and_quota_with_affinity_and_base_url_and_leases(
        store,
        Arc::new(MemorySessionAffinity::default()),
        base_url,
        Arc::new(TestLeaseCoordinator::default()),
        stream_max_retries,
    )
    .0
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
    provider_and_quota_with_affinity_and_base_url_and_leases(
        store,
        session_affinity,
        base_url,
        leases,
        u32::try_from(DEFAULT_STREAM_MAX_RETRIES).expect("default retry budget fits u32"),
    )
    .0
}

fn provider_and_quota_with_affinity_and_base_url_and_leases(
    store: &Arc<MemoryAccountStore>,
    session_affinity: Arc<MemorySessionAffinity>,
    base_url: String,
    leases: Arc<TestLeaseCoordinator>,
    stream_max_retries: u32,
) -> (CodexProvider, Arc<CodexCredentialQuotaService>) {
    let profile = wire_profile();
    let http = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("client");
    let websocket_pool = Arc::new(CodexWebSocketPool::default());
    let catalog = Arc::new(CodexCredentialCatalogService::new(
        store.repository(),
        profile.clone(),
        http.clone(),
        base_url.clone(),
        catalog_cache(),
    ));
    let quota = Arc::new(CodexCredentialQuotaService::new(
        store.repository(),
        profile.clone(),
        http.clone(),
        base_url.clone(),
        Arc::new(MemoryCooldownPort::new()),
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
        Arc::clone(&account_feedback),
        CodexCookiePolicy::official().expect("cookie policy"),
    ));

    let provider = CodexProvider::new(
        selector,
        catalog,
        Arc::clone(&quota),
        account_feedback,
        http,
        profile,
        base_url,
        websocket_pool,
        stream_max_retries,
    )
    .expect("official OpenAI provider");
    (provider, quota)
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

fn generate_with_session_context(
    session_id: &str,
    thread_id: Option<&str>,
    turn_metadata: Option<&str>,
) -> GenerateRequest {
    let mut body = Map::from_iter([
        ("model".to_owned(), json!("gpt-5.4")),
        ("input".to_owned(), json!("hello")),
        ("session_id".to_owned(), json!(session_id)),
    ]);
    if let Some(thread_id) = thread_id {
        body.insert("thread_id".to_owned(), json!(thread_id));
    }
    if let Some(turn_metadata) = turn_metadata {
        body.insert("turnMetadata".to_owned(), json!(turn_metadata));
    }
    GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", body).expect("OpenAI payload"),
    )
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
    let account_scope = Arc::new(FrozenAccountScope::new(
        Arc::new(RuntimeAccountDirectory::new(BTreeMap::from([(
            ProviderAccountId::new("acct_provider_contract").expect("account"),
            RuntimeAccount::new(provider.clone(), BTreeSet::new()),
        )]))),
        ClientRoutingScope::all_accounts(),
    ));
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).expect("revision"),
        account_policy(),
        vec![provider.clone()],
        vec![ProviderModel::new(
            provider,
            upstream_model,
            ModelCapabilities::new(BTreeSet::from([operation.kind()]), Some(32_000))
                .with_upstream_feature_validation(),
        )],
        Vec::new(),
    )
    .expect("snapshot");
    let plan = snapshot
        .plan(
            &public_model,
            &operation,
            account_scope,
            &RoutingContext::default(),
        )
        .expect("routing plan");

    ProviderRequest::new(operation, plan.candidates()[0].clone())
}

fn planned_provider_endpoint_request(provider_name: &str, operation: Operation) -> ProviderRequest {
    let provider = ProviderKind::new(provider_name).expect("provider");
    let account_scope = Arc::new(FrozenAccountScope::new(
        Arc::new(RuntimeAccountDirectory::new(BTreeMap::from([(
            ProviderAccountId::new("acct_provider_contract").expect("account"),
            RuntimeAccount::new(provider.clone(), BTreeSet::new()),
        )]))),
        ClientRoutingScope::all_accounts(),
    ));
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).expect("revision"),
        account_policy(),
        vec![provider.clone()],
        Vec::new(),
        Vec::new(),
    )
    .expect("snapshot");
    let plan = snapshot
        .plan_provider_endpoint(
            &provider,
            &operation,
            account_scope,
            &RoutingContext::default(),
        )
        .expect("provider endpoint routing plan");

    ProviderRequest::new(operation, plan.candidates()[0].clone())
}

fn contract_account_scope() -> Arc<FrozenAccountScope> {
    let provider = ProviderKind::new("openai").expect("provider");
    let accounts = [
        "acct_affinity",
        "acct_affinity_switch_a",
        "acct_affinity_switch_b",
        "acct_atomic_failure",
        "acct_bare_atomic_failure",
        "acct_bounded_replay_grace",
        "acct_bounded_session_state",
        "acct_capacity_busy",
        "acct_client_history",
        "acct_completed_affinity",
        "acct_continuation_prefetch",
        "acct_disabled_scheduling",
        "acct_first_event_latency",
        "acct_header_new",
        "acct_header_old",
        "acct_header_same",
        "acct_http_sse_exhausted",
        "acct_local_affinity",
        "acct_metadata_new",
        "acct_metadata_old",
        "acct_unknown_continuation",
        "acct_unknown_turn_state",
        "acct_prefetch_limit",
        "acct_presentation",
        "acct_provider_contract",
        "acct_scope_new",
        "acct_scope_old",
        "acct_scope_same",
        "acct_semantic_failure",
        "acct_session_affinity",
        "acct_subagent_a",
        "acct_subagent_b",
        "acct_success_exhausted",
        "acct_thread_spawn_affinity",
        "acct_truncated_stream",
        "acct_usage_limit_request_path",
        "acct_websocket_close",
        "acct_websocket_busy_replay",
        "acct_websocket_turn_state",
    ]
    .into_iter()
    .map(|id| {
        (
            ProviderAccountId::new(id).expect("account"),
            RuntimeAccount::new(provider.clone(), BTreeSet::new()),
        )
    })
    .collect::<BTreeMap<_, _>>();
    Arc::new(FrozenAccountScope::new(
        Arc::new(RuntimeAccountDirectory::new(accounts)),
        ClientRoutingScope::all_accounts(),
    ))
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
        AccountAttemptContext::new(BTreeSet::<ProviderAccountId>::new(), None, None)
            .with_account_scope(contract_account_scope()),
        None,
        cancellation,
    )
}

fn fallback_transport_context(request_id: &str) -> AttemptContext {
    context(request_id, CancellationToken::new()).with_transport(AttemptTransport::Fallback)
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
        AccountAttemptContext::new(BTreeSet::new(), None, Some(owner))
            .with_account_scope(contract_account_scope()),
        None,
        CancellationToken::new(),
    )
}

fn replay_any_context(request_id: &str, owner_account_id: &str) -> AttemptContext {
    let owner_account = ProviderAccountId::new(owner_account_id).expect("owner account id");
    let provider = ProviderKind::new("openai").expect("provider");
    let owner = ProviderAccountStateOwner::new(provider.clone(), owner_account.clone());
    let continuation = NativeContinuationPin::new(
        PreviousResponseId::new("client-previous-response"),
        PreviousResponseId::new("upstream-previous-response"),
        ClientApiKeyId::new("key_openai_contract").expect("client key id"),
        provider,
        owner_account,
    );
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new(request_id).expect("request id"),
            ClientApiKeyId::new("key_openai_contract").expect("client key id"),
        ),
        NonZeroU32::new(2).expect("attempt"),
        SystemTime::now() + Duration::from_secs(30),
        account_policy(),
        AccountAttemptContext::new(BTreeSet::new(), None, Some(owner))
            .with_account_scope(contract_account_scope()),
        Some(ContinuationBinding::Pinned(continuation)),
        CancellationToken::new(),
    )
    .with_continuation_attempt(ContinuationAttempt::ReplayAny)
}

fn pinned_continuation_context(
    request_id: &str,
    account_id: &str,
    client_previous_response_id: &str,
    upstream_previous_response_id: &str,
    attempt_index: u32,
    continuation_attempt: ContinuationAttempt,
) -> AttemptContext {
    let account = ProviderAccountId::new(account_id).expect("account id");
    let provider = ProviderKind::new("openai").expect("provider");
    let owner = ProviderAccountStateOwner::new(provider.clone(), account.clone());
    let continuation = NativeContinuationPin::new(
        PreviousResponseId::new(client_previous_response_id),
        PreviousResponseId::new(upstream_previous_response_id),
        ClientApiKeyId::new("key_openai_contract").expect("client key id"),
        provider,
        account,
    );
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new(request_id).expect("request id"),
            ClientApiKeyId::new("key_openai_contract").expect("client key id"),
        ),
        NonZeroU32::new(attempt_index).expect("attempt index"),
        SystemTime::now() + Duration::from_secs(30),
        account_policy(),
        AccountAttemptContext::new(BTreeSet::new(), None, Some(owner))
            .with_account_scope(contract_account_scope()),
        Some(ContinuationBinding::Pinned(continuation)),
        CancellationToken::new(),
    )
    .with_continuation_attempt(continuation_attempt)
}

fn external_continuation_context(request_id: &str) -> AttemptContext {
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new(request_id).expect("request id"),
            ClientApiKeyId::new("key_openai_contract").expect("client key id"),
        ),
        NonZeroU32::new(1).expect("attempt"),
        SystemTime::now() + Duration::from_secs(30),
        account_policy(),
        AccountAttemptContext::new(BTreeSet::new(), None, None)
            .with_account_scope(contract_account_scope()),
        Some(ContinuationBinding::External(PreviousResponseId::new(
            "external-previous-response",
        ))),
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

async fn capture_turn_state_request(
    request_id: &str,
    previous_turn_id: Option<&str>,
    current_turn_id: Option<&str>,
    client_turn_state: Option<&str>,
) -> wiremock::Request {
    let account_id = "acct_session_affinity";
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, account_id).await;
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
    let mut session_state = Map::from_iter([
        ("account_id".to_owned(), json!(account_id)),
        ("conversation_id".to_owned(), json!("conversation")),
        ("turn_state".to_owned(), json!("previous-turn-state")),
        ("continuation_scope".to_owned(), json!("persisted")),
    ]);
    if let Some(previous_turn_id) = previous_turn_id {
        session_state.insert("client_turn_id".to_owned(), json!(previous_turn_id));
    }
    let mut protocol_context = Map::from_iter([("use_websocket".to_owned(), json!(false))]);
    if let Some(current_turn_id) = current_turn_id {
        protocol_context.insert("turn_id".to_owned(), json!(current_turn_id));
    }
    if let Some(client_turn_state) = client_turn_state {
        protocol_context.insert("turn_state".to_owned(), json!(client_turn_state));
    }
    let operation = Operation::Generate(
        GenerateRequest::from_protocol_payload(
            ProtocolPayload::json_object(
                "openai",
                Map::from_iter([
                    ("model".to_owned(), json!("gpt-5.4")),
                    ("input".to_owned(), json!("current input")),
                ]),
            )
            .expect("OpenAI payload")
            .with_context(protocol_context),
        )
        .with_provider_session_state(
            ProviderSessionState::new("openai", session_state).expect("provider session state"),
        ),
    );
    let mut stream = provider_with_base_url(&store, server.uri())
        .execute(
            planned_request("openai", operation),
            context(request_id, CancellationToken::new()),
        )
        .await
        .expect("prepare turn-state provider stream");
    while let Some(event) = stream.next().await {
        event.expect("turn-state provider response");
    }
    let mut requests = server
        .received_requests()
        .await
        .expect("captured turn-state request");
    assert_eq!(requests.len(), 1);
    requests.pop().expect("single turn-state request")
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

async fn capture_http_request(stream: &mut TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let read = stream.read(&mut buffer).await.expect("read HTTP request");
        if read == 0 {
            return request;
        }
        request.extend_from_slice(&buffer[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            return request;
        }
    }
}

async fn read_http_request(stream: &mut TcpStream) {
    drop(capture_http_request(stream).await);
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
async fn image_endpoints_bypass_only_the_text_catalog_and_preserve_the_current_codex_wire() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"models":[{"slug":"gpt-5.4","display_name":"GPT-5.4","supported_in_api":true}]}"#,
            "application/json",
        ))
        .expect(1)
        .mount(&server)
        .await;

    let cases = [
        (
            ImageRequestKind::Generation,
            "/codex/images/generations",
            br#"{ "model":"gpt-image-future", "prompt":"a lighthouse", "background":"transparent", "future_option":{"schema":2}, "future_integer":9007199254740993 }"#.as_slice(),
            br#"{ "created": 1787212800, "data": [{"b64_json":"AAEC"}], "future": 9007199254740993 }"#.as_slice(),
        ),
        (
            ImageRequestKind::Edit,
            "/codex/images/edits",
            br#"{"model":"gpt-image-2","images":[{"image_url":"data:image/png;base64,AAEC"}],"prompt":"add fog","prompt":"duplicate remains opaque"}"#.as_slice(),
            br#"{"created":1787212801,"data":[{"b64_json":"AwQF"}],"quality":"high"}"#.as_slice(),
        ),
    ];
    for (_, endpoint, body, response_body) in &cases {
        Mock::given(method("POST"))
            .and(path(*endpoint))
            .and(header("originator", "codex_cli_rs"))
            .and(header("x-codex-image-turn-id", "turn_image_contract"))
            .and(body_bytes(body.to_vec()))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .insert_header("x-request-id", "upstream_image_request")
                    .insert_header("x-future-image-header", "preserved")
                    .set_body_raw(response_body.to_vec(), "application/json"),
            )
            .expect(1)
            .mount(&server)
            .await;
    }

    let provider = provider_with_base_url(&store, server.uri());
    let catalog = provider
        .query_model_capabilities()
        .await
        .expect("text model catalog");
    assert!(
        catalog
            .iter()
            .all(|model| model.upstream_model().as_str() != "gpt-image-2"),
        "the image model must not be published as a text model"
    );

    for (index, (kind, _, body, expected_response)) in cases.iter().enumerate() {
        let payload = RawJsonPayload::new("openai", Bytes::copy_from_slice(body))
            .expect("image payload")
            .with_context(Map::from_iter([(
                "image_turn_id".to_owned(),
                json!("turn_image_contract"),
            )]));
        let operation = Operation::GenerateImage(ImageRequest::from_raw_json(*kind, payload));
        let request = planned_provider_endpoint_request("openai", operation);
        let mut stream = provider
            .execute(
                request,
                context(
                    &format!("req_image_contract_{index}"),
                    CancellationToken::new(),
                ),
            )
            .await
            .expect("prepare image provider stream");
        let mut raw_response = None;
        let mut completed = false;
        let mut observed_http_json = false;
        while let Some(event) = stream.next().await {
            let event = event.expect("image provider event");
            completed |= event
                .canonical_facts()
                .iter()
                .any(|event| matches!(event, GatewayEvent::Completed(_)));
            if let Some(raw) = event.wire_event().and_then(|wire| wire.raw_json_body()) {
                assert!(raw_response.replace(raw.clone()).is_none());
            }
            if let Some(observation) = event.response_observation() {
                observed_http_json |= observation.transport().as_str() == "http_json"
                    && observation.status_code() == Some(200)
                    && observation.client_headers().iter().any(|header| {
                        header.name() == "x-future-image-header"
                            && header.value().as_ref() == b"preserved"
                    });
            }
        }

        assert!(completed);
        assert!(observed_http_json);
        assert_eq!(raw_response.as_deref(), Some(*expected_response));
    }
    server.verify().await;
}

#[tokio::test]
async fn image_endpoint_returns_the_exact_upstream_error_response() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let server = MockServer::start().await;
    let request_body =
        br#"{ "model":"gpt-image-2", "images":[], "future_invalid":9007199254740993 }"#;
    let response_body = br#"{ "error":{"message":"future image validation","type":"image_error","code":"future_code"}, "future":9007199254740993 }"#;
    Mock::given(method("POST"))
        .and(path("/codex/images/edits"))
        .and(body_bytes(request_body.to_vec()))
        .respond_with(
            ResponseTemplate::new(422)
                .insert_header("content-type", "application/problem+json")
                .insert_header("x-future-image-error", "preserved")
                .set_body_bytes(response_body.to_vec()),
        )
        .expect(1)
        .mount(&server)
        .await;

    let payload =
        RawJsonPayload::new("openai", Bytes::from_static(request_body)).expect("image payload");
    let operation =
        Operation::GenerateImage(ImageRequest::from_raw_json(ImageRequestKind::Edit, payload));
    let mut stream = provider_with_base_url(&store, server.uri())
        .execute(
            planned_provider_endpoint_request("openai", operation),
            context("req_image_error", CancellationToken::new()),
        )
        .await
        .expect("prepare image provider stream");
    let error = loop {
        match stream.next().await {
            Some(Ok(_)) => {}
            Some(Err(error)) => break error,
            None => panic!("image rejection must surface an upstream response"),
        }
    };

    assert_eq!(error.upstream_status(), Some(422));
    let response = error
        .client_visible_upstream_response()
        .expect("raw upstream image error response");
    assert_eq!(response.status(), 422);
    assert_eq!(
        response.content_type(),
        Some(b"application/problem+json".as_slice())
    );
    assert_eq!(response.body().as_ref(), response_body);
    assert!(response.headers().iter().any(|header| {
        header.name() == "x-future-image-error" && header.value().as_ref() == b"preserved"
    }));
    server.verify().await;
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
async fn websocket_close_after_delivery_preserves_details_without_enabling_sticky_http() {
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

        let (stream, _) = listener
            .accept()
            .await
            .expect("accept next-turn WebSocket connection");
        let mut websocket = accept_codex_test_websocket(stream).await;
        let _request = websocket
            .next()
            .await
            .expect("next-turn WebSocket request")
            .expect("valid next-turn WebSocket request");
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_after_committed_close",
                        "model": "gpt-5.4",
                        "status": "completed",
                        "output": [],
                        "usage": {
                            "input_tokens": 1,
                            "output_tokens": 1,
                            "total_tokens": 2
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("complete next-turn WebSocket request");
    });

    let provider = provider_with_base_url(&store, base_url);
    let first_operation = Operation::Generate(generate_with_session_context(
        "committed-websocket-session",
        Some("thread-first"),
        None,
    ));
    let mut stream = provider
        .execute(
            planned_request("openai", first_operation),
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
    drop(stream);

    assert!(!format!("{error:?}").contains("message too big"));
    assert!(!error.to_string().contains("message too big"));
    assert_eq!(error.kind(), ProviderErrorKind::Transport);
    assert_eq!(error.send_state(), UpstreamSendState::Ambiguous);
    assert!(
        !error.allows_pre_delivery_retry(),
        "a close after the delivery boundary must not request a hidden replay"
    );
    let detail = error
        .client_visible_upstream_error()
        .expect("WebSocket close detail");
    assert_eq!(detail.message(), "message too big");
    assert_eq!(detail.code(), Some("1009"));
    assert_eq!(detail.error_type(), Some("websocket_close_error"));
    assert_eq!(
        error.upstream_code().map(|code| code.as_str()),
        Some("websocket_close_1009")
    );

    let second_operation = Operation::Generate(generate_with_session_context(
        "committed-websocket-session",
        Some("thread-second"),
        None,
    ));
    let mut next_stream = provider
        .execute(
            planned_request("openai", second_operation),
            context("req_after_committed_close", CancellationToken::new()),
        )
        .await
        .expect("post-delivery close must not enable sticky HTTP fallback");
    assert_eq!(next_stream.metadata().transport().as_str(), "websocket");
    while let Some(event) = next_stream.next().await {
        event.expect("next turn WebSocket response");
    }
    server.await.expect("WebSocket server");
}

#[tokio::test]
async fn ambiguous_websocket_close_only_sticks_new_chains_in_the_current_session() {
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
        let completed_response = |response_id: &str| {
            Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": response_id,
                        "model": "gpt-5.4",
                        "status": "completed",
                        "output": [],
                        "usage": {
                            "input_tokens": 1,
                            "output_tokens": 1,
                            "total_tokens": 2
                        }
                    }
                })
                .to_string()
                .into(),
            )
        };
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
        for event in [
            json!({
                "type": "response.created",
                "response": {"id": "resp_structural", "model": "gpt-5.4"}
            }),
            json!({
                "type": "response.in_progress",
                "response": {"id": "resp_structural", "model": "gpt-5.4"}
            }),
        ] {
            websocket
                .send(Message::Text(event.to_string().into()))
                .await
                .expect("send structural WebSocket event");
        }
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.output_item.added",
                    "output_index": 0,
                    "item": {
                        "id": "msg_structural",
                        "type": "message",
                        "role": "assistant",
                        "status": "in_progress",
                        "content": []
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("send structural output event");
        websocket
            .close(Some(CloseFrame {
                code: CloseCode::Normal,
                reason: "".into(),
            }))
            .await
            .expect("close WebSocket before terminal event");

        let (mut http, _) = listener.accept().await.expect("accept sticky HTTP request");
        let request = capture_http_request(&mut http).await;
        let request_head = String::from_utf8_lossy(&request).to_ascii_lowercase();
        assert!(!request_head.contains("upgrade: websocket"));
        http.write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{CAPTURE_COMPLETED_SSE}",
                CAPTURE_COMPLETED_SSE.len()
            )
            .as_bytes(),
        )
        .await
        .expect("write sticky HTTP response");

        let (stream, _) = listener
            .accept()
            .await
            .expect("accept same-session continuation WebSocket connection");
        let mut websocket = accept_codex_test_websocket(stream).await;
        let request = websocket
            .next()
            .await
            .expect("same-session continuation request")
            .expect("valid same-session continuation request");
        let payload: Value = serde_json::from_str(
            request
                .to_text()
                .expect("same-session continuation request text"),
        )
        .expect("same-session continuation request JSON");
        assert_eq!(
            payload.get("previous_response_id"),
            Some(&json!("external-previous-response"))
        );
        websocket
            .send(completed_response("resp_external_continuation"))
            .await
            .expect("complete same-session continuation request");

        let (stream, _) = listener
            .accept()
            .await
            .expect("accept other-session WebSocket connection");
        let mut websocket = accept_codex_test_websocket(stream).await;
        let _request = websocket
            .next()
            .await
            .expect("other-session WebSocket request")
            .expect("valid other-session WebSocket request");
        websocket
            .send(completed_response("resp_other_session"))
            .await
            .expect("complete other-session WebSocket request");
    });

    let provider = provider_with_base_url_and_retry_budget(&store, base_url, 2);
    let mut saw_websocket_observation = false;
    let operation = Operation::Generate(generate_with_session_context(
        "sticky-websocket-session",
        Some("thread-first"),
        None,
    ));
    let mut stream = provider
        .execute(
            planned_request("openai", operation),
            context("req_websocket_normal_close", CancellationToken::new()),
        )
        .await
        .expect("prepare WebSocket provider stream");
    let error = loop {
        match stream.next().await {
            Some(Ok(event)) => {
                if let Some(observation) = event.response_observation() {
                    saw_websocket_observation = true;
                    assert_eq!(observation.status_code(), None);
                    if let Some(provider_metadata) = observation.provider_metadata() {
                        let metadata: Value = serde_json::from_str(provider_metadata.as_json())
                            .expect("OpenAI provider metadata JSON");
                        assert_eq!(
                            metadata.get("upstreamStatus").and_then(Value::as_u64),
                            Some(101),
                            "successful upgrade remains provider-owned diagnostics"
                        );
                    }
                }
                assert!(
                    !event.has_client_event(),
                    "structural events must remain behind the replay boundary"
                );
            }
            Some(Err(error)) => break error,
            None => panic!("WebSocket close must surface a provider error"),
        }
    };
    drop(stream);

    assert!(saw_websocket_observation);
    assert_eq!(error.send_state(), UpstreamSendState::Ambiguous);
    assert_eq!(error.pre_delivery_retry(), None);
    assert_eq!(
        error.upstream_code().map(|code| code.as_str()),
        Some("websocket_close_1000")
    );
    assert_eq!(
        error.diagnostic().map(|diagnostic| diagnostic.as_str()),
        Some(
            "OpenAI WebSocket closed before a terminal response (close code 1000); last event type: response.output_item.added"
        )
    );
    let raw_close: Value = serde_json::from_str(
        error
            .raw_upstream_error()
            .expect("raw WebSocket close")
            .as_str(),
    )
    .expect("raw WebSocket close JSON");
    assert_eq!(raw_close.get("type"), Some(&json!("websocket.close")));
    assert_eq!(raw_close.get("code"), Some(&json!(1000)));
    assert_eq!(raw_close.get("reason"), Some(&json!("")));
    assert_eq!(
        raw_close.get("last_event_type"),
        Some(&json!("response.output_item.added"))
    );

    let second_operation = Operation::Generate(generate_with_session_context(
        "sticky-websocket-session",
        Some("thread-second"),
        None,
    ));
    let mut sticky_stream = provider
        .execute(
            planned_request("openai", second_operation),
            context("req_websocket_sticky_http", CancellationToken::new()),
        )
        .await
        .expect("same session should prepare an HTTP stream");
    assert_eq!(sticky_stream.metadata().transport().as_str(), "http_sse");
    while let Some(event) = sticky_stream.next().await {
        event.expect("sticky HTTP response");
    }
    drop(sticky_stream);

    let continuation_operation = Operation::Generate(generate_with_session_context(
        "sticky-websocket-session",
        Some("thread-continuation"),
        None,
    ));
    let mut continuation_stream = provider
        .execute(
            planned_request("openai", continuation_operation),
            external_continuation_context("req_sticky_external_continuation"),
        )
        .await
        .expect("sticky state must not force a continuation onto HTTP");
    assert_eq!(
        continuation_stream.metadata().transport().as_str(),
        "websocket"
    );
    while let Some(event) = continuation_stream.next().await {
        event.expect("same-session continuation WebSocket response");
    }
    drop(continuation_stream);

    let other_operation = Operation::Generate(generate_with_session_context(
        "other-websocket-session",
        Some("thread-first"),
        None,
    ));
    let mut other_stream = provider
        .execute(
            planned_request("openai", other_operation),
            context("req_other_session_websocket", CancellationToken::new()),
        )
        .await
        .expect("another session should still prepare a WebSocket stream");
    assert_eq!(other_stream.metadata().transport().as_str(), "websocket");
    while let Some(event) = other_stream.next().await {
        event.expect("other-session WebSocket response");
    }
    drop(other_stream);

    let warmup_operation = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            Map::from_iter([
                ("model".to_owned(), json!("gpt-5.4")),
                ("input".to_owned(), json!("warm up")),
                ("session_id".to_owned(), json!("sticky-websocket-session")),
                ("generate".to_owned(), json!(false)),
                ("store".to_owned(), json!(false)),
            ]),
        )
        .expect("warmup payload"),
    ));
    let warmup_stream = provider
        .execute(
            planned_request("openai", warmup_operation),
            context(
                "req_sticky_session_required_warmup",
                CancellationToken::new(),
            ),
        )
        .await
        .expect("required warmup must bypass sticky HTTP fallback");
    assert_eq!(warmup_stream.metadata().transport().as_str(), "websocket");
    drop(warmup_stream);
    server.await.expect("WebSocket and HTTP server");
}

#[tokio::test]
async fn websocket_upgrade_required_immediately_enables_session_http_fallback() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_websocket_close").await;
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind upstream listener");
    let base_url = format!(
        "http://{}",
        listener.local_addr().expect("listener address")
    );
    let server = tokio::spawn(async move {
        let (mut opening, _) = listener.accept().await.expect("accept WS opening");
        let request = capture_http_request(&mut opening).await;
        assert!(String::from_utf8_lossy(&request).starts_with("GET /codex/responses"));
        let body = r#"{"error":{"code":"upgrade_required","message":"use HTTP"}}"#;
        opening
            .write_all(
                format!(
                    "HTTP/1.1 426 Upgrade Required\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .expect("write 426 response");

        let (mut http, _) = listener.accept().await.expect("accept sticky HTTP request");
        let request = capture_http_request(&mut http).await;
        assert!(String::from_utf8_lossy(&request).starts_with("POST /codex/responses"));
        http.write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{CAPTURE_COMPLETED_SSE}",
                CAPTURE_COMPLETED_SSE.len()
            )
            .as_bytes(),
        )
        .await
        .expect("write HTTP fallback response");
    });

    let provider = provider_with_base_url(&store, base_url);
    let operation = || {
        Operation::Generate(generate_with_session_context(
            "sticky-websocket-session",
            Some("thread-first"),
            None,
        ))
    };
    let mut first = provider
        .execute(
            planned_request("openai", operation()),
            context("req_upgrade_required", CancellationToken::new()),
        )
        .await
        .expect("prepare WebSocket stream");
    let error = loop {
        match first.next().await {
            Some(Ok(_)) => {}
            Some(Err(error)) => break error,
            None => panic!("426 opening must surface a fallback signal"),
        }
    };
    assert_eq!(error.upstream_status(), Some(426));
    assert_eq!(
        error.pre_delivery_retry(),
        Some(PreDeliveryRetry::SameAccountTransportFallback)
    );
    drop(first);

    let mut second = provider
        .execute(
            planned_request("openai", operation()),
            context("req_upgrade_required_next_turn", CancellationToken::new()),
        )
        .await
        .expect("same session should select HTTP");
    assert_eq!(second.metadata().transport().as_str(), "http_sse");
    while let Some(event) = second.next().await {
        event.expect("HTTP fallback response");
    }
    server.await.expect("upstream server");
}

#[tokio::test]
async fn fallback_attempt_transport_forces_http_sse_for_a_websocket_request() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_http_sse_exhausted").await;
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
            planned_request("openai", generate_operation()),
            fallback_transport_context("req_fallback_transport_http"),
        )
        .await
        .expect("prepare fallback HTTP stream");
    while let Some(event) = stream.next().await {
        event.expect("fallback HTTP response");
    }
}

#[tokio::test]
async fn websocket_turn_state_metadata_is_exposed_through_response_observation() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_websocket_turn_state").await;
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
                    "type": "response.metadata",
                    "headers": {
                        "x-codex-turn-state": ["turn-state-from-websocket"]
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("send response metadata");
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_websocket_turn_state",
                        "model": "gpt-5.4",
                        "status": "completed",
                        "output": [],
                        "usage": {
                            "input_tokens": 1,
                            "output_tokens": 1,
                            "total_tokens": 2
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("send completed response");
        websocket.close(None).await.expect("close WebSocket");
    });

    let mut stream = provider_with_base_url(&store, base_url)
        .execute(
            planned_request("openai", generate_operation()),
            context("req_websocket_turn_state", CancellationToken::new()),
        )
        .await
        .expect("prepare WebSocket provider stream");
    let mut observed_turn_state = false;
    while let Some(event) = stream.next().await {
        let event = event.expect("provider event");
        if let Some(observation) = event.response_observation() {
            observed_turn_state |= observation.client_headers().iter().any(|header| {
                header.name().eq_ignore_ascii_case("x-codex-turn-state")
                    && header.value().as_ref() == b"turn-state-from-websocket"
            });
        }
        if event
            .canonical_facts()
            .iter()
            .any(|event| matches!(event, GatewayEvent::Completed(_)))
        {
            break;
        }
    }
    server.await.expect("WebSocket server");

    assert!(observed_turn_state);
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
async fn cross_account_full_replay_should_preserve_the_complete_transcript() {
    let transcript = json!([
        {
            "type": "reasoning",
            "id": "reasoning-item-id",
            "encrypted_content": "reasoning-ciphertext",
            "summary": []
        },
        {
            "type": "compaction",
            "id": "compaction-item-id",
            "encrypted_content": "compaction-ciphertext"
        },
        {
            "type": "agent_message",
            "id": "agent-message-id",
            "content": [
                {"type": "output_text", "text": "visible"},
                {"type": "encrypted_content", "encrypted_content": "nested-ciphertext"}
            ]
        },
        {
            "type": "function_call",
            "id": "function-call-id",
            "call_id": "call-id",
            "name": "tool",
            "arguments": "{}",
            "encrypted_function_args": "function-args-ciphertext"
        }
    ]);
    let request = capture_scoped_http_request(
        "req_scope_transcript",
        "acct_scope_new",
        "acct_scope_old",
        Map::from_iter([
            ("model".to_owned(), json!("gpt-5.4")),
            ("input".to_owned(), transcript.clone()),
        ]),
        Map::new(),
    )
    .await;
    let body: Value = captured_request_body(&request);

    assert_eq!(body.get("input"), Some(&transcript));
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
async fn cross_account_continuation_should_require_client_replay_without_an_upstream_probe() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_scope_new").await;
    let server = MockServer::start().await;
    let generation = GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            Map::from_iter([
                ("model".to_owned(), json!("gpt-5.4")),
                (
                    "input".to_owned(),
                    json!([{"role": "user", "content": "delta"}]),
                ),
                (
                    "previous_response_id".to_owned(),
                    json!("client-previous-response"),
                ),
                ("authorization".to_owned(), json!("client-extension-secret")),
                (
                    "turnMetadata".to_owned(),
                    json!(r#"{"account_id":"acct_scope_old","safe":true}"#),
                ),
                (
                    "client_metadata".to_owned(),
                    json!({
                        "x-codex-turn-state": "old-account-turn-state",
                        "x-codex-turn-metadata": "old-account-turn-metadata",
                        "future": "keep"
                    }),
                ),
            ]),
        )
        .expect("OpenAI payload")
        .with_context(Map::from_iter([
            ("use_websocket".to_owned(), json!(false)),
            ("turn_id".to_owned(), json!("turn-probe")),
            ("turn_state".to_owned(), json!("old-account-turn-state")),
        ])),
    )
    .with_provider_session_state(
        ProviderSessionState::new(
            "openai",
            Map::from_iter([
                ("account_id".to_owned(), json!("acct_scope_old")),
                ("conversation_id".to_owned(), json!("conversation")),
                ("turn_state".to_owned(), json!("old-account-turn-state")),
                ("client_turn_id".to_owned(), json!("turn-probe")),
                ("continuation_scope".to_owned(), json!("persisted")),
            ]),
        )
        .expect("provider session state"),
    );
    let result = provider_with_base_url(&store, server.uri())
        .execute(
            planned_request("openai", Operation::Generate(generation)),
            replay_any_context("req_cross_account_probe", "acct_scope_old"),
        )
        .await;
    let Err(error) = result else {
        panic!("cross-account continuation must require a full client replay");
    };
    let detail = error
        .client_visible_upstream_error()
        .expect("client replay error detail");
    let requests = server
        .received_requests()
        .await
        .expect("captured upstream requests");

    assert_eq!(
        (
            error.kind(),
            error.send_state(),
            error.continuation_failure(),
            detail.code(),
            detail.error_type(),
        ),
        (
            ProviderErrorKind::ContinuationRecoveryRequired,
            UpstreamSendState::NotSent,
            Some(ContinuationFailure::HistoryUnavailable),
            Some("previous_response_not_found"),
            Some("invalid_request_error"),
        )
    );
    assert!(requests.is_empty());
}

#[tokio::test]
async fn missing_affinity_continuation_should_require_client_replay_without_an_upstream_probe() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_unknown_continuation").await;
    let server = MockServer::start().await;
    let generation = GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            Map::from_iter([
                ("model".to_owned(), json!("gpt-5.4")),
                (
                    "input".to_owned(),
                    json!([{"role": "user", "content": "delta"}]),
                ),
                (
                    "previous_response_id".to_owned(),
                    json!("external-previous-response"),
                ),
            ]),
        )
        .expect("OpenAI payload")
        .with_context(Map::from_iter([
            ("use_websocket".to_owned(), json!(false)),
            ("session_id".to_owned(), json!("expired-affinity-session")),
        ])),
    );
    let result = provider_with_base_url(&store, server.uri())
        .execute(
            planned_request("openai", Operation::Generate(generation)),
            external_continuation_context("req_unknown_continuation"),
        )
        .await;
    let Err(error) = result else {
        panic!("unowned continuation must require a full client replay");
    };
    let requests = server
        .received_requests()
        .await
        .expect("captured upstream requests");

    assert_eq!(
        error.kind(),
        ProviderErrorKind::ContinuationRecoveryRequired
    );
    assert_eq!(
        error
            .client_visible_upstream_error()
            .and_then(|detail| detail.code()),
        Some("previous_response_not_found")
    );
    assert!(requests.is_empty());
}

#[tokio::test]
async fn missing_affinity_full_request_should_clear_unowned_turn_state() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_unknown_turn_state").await;
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
    let generation = GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            Map::from_iter([
                ("model".to_owned(), json!("gpt-5.4")),
                ("input".to_owned(), json!("full transcript")),
            ]),
        )
        .expect("OpenAI payload")
        .with_context(Map::from_iter([
            ("use_websocket".to_owned(), json!(false)),
            ("session_id".to_owned(), json!("expired-turn-state-session")),
            ("turn_id".to_owned(), json!("turn-after-affinity-expiry")),
            ("turn_state".to_owned(), json!("unowned-turn-state")),
        ])),
    );
    let mut stream = provider_with_base_url(&store, server.uri())
        .execute(
            planned_request("openai", Operation::Generate(generation)),
            context("req_unknown_turn_state", CancellationToken::new()),
        )
        .await
        .expect("prepare full replay stream");
    while let Some(event) = stream.next().await {
        event.expect("full replay response");
    }
    let requests = server
        .received_requests()
        .await
        .expect("captured full replay request");

    assert!(captured_header_values(&requests[0], "x-codex-turn-state").is_empty());
}

#[tokio::test]
async fn matching_turn_id_should_restore_previous_turn_state() {
    let request = capture_turn_state_request(
        "req_restore_same_turn_state",
        Some("turn-same"),
        Some("turn-same"),
        None,
    )
    .await;

    assert_eq!(
        captured_header_values(&request, "x-codex-turn-state"),
        vec![b"previous-turn-state".to_vec()]
    );
}

#[tokio::test]
async fn matching_turn_id_should_prefer_the_latest_provider_state_over_a_stale_client_echo() {
    let request = capture_turn_state_request(
        "req_replace_stale_client_turn_state",
        Some("turn-same"),
        Some("turn-same"),
        Some("stale-client-turn-state"),
    )
    .await;

    assert_eq!(
        captured_header_values(&request, "x-codex-turn-state"),
        vec![b"previous-turn-state".to_vec()]
    );
}

#[tokio::test]
async fn new_or_unidentified_turn_should_not_restore_previous_turn_state() {
    for (request_id, previous_turn_id, current_turn_id) in [
        ("req_new_turn_state", Some("turn-old"), Some("turn-new")),
        ("req_unidentified_turn_state", None, Some("turn-new")),
        ("req_missing_current_turn_state", Some("turn-old"), None),
    ] {
        let request =
            capture_turn_state_request(request_id, previous_turn_id, current_turn_id, None).await;
        assert!(captured_header_values(&request, "x-codex-turn-state").is_empty());
    }
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

#[tokio::test(flavor = "current_thread")]
async fn account_selection_log_should_include_affinity_observation_fields() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_affinity").await;
    let provider = provider_with_affinity(&store, Arc::new(MemorySessionAffinity::default()));
    let captured = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .without_time()
        .with_ansi(false)
        .with_target(false)
        .with_writer(captured.clone())
        .finish();
    // 该 integration test binary 没有其他 subscriber；全局安装可避免并行测试切换
    // thread-local dispatcher 时重建 tracing callsite interest 所产生的竞争。
    tracing::subscriber::set_global_default(subscriber)
        .expect("install affinity observation log subscriber");

    for (request_id, prompt_cache_key, session_id) in [
        (
            "req_affinity_observation_session_first",
            "turn-cache-first",
            Some("stable-observation-session"),
        ),
        (
            "req_affinity_observation_session_second",
            "turn-cache-second",
            Some("stable-observation-session"),
        ),
        (
            "req_affinity_observation_conversation",
            "turn-cache-with-conversation",
            None,
        ),
    ] {
        let mut payload = ProtocolPayload::json_object(
            "openai",
            Map::from_iter([
                ("model".to_owned(), json!("gpt-5.4")),
                ("input".to_owned(), json!("hello")),
                ("prompt_cache_key".to_owned(), json!(prompt_cache_key)),
            ]),
        )
        .expect("OpenAI payload");
        if let Some(session_id) = session_id {
            payload = payload.with_context(Map::from_iter([(
                "session_id".to_owned(),
                json!(session_id),
            )]));
        } else {
            payload = payload.with_context(Map::from_iter([(
                "conversation_id".to_owned(),
                json!("root-observation-conversation"),
            )]));
        }
        let generation = GenerateRequest::from_protocol_payload(payload);
        let stream = provider
            .execute(
                planned_request("openai", Operation::Generate(generation)),
                context(request_id, CancellationToken::new()),
            )
            .await
            .expect("prepare affinity observation request");
        drop(stream);
    }

    let events = captured.json_events();
    let first = selected_account_log_fields(&events, "req_affinity_observation_session_first");
    let second = selected_account_log_fields(&events, "req_affinity_observation_session_second");
    for fields in [first, second] {
        assert_eq!(fields["affinity_anchor_source"], "root-session");
        assert_eq!(fields["affinity_anchor"], "stable-observation-session");
        assert_eq!(fields["session_id"], "stable-observation-session");
        assert_eq!(fields["session_id_present"], true);
        let key_hash = fields["affinity_key_hash"]
            .as_str()
            .expect("affinity key hash");
        assert_eq!(key_hash.len(), 12);
        assert!(key_hash.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }
    assert_eq!(
        first["affinity_key_hash"], second["affinity_key_hash"],
        "the same session must emit the same affinity hash"
    );

    let conversation =
        selected_account_log_fields(&events, "req_affinity_observation_conversation");
    assert_eq!(conversation["affinity_anchor_source"], "root-conversation");
    assert_eq!(
        conversation["affinity_anchor"],
        "root-observation-conversation"
    );
    assert_eq!(conversation["session_id"], "");
    assert_eq!(conversation["session_id_present"], false);
    assert_ne!(
        first["affinity_key_hash"], conversation["affinity_key_hash"],
        "different anchors must not share the same affinity hash"
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
async fn subagent_requests_should_share_the_root_session_account_affinity_key() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_subagent_a").await;
    create_account(&store, "acct_subagent_b").await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CAPTURE_COMPLETED_SSE),
        )
        .mount(&server)
        .await;
    let provider = provider_with_affinity_and_base_url(&store, Arc::clone(&affinity), server.uri());

    for (request_id, subagent_kind) in [
        ("req_root_affinity", None),
        ("req_subagent_affinity_first", Some("review")),
        ("req_subagent_affinity_second", Some("review")),
    ] {
        let mut body = Map::from_iter([
            ("model".to_owned(), json!("gpt-5.4")),
            ("input".to_owned(), json!("new task")),
            ("prompt_cache_key".to_owned(), json!("root-session-key")),
        ]);
        if let Some(subagent_kind) = subagent_kind {
            body.insert(
                "client_metadata".to_owned(),
                json!({"x-openai-subagent": subagent_kind}),
            );
        }
        let generation = GenerateRequest::from_protocol_payload(
            ProtocolPayload::json_object("openai", body)
                .expect("OpenAI payload")
                .with_context(Map::from_iter([("use_websocket".to_owned(), json!(false))])),
        );
        let mut stream = provider
            .execute(
                planned_request("openai", Operation::Generate(generation)),
                context(request_id, CancellationToken::new()),
            )
            .await
            .expect("prepare subagent provider stream");
        while let Some(event) = stream.next().await {
            event.expect("subagent response");
        }
        drop(stream);
    }

    let keys = affinity.lookup_keys();
    assert_eq!(keys.len(), 3);
    assert!(
        keys.iter().all(|key| key == &keys[0]),
        "root and derived subagent requests must prefer the same account"
    );
    let requests = server
        .received_requests()
        .await
        .expect("captured root and subagent requests");
    let selected_accounts = requests
        .iter()
        .filter_map(|request| request.headers.get("chatgpt-account-id"))
        .filter_map(|value| value.to_str().ok())
        .collect::<Vec<_>>();
    assert_eq!(selected_accounts.len(), 3);
    assert!(
        selected_accounts
            .iter()
            .all(|account| account == &selected_accounts[0]),
        "root and subagents should route to the same preferred account"
    );
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
async fn affinity_quota_switch_should_clear_old_turn_state_without_a_provider_state_owner() {
    let store = Arc::new(MemoryAccountStore::default());
    let first_account_id = "acct_affinity_switch_a";
    let second_account_id = "acct_affinity_switch_b";
    create_account(&store, first_account_id).await;
    let affinity = Arc::new(MemorySessionAffinity::default());
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
    let (provider, quota) = provider_and_quota_with_affinity_and_base_url_and_leases(
        &store,
        Arc::clone(&affinity),
        server.uri(),
        Arc::new(TestLeaseCoordinator::default()),
        u32::try_from(DEFAULT_STREAM_MAX_RETRIES).expect("default retry budget fits u32"),
    );
    let session_id = "stable-affinity-switch-session";
    let first_payload = ProtocolPayload::json_object(
        "openai",
        Map::from_iter([
            ("model".to_owned(), json!("gpt-5.4")),
            ("input".to_owned(), json!("first request")),
            ("session_id".to_owned(), json!(session_id)),
        ]),
    )
    .expect("OpenAI payload")
    .with_context(Map::from_iter([("use_websocket".to_owned(), json!(false))]));
    let first = provider
        .execute(
            planned_request(
                "openai",
                Operation::Generate(GenerateRequest::from_protocol_payload(first_payload)),
            ),
            context("req_affinity_switch_first", CancellationToken::new()),
        )
        .await
        .expect("prepare first affinity request");
    drop(first);
    let affinity_keys = affinity.lookup_keys();
    assert_eq!(affinity_keys.len(), 1);
    affinity.seed_binding(
        &ProviderKind::new("openai").expect("provider"),
        &affinity_keys[0],
        ProviderAccountId::new(first_account_id).expect("first account id"),
    );
    assert_eq!(affinity.binding_count(), 1);

    create_account(&store, second_account_id).await;
    let first_account = store.account(first_account_id).expect("first account");
    let observed_at = SystemTime::now();
    store
        .compare_and_swap_quota(QuotaObservation {
            account_id: first_account.id().clone(),
            expected_revision: first_account.revision(),
            quota: OpaqueProviderData::new(Map::new()),
            observed_at,
            state: QuotaState::exhausted(QuotaEvidence::ProviderDenied, observed_at, None),
        })
        .await
        .expect("seed exhausted affinity account");
    let accounts = [
        store
            .account(first_account_id)
            .expect("first account after quota"),
        store.account(second_account_id).expect("second account"),
    ];
    quota.prepare_scheduling(&accounts).await;

    let second_payload = ProtocolPayload::json_object(
        "openai",
        Map::from_iter([
            ("model".to_owned(), json!("gpt-5.4")),
            ("input".to_owned(), json!("second request")),
            ("session_id".to_owned(), json!(session_id)),
        ]),
    )
    .expect("OpenAI payload")
    .with_context(Map::from_iter([
        ("use_websocket".to_owned(), json!(false)),
        ("turn_id".to_owned(), json!("turn-affinity-switch")),
        (
            "turn_state".to_owned(),
            json!("state-created-by-exhausted-account"),
        ),
    ]));
    let mut second = provider
        .execute(
            planned_request(
                "openai",
                Operation::Generate(GenerateRequest::from_protocol_payload(second_payload)),
            ),
            context("req_affinity_switch_second", CancellationToken::new()),
        )
        .await
        .expect("prepare switched affinity request");
    while let Some(event) = second.next().await {
        event.expect("switched affinity response");
    }

    let requests = server
        .received_requests()
        .await
        .expect("captured affinity requests");
    assert_eq!(requests.len(), 1);
    let second_request = &requests[0];
    assert_eq!(
        captured_request_body(second_request).get("input"),
        Some(&json!("second request"))
    );
    assert!(captured_header_values(second_request, "x-codex-turn-state").is_empty());
    let expected_account_id = format!("chatgpt-{second_account_id}");
    assert_eq!(
        second_request
            .headers
            .get("chatgpt-account-id")
            .and_then(|value| value.to_str().ok()),
        Some(expected_account_id.as_str())
    );
}

#[tokio::test]
async fn thread_spawn_children_should_share_the_parent_account_affinity_key() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_thread_spawn_affinity").await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let provider = provider_with_affinity(&store, Arc::clone(&affinity));
    let thread_spawn = r#"{"subagent_kind":"thread_spawn"}"#;

    for (request_id, thread_id, turn_metadata) in [
        (
            "req_thread_spawn_first",
            Some("child-one"),
            Some(thread_spawn),
        ),
        (
            "req_thread_spawn_second",
            Some("child-two"),
            Some(thread_spawn),
        ),
        (
            "req_thread_spawn_repeat",
            Some("child-one"),
            Some(thread_spawn),
        ),
        ("req_thread_spawn_fallback", None, Some(thread_spawn)),
        ("req_thread_spawn_parent", None, None),
    ] {
        let stream = provider
            .execute(
                planned_request(
                    "openai",
                    Operation::Generate(generate_with_session_context(
                        "parent-session",
                        thread_id,
                        turn_metadata,
                    )),
                ),
                context(request_id, CancellationToken::new()),
            )
            .await
            .expect("prepare provider stream");
        drop(stream);
    }

    let keys = affinity.lookup_keys();
    assert_eq!(keys.len(), 5);
    assert!(
        keys.iter().all(|key| key == &keys[0]),
        "child thread transport identities must not split parent account affinity"
    );
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
    assert_eq!(
        failure
            .raw_upstream_error()
            .expect("raw response.failed data")
            .as_str(),
        r#"{"type":"response.failed","status_code":429,"retry_after_seconds":17,"response":{"id":"resp_atomic_failure","status":"failed","error":{"code":"rate_limit_exceeded","message":"atomic upstream failure"}}}"#
    );
    let events = failure.take_atomic_client_events();
    assert_eq!(
        events
            .iter()
            .filter_map(|event| event.wire_event()?.event_type())
            .collect::<Vec<_>>(),
        vec!["response.created", "response.failed"]
    );
    let account = store.account(account_id).expect("rate-limited account");
    assert_eq!(account.credential_state(), CredentialState::Ready);
    assert_eq!(account.quota().access(), QuotaAccessState::Unknown);
    let _ = release.send(());
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn official_usage_limit_failure_persists_fact_without_fabricating_usage() {
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_usage_limit_request_path";
    let reset_at = 1_900_000_000;
    create_account(&store, account_id).await;
    let account = store.account(account_id).expect("created account");
    let observed_at = SystemTime::now();
    store
        .compare_and_swap_quota(QuotaObservation {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            quota: OpaqueProviderData::new(
                json!({
                    "rate_limit": {
                        "allowed": true,
                        "limit_reached": false,
                        "primary_window": {"used_percent": 99, "reset_at": reset_at},
                        "secondary_window": {"used_percent": 0}
                    }
                })
                .as_object()
                .expect("stale quota object")
                .clone(),
            ),
            observed_at,
            state: QuotaState::allowed(observed_at),
        })
        .await
        .expect("seed stale passive quota");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header("authorization", format!("Bearer at-{account_id}")))
        // 生产环境会短暂返回互相矛盾的快照：98/99% 且布尔位仍声称可用。
        // 后台同步不能覆盖真实请求已确认的额度耗尽。
        .respond_with(
            ResponseTemplate::new(200)
                // 验证后台 usage 同步不能把原始的额度错误响应拖到查询完成之后。
                .set_delay(Duration::from_millis(750))
                .set_body_json(json!({
                    "rate_limit": {
                        "allowed": true,
                        "limit_reached": false,
                        "primary_window": {"used_percent": 99, "reset_at": reset_at},
                        "secondary_window": {"used_percent": 0}
                    }
                })),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "error": {
                "type": "usage_limit_reached",
                "message": "usage limit reached",
                "resets_at": reset_at
            }
        })))
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
    assert_eq!(account.quota().access(), QuotaAccessState::Exhausted);
    assert_eq!(
        account.quota().evidence(),
        Some(QuotaEvidence::UsageLimitReached)
    );
    let projected = store
        .get_quotas(&[account.id().clone()])
        .await
        .expect("read immediate quota projection")
        .into_iter()
        .next()
        .expect("confirmed quota observation");
    let projected_observed_at = projected.observed_at;
    let projected = Value::Object(projected.quota.into_inner());
    assert_eq!(
        projected
            .pointer("/rate_limit/primary_window/used_percent")
            .and_then(Value::as_u64),
        Some(99)
    );
    assert_eq!(
        projected
            .pointer("/rate_limit/secondary_window/used_percent")
            .and_then(Value::as_u64),
        Some(0),
        "request failure must not rewrite the raw display document"
    );
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

    timeout(Duration::from_secs(5), async {
        loop {
            let requests = server.received_requests().await.expect("received requests");
            if requests
                .iter()
                .any(|request| request.url.path() == "/api/codex/usage")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("background usage refresh must run");
    let observation = store
        .get_quotas(&[account.id().clone()])
        .await
        .expect("read refreshed quota")
        .into_iter()
        .next()
        .expect("authoritative quota projection");
    assert_eq!(observation.observed_at, projected_observed_at);
    let quota = Value::Object(observation.quota.into_inner());
    assert_eq!(
        quota
            .pointer("/rate_limit/primary_window/used_percent")
            .and_then(Value::as_u64),
        Some(99)
    );
    assert_eq!(
        quota
            .pointer("/rate_limit/primary_window/reset_at")
            .and_then(Value::as_i64),
        Some(reset_at)
    );
    assert_eq!(
        quota
            .pointer("/rate_limit/secondary_window/used_percent")
            .and_then(Value::as_u64),
        Some(0),
        "a blocked refresh must preserve the raw quota document"
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
            .quota()
            .access(),
        QuotaAccessState::Exhausted,
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
async fn same_account_previous_response_not_found_should_remain_client_visible() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_client_history").await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(concat!(
                    "event: response.failed\n",
                    "data: {\"type\":\"response.failed\",\"status_code\":400,\"response\":{\"id\":\"resp_history_missing\",\"status\":\"failed\",\"error\":{\"code\":\"previous_response_not_found\",\"message\":\"Previous response was not found. Retrying the full request.\"}}}\n\n"
                )),
        )
        .expect(1)
        .mount(&server)
        .await;
    let payload = ProtocolPayload::json_object(
        "openai",
        Map::from_iter([
            ("model".to_owned(), json!("gpt-5.4")),
            (
                "input".to_owned(),
                json!([{"role": "user", "content": "delta"}]),
            ),
            (
                "previous_response_id".to_owned(),
                json!("missing-previous-response"),
            ),
        ]),
    )
    .expect("OpenAI payload")
    .with_context(Map::from_iter([("use_websocket".to_owned(), json!(false))]));
    let mut stream = provider_with_base_url(&store, server.uri())
        .execute(
            planned_request(
                "openai",
                Operation::Generate(GenerateRequest::from_protocol_payload(payload)),
            ),
            context_with_state_owner("req_history_missing", "acct_client_history"),
        )
        .await
        .expect("prepare missing-history stream");
    let mut failure = loop {
        match stream.next().await {
            Some(Ok(_)) => {}
            Some(Err(error)) => break error,
            None => panic!("previous_response_not_found must produce a typed failure"),
        }
    };

    assert_eq!(
        failure.kind(),
        ProviderErrorKind::ContinuationRecoveryRequired
    );
    assert_eq!(
        failure.continuation_failure(),
        Some(ContinuationFailure::HistoryUnavailable)
    );
    assert!(!failure.replay_is_safe());
    assert_eq!(
        failure
            .upstream_code()
            .map(gateway_core::error::OpaqueUpstreamValue::as_str),
        Some("previous_response_not_found")
    );
    let events = failure.take_atomic_client_events();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].wire_event().and_then(|wire| wire.event_type()),
        Some("response.failed")
    );
}

#[tokio::test]
async fn exact_websocket_busy_then_replay_scope_relaxation_is_rejected_before_send() {
    const ACCOUNT_ID: &str = "acct_websocket_busy_replay";
    const CONVERSATION_ID: &str = "conversation-websocket-busy-replay";
    const CLIENT_PREVIOUS_RESPONSE_ID: &str = "client-resp-busy-seed";
    const UPSTREAM_PREVIOUS_RESPONSE_ID: &str = "resp_busy_seed";

    fn operation(
        previous_response_id: Option<&str>,
        session_state: ProviderSessionState,
    ) -> Operation {
        let mut body = Map::from_iter([
            ("model".to_owned(), json!("gpt-5.4")),
            ("input".to_owned(), json!("hello")),
        ]);
        if let Some(previous_response_id) = previous_response_id {
            body.insert(
                "previous_response_id".to_owned(),
                json!(previous_response_id),
            );
        }
        Operation::Generate(
            GenerateRequest::from_protocol_payload(
                ProtocolPayload::json_object("openai", body).expect("OpenAI payload"),
            )
            .with_provider_session_state(session_state),
        )
    }

    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, ACCOUNT_ID).await;
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind upstream listener");
    let base_url = format!(
        "http://{}",
        listener.local_addr().expect("listener address")
    );
    let (release_busy_sender, release_busy_receiver) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept WebSocket");
        let mut websocket = accept_codex_test_websocket(stream).await;

        let seed = websocket
            .next()
            .await
            .expect("seed request")
            .expect("valid seed request");
        let Message::Text(seed) = seed else {
            panic!("seed request must be text");
        };
        let seed: Value = serde_json::from_str(&seed).expect("seed request JSON");
        assert_eq!(seed.get("previous_response_id"), None);
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": UPSTREAM_PREVIOUS_RESPONSE_ID,
                        "model": "gpt-5.4",
                        "status": "completed",
                        "output": [],
                        "usage": {
                            "input_tokens": 1,
                            "output_tokens": 1,
                            "total_tokens": 2
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("complete seed request");

        let busy = websocket
            .next()
            .await
            .expect("busy continuation request")
            .expect("valid busy continuation request");
        let Message::Text(busy) = busy else {
            panic!("busy continuation request must be text");
        };
        let busy: Value = serde_json::from_str(&busy).expect("busy continuation JSON");
        assert_eq!(
            busy.get("previous_response_id").and_then(Value::as_str),
            Some(UPSTREAM_PREVIOUS_RESPONSE_ID)
        );
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.output_text.delta",
                    "delta": "the exact WebSocket is busy"
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("send busy stream output");

        release_busy_receiver.await.expect("release busy stream");
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_busy_finished",
                        "model": "gpt-5.4",
                        "status": "completed",
                        "output": [],
                        "usage": {
                            "input_tokens": 1,
                            "output_tokens": 1,
                            "total_tokens": 2
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("complete busy stream");
        assert!(
            timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "connection-local replay must not open another upstream connection"
        );
    });
    let provider = provider_with_base_url(&store, base_url);
    let initial_session_state = ProviderSessionState::new(
        "openai",
        Map::from_iter([
            ("account_id".to_owned(), json!(ACCOUNT_ID)),
            ("conversation_id".to_owned(), json!(CONVERSATION_ID)),
            ("continuation_scope".to_owned(), json!("connection_local")),
        ]),
    )
    .expect("initial provider session state");

    let mut seed = provider
        .execute(
            planned_request("openai", operation(None, initial_session_state)),
            context("req_ws_busy_seed", CancellationToken::new()),
        )
        .await
        .expect("prepare seed stream");
    let mut session_state = None;
    while let Some(event) = seed.next().await {
        let event = event.expect("seed event");
        if let Some(update) = event.session_update() {
            session_state = Some(update.clone());
        }
    }
    let session_state = session_state.expect("seed session update");
    assert_eq!(
        session_state
            .payload()
            .get("continuation_scope")
            .and_then(Value::as_str),
        Some("connection_local")
    );

    let continuation = operation(Some(CLIENT_PREVIOUS_RESPONSE_ID), session_state);
    let mut busy = provider
        .execute(
            planned_request("openai", continuation.clone()),
            pinned_continuation_context(
                "req_ws_busy_owner",
                ACCOUNT_ID,
                CLIENT_PREVIOUS_RESPONSE_ID,
                UPSTREAM_PREVIOUS_RESPONSE_ID,
                1,
                ContinuationAttempt::Native,
            ),
        )
        .await
        .expect("prepare busy continuation stream");
    loop {
        let event = busy
            .next()
            .await
            .expect("busy stream event")
            .expect("valid busy stream event");
        if event.has_client_event() {
            break;
        }
    }

    let mut exact = provider
        .execute(
            planned_request("openai", continuation.clone()),
            pinned_continuation_context(
                "req_ws_busy_exact",
                ACCOUNT_ID,
                CLIENT_PREVIOUS_RESPONSE_ID,
                UPSTREAM_PREVIOUS_RESPONSE_ID,
                1,
                ContinuationAttempt::Native,
            ),
        )
        .await
        .expect("prepare competing exact continuation");
    let exact_error = loop {
        match exact.next().await {
            Some(Ok(_)) => {}
            Some(Err(error)) => break error,
            None => panic!("competing exact continuation must fail while its socket is busy"),
        }
    };
    assert_eq!(
        exact_error.kind(),
        ProviderErrorKind::ContinuationRecoveryRequired
    );
    assert_eq!(
        exact_error.continuation_failure(),
        Some(ContinuationFailure::Busy)
    );
    assert_eq!(
        exact_error.continuation_recovery_disposition(),
        Some(ContinuationRecoveryDisposition::ClientReplayRequired)
    );

    for (request_id, attempt_index, attempt) in [
        (
            "req_ws_busy_replay_owner",
            2,
            ContinuationAttempt::ReplayOwner,
        ),
        ("req_ws_busy_replay_any", 3, ContinuationAttempt::ReplayAny),
    ] {
        let result = provider
            .execute(
                planned_request("openai", continuation.clone()),
                pinned_continuation_context(
                    request_id,
                    ACCOUNT_ID,
                    CLIENT_PREVIOUS_RESPONSE_ID,
                    UPSTREAM_PREVIOUS_RESPONSE_ID,
                    attempt_index,
                    attempt,
                ),
            )
            .await;
        let Err(replay_error) = result else {
            panic!("connection-local scope relaxation must fail before stream creation");
        };
        let detail = replay_error
            .client_visible_upstream_error()
            .expect("client-visible replay error");

        assert_eq!(
            (
                replay_error.kind(),
                replay_error.send_state(),
                replay_error.continuation_failure(),
                replay_error.continuation_recovery_disposition(),
                detail.code(),
                detail.error_type(),
                detail.message(),
            ),
            (
                ProviderErrorKind::ContinuationRecoveryRequired,
                UpstreamSendState::NotSent,
                Some(ContinuationFailure::HistoryUnavailable),
                Some(ContinuationRecoveryDisposition::ClientReplayRequired),
                Some("previous_response_not_found"),
                Some("invalid_request_error"),
                "Previous response was not found. Retrying the full request.",
            )
        );
    }

    release_busy_sender.send(()).expect("release busy stream");
    while busy
        .next()
        .await
        .transpose()
        .expect("busy stream event")
        .is_some()
    {}
    server.await.expect("upstream server");
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
        .apply_quota_access(QuotaAccessChange {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            state: QuotaState::exhausted(QuotaEvidence::UsageLimitReached, SystemTime::now(), None),
        })
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
    assert_eq!(account.quota().access(), QuotaAccessState::Exhausted);
    assert!(!store.has_quota(account_id));
    assert_eq!(affinity.binding_count(), 0);
}

#[tokio::test]
async fn quota_limited_account_diagnostic_uses_upstream() {
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_quota_limited_diagnostic";
    create_account(&store, account_id).await;
    let account = store.account(account_id).expect("test account");
    let raw_quota = json!({
        "rate_limit": {
            "allowed": false,
            "limit_reached": true,
            "primary_window": {"used_percent": 100, "reset_at": 1_900_000_000}
        }
    });
    let observed_at = SystemTime::now();
    store
        .compare_and_swap_quota(QuotaObservation {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            quota: OpaqueProviderData::new(
                raw_quota
                    .as_object()
                    .expect("quota snapshot object")
                    .clone(),
            ),
            observed_at,
            state: QuotaState::exhausted(QuotaEvidence::ProviderDenied, observed_at, None),
        })
        .await
        .expect("seed quota-limited snapshot");

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(concat!(
                    "event: response.created\n",
                    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_quota_limited_diagnostic\",\"model\":\"gpt-5.4\"}}\n\n",
                    "event: response.completed\n",
                    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_quota_limited_diagnostic\",\"model\":\"gpt-5.4\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n"
                )),
        )
        .expect(1)
        .mount(&server)
        .await;
    let (provider, quota) = provider_and_quota_with_affinity_and_base_url_and_leases(
        &store,
        Arc::new(MemorySessionAffinity::default()),
        server.uri(),
        Arc::new(TestLeaseCoordinator::default()),
        u32::try_from(DEFAULT_STREAM_MAX_RETRIES).expect("default retry budget fits u32"),
    );
    quota
        .prepare_scheduling(std::slice::from_ref(&account))
        .await;

    let mut stream = provider
        .execute(
            planned_request("openai", http_generate_operation()),
            diagnostic_context("req_quota_limited_diagnostic", account_id),
        )
        .await
        .expect("quota-limited diagnostic should prepare a fixed-account stream");
    let mut completed = false;
    while let Some(event) = stream.next().await {
        let event = event.expect("quota-limited diagnostic upstream response");
        completed |= event
            .canonical_facts()
            .iter()
            .any(|event| matches!(event, GatewayEvent::Completed(_)));
    }

    assert!(completed);
    let requests = server
        .received_requests()
        .await
        .expect("captured quota-limited diagnostic request");
    assert_eq!(requests.len(), 1);
}

#[tokio::test]
async fn successful_response_treats_inference_success_as_authoritative_allowance() {
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
            .quota()
            .access(),
        QuotaAccessState::Allowed
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
    assert_eq!(account.credential_state(), CredentialState::Ready);
    assert_eq!(account.quota().access(), QuotaAccessState::Allowed);
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

#[tokio::test]
async fn provider_routes_catalog_model_when_supported_in_api_is_false() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_non_api_model").await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_raw(
                    br#"{"models":[{"slug":"gpt-5.6-sol-wm","display_name":"GPT-5.6-Sol-WM","supported_in_api":false,"visibility":"hide"}]}"#,
                    "application/json",
                ),
        )
        .mount(&server)
        .await;
    let provider = provider_with_base_url(&store, server.uri());

    let capabilities = provider
        .query_model_capabilities()
        .await
        .expect("catalog model capabilities");
    let model = capabilities.first().expect("catalog model");

    assert!(
        model
            .capabilities()
            .match_requirements(&CapabilityRequirements::new(OperationKind::Generate))
            .is_some()
    );
}
