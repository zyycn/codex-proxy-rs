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
    AccountFeedbackStats, AccountWeight, CredentialState, OpaqueProviderData, ProviderAccountId,
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
use gateway_core::event::{GatewayEvent, WebSocketPoolKind};
use gateway_core::lifecycle::CancellationToken;
use gateway_core::metering::Usage;
use gateway_core::operation::{
    CapabilityRequirements, GenerateRequest, ImageRequest, ImageRequestKind, Operation,
    OperationKind, ProtocolPayload, ProviderSessionState, RawJsonPayload, StandaloneSearchRequest,
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
        residency: None,
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

fn generate_with_persisted_session_context(
    account_id: &str,
    conversation_id: &str,
    session_id: &str,
    thread_id: &str,
) -> GenerateRequest {
    generate_with_session_context(session_id, Some(thread_id), None).with_provider_session_state(
        ProviderSessionState::new(
            "openai",
            Map::from_iter([
                ("account_id".to_owned(), json!(account_id)),
                ("conversation_id".to_owned(), json!(conversation_id)),
                ("continuation_scope".to_owned(), json!("persisted")),
            ]),
        )
        .expect("provider session state"),
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
        "acct_abrupt_disconnect",
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
        "acct_websocket_fast_path",
        "acct_websocket_busy_replay",
        "acct_websocket_metadata_close",
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
    assert_eq!(
        error.diagnostic().and_then(|diagnostic| diagnostic.code()),
        Some("unexpected_eof")
    );
    assert!(!error.diagnostic().unwrap().as_str().contains("127.0.0.1"));
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
        (
            ImageRequestKind::Generation,
            "/codex/images/generations",
            br#"{"model":"gpt-image-2.5-flare","prompt":"a lighthouse","quality":"xhigh"}"#.as_slice(),
            br#"{"created":1788900000,"data":[{"b64_json":"AAEC"}],"quality":"xhigh"}"#.as_slice(),
        ),
        (
            ImageRequestKind::Edit,
            "/codex/images/edits",
            br#"{"model":"gpt-image-2.5-sunburst","images":[{"image_url":"data:image/png;base64,AAEC"}],"prompt":"add fog","quality":"max"}"#.as_slice(),
            br#"{"created":1788900001,"data":[{"b64_json":"AwQF"}],"quality":"max"}"#.as_slice(),
        ),
    ];
    for (_, endpoint, body, response_body) in &cases {
        Mock::given(method("POST"))
            .and(path(*endpoint))
            .and(header("originator", "codex_cli_rs"))
            .and(header("x-codex-image-turn-id", "turn_image_contract"))
            .and(header("version", "0.144.0"))
            .and(header("accept", "*/*"))
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
            assert!(
                event
                    .canonical_facts()
                    .iter()
                    .all(|fact| !matches!(fact, GatewayEvent::Usage(_))),
                "missing upstream usage must remain unknown"
            );
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
async fn image_endpoints_should_report_usage_before_delivering_the_unchanged_body() {
    let response = br#"{ "created":1778832973,"data":[{"b64_json":"AAEC"}],"quality":"medium","size":"1024x1536","usage":{"input_tokens":1474,"input_tokens_details":{"image_tokens":1457,"text_tokens":17,"cached_tokens":100},"output_tokens":1372,"output_tokens_details":{"image_tokens":1372,"text_tokens":0},"total_tokens":2846},"future":9007199254740993 }"#;
    let expected = Usage {
        input_tokens: Some(1474),
        output_tokens: Some(1372),
        cached_tokens: Some(100),
        image_input_tokens: Some(1457),
        image_output_tokens: Some(1372),
        total_tokens: Some(2846),
        ..Usage::default()
    };
    for kind in [ImageRequestKind::Generation, ImageRequestKind::Edit] {
        let events = image_usage_events(kind, response).await;
        let usage = events
            .iter()
            .flat_map(|event| event.canonical_facts())
            .filter_map(|fact| match fact {
                GatewayEvent::Usage(usage) => Some(usage.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(usage.as_slice(), std::slice::from_ref(&expected));
        let usage_index = events
            .iter()
            .position(|event| {
                event
                    .canonical_facts()
                    .iter()
                    .any(|fact| matches!(fact, GatewayEvent::Usage(_)))
            })
            .expect("usage event");
        let wire_index = events
            .iter()
            .position(|event| {
                event
                    .wire_event()
                    .and_then(|wire| wire.raw_json_body())
                    .is_some()
            })
            .expect("raw image response");
        assert!(
            usage_index < wire_index,
            "usage must be observed before image delivery"
        );
        assert_eq!(
            events[wire_index]
                .wire_event()
                .and_then(|wire| wire.raw_json_body())
                .map(|body| body.as_ref()),
            Some(response.as_slice())
        );
    }
}

#[tokio::test]
async fn image_usage_should_preserve_unknown_fields_and_explicit_zero_counts() {
    let cases = [
        (json!(null), None),
        (json!({}), None),
        (json!([]), None),
        (
            json!({"input_tokens": -1, "output_tokens": "42", "total_tokens": 1.5}),
            None,
        ),
        (
            json!({"input_tokens": 0, "output_tokens": 0, "total_tokens": 0, "input_tokens_details": {"image_tokens": 0, "cached_tokens": 0}, "output_tokens_details": {"image_tokens": 0}}),
            Some(Usage {
                input_tokens: Some(0),
                output_tokens: Some(0),
                total_tokens: Some(0),
                cached_tokens: Some(0),
                image_input_tokens: Some(0),
                image_output_tokens: Some(0),
                ..Usage::default()
            }),
        ),
        (
            json!({"input_tokens": 17, "output_tokens": 1372, "input_tokens_details": {"image_tokens": "invalid"}, "output_tokens_details": {"image_tokens": 1372}}),
            Some(Usage {
                input_tokens: Some(17),
                output_tokens: Some(1372),
                image_output_tokens: Some(1372),
                ..Usage::default()
            }),
        ),
    ];
    for (usage, expected) in cases {
        let response = serde_json::to_vec(
            &json!({"created":1778832973,"data":[{"b64_json":"AAEC"}],"usage":usage}),
        )
        .expect("image response JSON");
        let events = image_usage_events(ImageRequestKind::Generation, &response).await;
        let actual = events
            .iter()
            .flat_map(|event| event.canonical_facts())
            .filter_map(|fact| match fact {
                GatewayEvent::Usage(usage) => Some(usage.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(actual, expected.into_iter().collect::<Vec<_>>());
        let raw = events
            .iter()
            .find_map(|event| event.wire_event().and_then(|wire| wire.raw_json_body()));
        assert_eq!(raw.map(|body| body.as_ref()), Some(response.as_slice()));
    }
}

#[tokio::test]
async fn image_prices_should_use_modality_rates_and_precede_delivery() {
    // First case is the real gpt-image-2 response verified on 2026-09-08.
    let cases = [
        (18, 0, 229, None, 69_600_000_u128),
        (17, 1457, 1372, None, 529_010_000),
        (100, 0, 10, Some(40), 6_500_000),
        (0, 100, 10, Some(40), 8_600_000),
        (20, 80, 10, Some(100), 4_850_000),
        (0, 0, 0, Some(0), 0),
    ];
    for (text, image, output, cached, expected_ticks) in cases {
        let mut usage = json!({
            "input_tokens":text+image,
            "input_tokens_details":{"text_tokens":text,"image_tokens":image},
            "output_tokens":output,
            "output_tokens_details":{"text_tokens":0,"image_tokens":output},
            "total_tokens":text+image+output,
        });
        if let Some(cached) = cached {
            usage["input_tokens_details"]["cached_tokens"] = json!(cached);
        }
        let response = serde_json::to_vec(&json!({"data":[{"b64_json":"AAEC"}],"usage":usage}))
            .expect("response");
        for (kind, model) in [
            (ImageRequestKind::Generation, "gpt-image-2"),
            (ImageRequestKind::Edit, "gpt-image-2-2026-04-21"),
            (ImageRequestKind::Generation, "gpt-image-2.5-sunburst"),
            (ImageRequestKind::Edit, "gpt-image-2.5-sunburst-2026-09-08"),
            (ImageRequestKind::Generation, "gpt-image-2.5-flare"),
            (ImageRequestKind::Edit, "gpt-image-2.5-flare-2026-09-08"),
        ] {
            let events = image_metering_events(kind, &response, model).await;
            let costs = events
                .iter()
                .enumerate()
                .flat_map(|(index, event)| {
                    event
                        .canonical_facts()
                        .iter()
                        .filter_map(move |fact| match fact {
                            GatewayEvent::CalculatedCost(cost) => Some((index, *cost)),
                            _ => None,
                        })
                })
                .collect::<Vec<_>>();
            assert_eq!(costs.len(), 1, "model={model}");
            let (cost_index, cost) = costs[0];
            assert_eq!(
                cost.total().amount().scaled(),
                expected_ticks,
                "model={model}"
            );
            assert_eq!(cost.into_estimate().source().as_str(), "calculated");
            let wire_index = events
                .iter()
                .position(|event| event.wire_event().is_some())
                .expect("wire event");
            assert!(cost_index < wire_index);
            assert_eq!(
                events[wire_index]
                    .wire_event()
                    .and_then(|wire| wire.raw_json_body())
                    .map(|body| body.as_ref()),
                Some(response.as_slice())
            );
        }
    }
}

#[tokio::test]
async fn image_prices_should_remain_unknown_when_modality_or_model_is_uncertain() {
    let valid = json!({
        "input_tokens":100,
        "input_tokens_details":{"text_tokens":20,"image_tokens":80},
        "output_tokens":10,
        "output_tokens_details":{"text_tokens":0,"image_tokens":10},
        "total_tokens":110,
    });
    let mut cases = vec![
        ("gpt-image-future", valid.clone()),
        ("gpt-image-2-future", valid.clone()),
        ("gpt-image-2.5", valid.clone()),
        ("gpt-image-2.5-sunburst-future", valid.clone()),
        ("gpt-image-2.5-flare-future", valid.clone()),
        ("gpt-image-2", json!(null)),
        (
            "gpt-image-2",
            json!({"input_tokens":100,"output_tokens":10}),
        ),
    ];
    for (pointer, value) in [
        ("/input_tokens", json!(101)),
        ("/input_tokens_details/text_tokens", json!(null)),
        ("/input_tokens_details/image_tokens", json!("80")),
        ("/output_tokens_details/text_tokens", json!(1)),
        ("/output_tokens_details/image_tokens", json!(9)),
        ("/output_tokens", json!(-1)),
        ("/total_tokens", json!(111)),
    ] {
        let mut usage = valid.clone();
        *usage.pointer_mut(pointer).expect("existing field") = value;
        cases.push(("gpt-image-2", usage));
    }
    for cached in [json!(40), json!(101), json!(null), json!("40")] {
        let mut usage = valid.clone();
        usage["input_tokens_details"]["cached_tokens"] = cached;
        cases.push(("gpt-image-2", usage));
    }
    cases.push(("gpt-image-2", json!({
        "input_tokens":u64::MAX,"input_tokens_details":{"text_tokens":u64::MAX,"image_tokens":0},
        "output_tokens":10,
    })));
    for (model, usage) in cases {
        let response = serde_json::to_vec(&json!({"data":[{"b64_json":"AAEC"}],"usage":usage}))
            .expect("response");
        let events = image_metering_events(ImageRequestKind::Generation, &response, model).await;
        assert!(
            events
                .iter()
                .flat_map(|event| event.canonical_facts())
                .all(|fact| !matches!(fact, GatewayEvent::CalculatedCost(_)))
        );
        assert_eq!(
            events
                .iter()
                .find_map(|event| event.wire_event().and_then(|wire| wire.raw_json_body()))
                .map(|body| body.as_ref()),
            Some(response.as_slice())
        );
    }
}

async fn image_usage_events(
    kind: ImageRequestKind,
    response: &[u8],
) -> Vec<gateway_core::event::ProviderEvent> {
    image_metering_events(kind, response, "gpt-image-2").await
}

async fn image_metering_events(
    kind: ImageRequestKind,
    response: &[u8],
    model: &str,
) -> Vec<gateway_core::event::ProviderEvent> {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let server = MockServer::start().await;
    let endpoint = match kind {
        ImageRequestKind::Generation => "/codex/images/generations",
        ImageRequestKind::Edit => "/codex/images/edits",
    };
    Mock::given(method("POST"))
        .and(path(endpoint))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(response.to_vec(), "application/json"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let provider = provider_with_base_url(&store, server.uri());
    let payload = RawJsonPayload::new(
        "openai",
        Bytes::from(
            serde_json::to_vec(&json!({"model":model,"prompt":"draw a tree"}))
                .expect("request JSON"),
        ),
    )
    .expect("image payload");
    let operation = Operation::GenerateImage(ImageRequest::from_raw_json(kind, payload));
    let mut stream = provider
        .execute(
            planned_provider_endpoint_request("openai", operation),
            context("req_image_usage", CancellationToken::new()),
        )
        .await
        .expect("prepare image stream");
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.expect("image provider event"));
    }
    server.verify().await;
    events
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
async fn search_and_images_should_share_responses_affinity_and_existing_busy_failover() {
    assert_cross_endpoint_affinity(None).await;
}

#[tokio::test]
async fn child_search_and_images_should_share_child_failover_without_changing_the_root() {
    assert_cross_endpoint_affinity(Some("child-thread")).await;
}

async fn assert_cross_endpoint_affinity(thread_id: Option<&str>) {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_affinity_switch_a").await;
    create_account(&store, "acct_affinity_switch_b").await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let leases = Arc::new(TestLeaseCoordinator::default());
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/alpha/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"output":"result"})))
        .expect(2)
        .mount(&server)
        .await;
    let provider = provider_with_affinity_and_base_url_and_leases(
        &store,
        Arc::clone(&affinity),
        server.uri(),
        Arc::clone(&leases),
    );
    let root = Operation::Generate(generate_with_session_context("shared-root", None, None));
    let root_stream = provider
        .execute(
            planned_request("openai", root.clone()),
            context("req_cross_endpoint_root", CancellationToken::new()),
        )
        .await
        .expect("root selection");
    let root_account = root_stream.metadata().provider_account_id().clone();
    drop(root_stream);
    let generation = Operation::Generate(generate_with_session_context(
        "shared-root",
        thread_id,
        None,
    ));
    let first = provider
        .execute(
            planned_request("openai", generation.clone()),
            context("req_cross_endpoint_first", CancellationToken::new()),
        )
        .await
        .expect("first selection");
    let first_account = first.metadata().provider_account_id().clone();
    assert_eq!(
        first_account, root_account,
        "new child inherits the root account"
    );
    drop(first);
    let other_account = if first_account.as_str() == "acct_affinity_switch_a" {
        "acct_affinity_switch_b"
    } else {
        "acct_affinity_switch_a"
    };
    store.set_scheduling(
        other_account,
        None,
        AccountWeight::new(100).expect("weight"),
    );

    // 官方 Search 正文的 id 与 Responses 的 session-id 是同一个根身份。
    let search = Operation::Search(StandaloneSearchRequest::from_raw_json(
        RawJsonPayload::new(
            "openai",
            Bytes::from_static(br#"{ "id":"shared-root", "commands":{}, "future":1, "future":2 }"#),
        )
        .expect("search payload")
        .with_context(thread_id.map_or_else(Map::new, |thread_id| {
            Map::from_iter([(
                "turn_metadata".to_owned(),
                json!(json!({"thread_id":thread_id}).to_string()),
            )])
        })),
    ));
    let client_key = ClientApiKeyId::new("key_openai_contract").expect("client key");
    assert_eq!(
        provider
            .request_observation(&search, &client_key)
            .continuation
            .affinity_hash,
        provider
            .request_observation(&generation, &client_key)
            .continuation
            .affinity_hash
    );
    let mut same = provider
        .execute(
            planned_provider_endpoint_request("openai", search.clone()),
            context("req_cross_endpoint_search", CancellationToken::new()),
        )
        .await
        .expect("search selection");
    assert_eq!(
        same.metadata().provider_account_id(),
        &first_account,
        "session affinity outranks the other account's weight"
    );
    while let Some(event) = same.next().await {
        event.expect("search response");
    }
    drop(same);

    // 仍由旧租约流程报告繁忙并换号，亲和不绕过并发限制。
    leases
        .busy_accounts
        .lock()
        .expect("busy accounts")
        .insert(first_account.clone());
    let mut fallback = provider
        .execute(
            planned_provider_endpoint_request("openai", search),
            context("req_cross_endpoint_busy", CancellationToken::new()),
        )
        .await
        .expect("busy fallback");
    assert_eq!(
        fallback.metadata().provider_account_id().as_str(),
        other_account
    );
    fallback
        .next()
        .await
        .expect("first event")
        .expect("successful JSON response");
    drop(fallback); // 只消费一个事件也必须完成绑定迁移。
    leases.busy_accounts.lock().expect("busy accounts").clear();
    store.set_scheduling(
        first_account.as_str(),
        None,
        AccountWeight::new(100).expect("weight"),
    );
    store.set_scheduling(other_account, None, AccountWeight::new(1).expect("weight"));

    let resumed = provider
        .execute(
            planned_request("openai", generation.clone()),
            context("req_cross_endpoint_resumed", CancellationToken::new()),
        )
        .await
        .expect("resumed responses");
    assert_eq!(
        resumed.metadata().provider_account_id().as_str(),
        other_account,
        "successful failover is shared back to Responses"
    );
    drop(resumed);
    for kind in [ImageRequestKind::Generation, ImageRequestKind::Edit] {
        let image = Operation::GenerateImage(ImageRequest::from_raw_json(
            kind,
            RawJsonPayload::new("openai", Bytes::from_static(br#"{"prompt":"image"}"#))
                .expect("image payload")
                .with_context({
                    let mut context =
                        Map::from_iter([("session_id".to_owned(), json!("shared-root"))]);
                    if let Some(thread_id) = thread_id {
                        context.insert("thread_id".to_owned(), json!(thread_id));
                    }
                    context
                }),
        ));
        assert_eq!(
            provider
                .request_observation(&image, &client_key)
                .continuation
                .affinity_hash,
            provider
                .request_observation(&generation, &client_key)
                .continuation
                .affinity_hash
        );
        let selected_image = provider
            .execute(
                planned_provider_endpoint_request("openai", image),
                context("req_cross_endpoint_image", CancellationToken::new()),
            )
            .await
            .expect("image selection");
        assert_eq!(
            selected_image.metadata().provider_account_id().as_str(),
            other_account
        );
        drop(selected_image);
    }
    if thread_id.is_some() {
        let root_stream = provider
            .execute(
                planned_request("openai", root),
                context(
                    "req_cross_endpoint_root_after_child",
                    CancellationToken::new(),
                ),
            )
            .await
            .expect("root after child migration");
        assert_eq!(root_stream.metadata().provider_account_id(), &root_account);
        drop(root_stream);
        assert_eq!(affinity.binding_count(), 2, "only root and child bindings");
    } else {
        let keys = affinity.lookup_keys();
        assert!(
            keys.iter().all(|key| key == &keys[0]),
            "all endpoints must use the same affinity key"
        );
    }
    assert_ne!(
        provider
            .request_observation(
                &generation,
                &ClientApiKeyId::new("different-client").expect("client key")
            )
            .continuation
            .affinity_hash,
        provider
            .request_observation(&generation, &client_key)
            .continuation
            .affinity_hash
    );
    let unrelated = Operation::Generate(generate_with_session_context("another-root", None, None));
    assert_ne!(
        provider
            .request_observation(&unrelated, &client_key)
            .continuation
            .affinity_hash,
        provider
            .request_observation(&generation, &client_key)
            .continuation
            .affinity_hash
    );
    server.verify().await;
}

#[tokio::test]
async fn standalone_search_preserves_wire_and_scopes_turn_metadata_to_the_selected_account() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let server = MockServer::start().await;
    let request_body = br#"{ "id":"search-session", "model":"gpt-future", "commands":{"search_query":[{"q":"private query","future":9007199254740993}]}, "future":1, "future":2 }"#;
    let response_body = br#"{ "encrypted_output":"ciphertext", "output":"search result", "results":[{"type":"text_result","ref_id":"turn0search0","future":9007199254740993}] }"#;
    Mock::given(method("POST"))
        .and(path("/codex/alpha/search"))
        .and(header("originator", "codex_cli_rs"))
        .and(header("version", "0.144.0"))
        .and(header("accept", "*/*"))
        .and(body_bytes(request_body.to_vec()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .insert_header("x-future-search-header", "preserved")
                .set_body_raw(response_body.to_vec(), "application/json"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let turn_metadata = r#"{"session_id":"session","thread_id":"thread","turn_id":"turn","installation_id":"client-installation","account_id":"client-account","future":true}"#;
    let payload = RawJsonPayload::new("openai", Bytes::from_static(request_body))
        .expect("search payload")
        .with_context(Map::from_iter([(
            "turn_metadata".to_owned(),
            json!(turn_metadata),
        )]));
    let operation = Operation::Search(StandaloneSearchRequest::from_raw_json(payload));
    let mut stream = provider_with_base_url(&store, server.uri())
        .execute(
            planned_provider_endpoint_request("openai", operation),
            context("req_search_contract", CancellationToken::new()),
        )
        .await
        .expect("prepare search provider stream");
    let mut raw_response = None;
    let mut completed = false;
    let mut observed_http_json = false;
    while let Some(event) = stream.next().await {
        let event = event.expect("search provider event");
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
                    header.name() == "x-future-search-header"
                        && header.value().as_ref() == b"preserved"
                });
        }
    }

    assert!(completed);
    assert!(observed_http_json);
    assert_eq!(raw_response.as_deref(), Some(response_body.as_slice()));
    let requests = server.received_requests().await.expect("received requests");
    let request = requests.first().expect("single search request");
    assert_eq!(request.body.as_slice(), request_body);
    let metadata_values = captured_header_values(request, "x-codex-turn-metadata");
    assert_eq!(metadata_values.len(), 1);
    let metadata: Value = serde_json::from_slice(&metadata_values[0]).expect("turn metadata JSON");
    assert_eq!(metadata.get("session_id"), Some(&json!("session")));
    assert_eq!(metadata.get("thread_id"), Some(&json!("thread")));
    assert_eq!(metadata.get("turn_id"), Some(&json!("turn")));
    assert_eq!(metadata.get("future"), Some(&json!(true)));
    assert!(metadata.get("account_id").is_none());
    assert_ne!(
        metadata.get("installation_id"),
        Some(&json!("client-installation"))
    );
    server.verify().await;
}

#[tokio::test]
async fn standalone_search_returns_the_exact_upstream_error_response() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let server = MockServer::start().await;
    let request_body =
        br#"{ "id":"search-session", "model":"gpt-future", "commands":{"open":[]} }"#;
    let response_body = br#"{ "error":{"message":"future search validation","type":"search_error","code":"future_code"}, "future":9007199254740993 }"#;
    Mock::given(method("POST"))
        .and(path("/codex/alpha/search"))
        .and(body_bytes(request_body.to_vec()))
        .respond_with(
            ResponseTemplate::new(422)
                .insert_header("content-type", "application/problem+json")
                .insert_header("x-future-search-error", "preserved")
                .set_body_bytes(response_body.to_vec()),
        )
        .expect(1)
        .mount(&server)
        .await;

    let payload =
        RawJsonPayload::new("openai", Bytes::from_static(request_body)).expect("search payload");
    let operation = Operation::Search(StandaloneSearchRequest::from_raw_json(payload));
    let mut stream = provider_with_base_url(&store, server.uri())
        .execute(
            planned_provider_endpoint_request("openai", operation),
            context("req_search_error", CancellationToken::new()),
        )
        .await
        .expect("prepare search provider stream");
    let error = loop {
        match stream.next().await {
            Some(Ok(_)) => {}
            Some(Err(error)) => break error,
            None => panic!("search rejection must surface an upstream response"),
        }
    };

    assert_eq!(error.upstream_status(), Some(422));
    let response = error
        .client_visible_upstream_response()
        .expect("raw upstream search error response");
    assert_eq!(response.status(), 422);
    assert_eq!(
        response.content_type(),
        Some(b"application/problem+json".as_slice())
    );
    assert_eq!(response.body().as_ref(), response_body);
    assert!(response.headers().iter().any(|header| {
        header.name() == "x-future-search-error" && header.value().as_ref() == b"preserved"
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
async fn websocket_close_after_delivery_preserves_details_and_reconnects_through_pool() {
    const ACCOUNT_ID: &str = "acct_websocket_close";
    const CONVERSATION_ID: &str = "conversation-websocket-close-after-delivery";
    const SESSION_ID: &str = "committed-websocket-session";

    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, ACCOUNT_ID).await;
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
                    "type": "response.created",
                    "response": {
                        "id": "resp_before_committed_close",
                        "model": "gpt-5.4"
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("start WebSocket response");
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
                    "type": "response.created",
                    "response": {
                        "id": "resp_after_committed_close",
                        "model": "gpt-5.4"
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("start next-turn WebSocket response");
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
    let first_operation = Operation::Generate(generate_with_persisted_session_context(
        ACCOUNT_ID,
        CONVERSATION_ID,
        SESSION_ID,
        "thread-first",
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

    let second_operation = Operation::Generate(generate_with_persisted_session_context(
        ACCOUNT_ID,
        CONVERSATION_ID,
        SESSION_ID,
        "thread-second",
    ));
    let mut next_stream = provider
        .execute(
            planned_request("openai", second_operation),
            context("req_after_committed_close", CancellationToken::new()),
        )
        .await
        .expect("post-delivery close must allow a fresh WebSocket client retry");
    assert_eq!(next_stream.metadata().transport().as_str(), "websocket");
    let mut retry_pool_observation = None;
    while let Some(event) = next_stream.next().await {
        let event = event.expect("next turn WebSocket response");
        if let Some(observation) = event.response_observation()
            && observation.transport().as_str() == "websocket"
        {
            retry_pool_observation = Some(observation.websocket_pool());
        }
    }
    assert_eq!(retry_pool_observation, Some(Some(WebSocketPoolKind::New)));
    server.await.expect("WebSocket server");
}

#[tokio::test]
async fn websocket_fast_path_miss_uses_http_and_keeps_background_preconnect() {
    const ACCOUNT_ID: &str = "acct_websocket_fast_path";
    const CONVERSATION_ID: &str = "conversation-websocket-fast-path";
    const SESSION_ID: &str = "websocket-fast-path-session";

    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, ACCOUNT_ID).await;
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind upstream listener");
    let base_url = format!(
        "http://{}",
        listener.local_addr().expect("listener address")
    );
    let (preconnect_ready_tx, preconnect_ready_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (websocket_stream, _) = listener
            .accept()
            .await
            .expect("accept background WebSocket opening");
        let websocket = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let mut websocket = accept_codex_test_websocket(websocket_stream).await;
            assert!(
                timeout(Duration::from_millis(100), websocket.next())
                    .await
                    .is_err(),
                "background preconnect must not send the HTTP-fallback request payload"
            );
            preconnect_ready_tx
                .send(())
                .expect("signal background WebSocket readiness");
            let request = websocket
                .next()
                .await
                .expect("next request should reuse the background WebSocket")
                .expect("valid reused WebSocket request");
            let payload: Value =
                serde_json::from_str(request.to_text().expect("reused WebSocket request text"))
                    .expect("reused WebSocket request JSON");
            assert_eq!(payload.get("type"), Some(&json!("response.create")));
            websocket
                .send(Message::Text(
                    json!({
                        "type": "response.completed",
                        "response": {
                            "id": "resp_background_preconnect",
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
                .expect("complete reused WebSocket request");
        });

        let (mut http, _) = listener
            .accept()
            .await
            .expect("accept HTTP fallback request");
        let request = capture_http_request(&mut http).await;
        assert!(String::from_utf8_lossy(&request).starts_with("POST /codex/responses"));
        http.write_all(
            format!(
                "HTTP/1.1 200 OK\r\nopenai-model: gpt-reported-by-upstream\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{CAPTURE_COMPLETED_SSE}",
                CAPTURE_COMPLETED_SSE.len()
            )
            .as_bytes(),
        )
        .await
        .expect("write HTTP fallback response");
        websocket.await.expect("background WebSocket server");
    });

    let provider = provider_with_base_url(&store, base_url);
    let operation = |thread_id| {
        Operation::Generate(generate_with_persisted_session_context(
            ACCOUNT_ID,
            CONVERSATION_ID,
            SESSION_ID,
            thread_id,
        ))
    };

    let mut first = provider
        .execute(
            planned_request("openai", operation("thread-http-fallback")),
            context("req_fast_path_http", CancellationToken::new()),
        )
        .await
        .expect("prepare first provider stream");
    let mut first_transport = None;
    let mut first_decision = None;
    let mut first_timings = None;
    let mut first_http_version = None;
    while let Some(event) = first.next().await {
        let event = event.expect("fast-path miss must not surface as a provider error");
        if let Some(observation) = event.response_observation() {
            first_transport = Some(observation.transport().as_str().to_owned());
            first_timings = Some(observation.timings());
            first_http_version = observation.http_version();
            if let Some(provider_metadata) = observation.provider_metadata() {
                let metadata: Value = serde_json::from_str(provider_metadata.as_json())
                    .expect("OpenAI provider metadata JSON");
                assert_eq!(metadata["schemaVersion"], 2);
                for field in [
                    "attemptAccountId",
                    "attemptIndex",
                    "compact",
                    "httpVersion",
                    "effectiveModel",
                    "serviceTier",
                    "transportDecisionWaitMs",
                    "wsConnectMs",
                    "upstreamHeadersMs",
                    "firstEventMs",
                    "firstReasoningMs",
                    "firstTextMs",
                    "firstTokenMs",
                    "openaiProcessingMs",
                    "websocketPool",
                    "cfRay",
                ] {
                    assert!(
                        metadata.get(field).is_none(),
                        "duplicate metadata field {field}"
                    );
                }
                assert_eq!(metadata["upstreamStatus"], 200);
                assert_eq!(
                    metadata["upstreamReportedModel"],
                    "gpt-reported-by-upstream"
                );
                assert!(metadata["requestSummary"].is_object());
                assert!(metadata["upstreamTraceHeaders"].is_array());
                first_decision = metadata
                    .get("transportDecision")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
            }
        }
    }
    assert_eq!(first_transport.as_deref(), Some("http_sse"));
    assert_eq!(first_decision.as_deref(), Some("http2_ws_budget_exhausted"));
    assert!(first_http_version.is_some());
    assert!(
        first_timings
            .expect("typed response timings")
            .first_event_ms
            .is_some()
    );

    preconnect_ready_rx
        .await
        .expect("background WebSocket should become ready");
    tokio::time::sleep(Duration::from_millis(20)).await;
    let mut second = provider
        .execute(
            planned_request("openai", operation("thread-websocket-reuse")),
            context("req_background_ws_reuse", CancellationToken::new()),
        )
        .await
        .expect("prepare second provider stream");
    let mut second_transport = None;
    let mut second_pool = None;
    while let Some(event) = second.next().await {
        let event = event.expect("background WebSocket reuse should complete");
        if let Some(observation) = event.response_observation() {
            second_transport = Some(observation.transport().as_str().to_owned());
            second_pool = Some(observation.websocket_pool());
        }
    }
    assert_eq!(second_transport.as_deref(), Some("websocket"));
    assert_eq!(second_pool, Some(Some(WebSocketPoolKind::Reuse)));
    server.await.expect("HTTP and WebSocket upstream server");
}

#[tokio::test]
async fn connection_failure_preserves_io_cause_without_claiming_payload_was_sent() {
    for websocket in [false, true] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_abrupt_disconnect").await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let operation = if websocket {
            Operation::Generate(generate_with_persisted_session_context(
                "acct_abrupt_disconnect",
                "conversation-refused",
                "session-refused",
                "turn-refused",
            ))
        } else {
            http_generate_operation()
        };
        let mut stream = provider_with_base_url(&store, base_url)
            .execute(
                planned_request("openai", operation),
                context("req_connect_refused", CancellationToken::new()),
            )
            .await
            .expect("prepare provider stream");
        let error = loop {
            match stream.next().await {
                Some(Ok(_)) => {}
                Some(Err(error)) => break error,
                None => panic!("closed listener must fail"),
            }
        };
        let diagnostic = error.diagnostic().expect("connection diagnosis");
        assert_eq!(diagnostic.code(), Some("connection_refused"));
        assert_eq!(diagnostic.stage(), Some("connect"));
        assert_eq!(error.send_state(), UpstreamSendState::NotSent);
        assert!(!diagnostic.as_str().contains("after payload send"));
        assert!(!diagnostic.as_str().contains("127.0.0.1"));
    }
}

#[tokio::test]
async fn abrupt_websocket_disconnect_preserves_diagnosis_and_ambiguous_send_state() {
    const ACCOUNT_ID: &str = "acct_abrupt_disconnect";
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, ACCOUNT_ID).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        websocket.next().await.unwrap().unwrap();
        // The peer disappears after receiving the payload, without sending a Close frame.
    });
    let operation = Operation::Generate(generate_with_persisted_session_context(
        ACCOUNT_ID,
        "conversation-abrupt-disconnect",
        "session-abrupt-disconnect",
        "turn-abrupt-disconnect",
    ));
    let mut stream = provider_with_base_url(&store, base_url)
        .execute(
            planned_request("openai", operation),
            context("req_abrupt_disconnect", CancellationToken::new()),
        )
        .await
        .expect("prepare WebSocket provider stream");
    let error = loop {
        match stream.next().await {
            Some(Ok(_)) => {}
            Some(Err(error)) => break error,
            None => panic!("abrupt disconnect must surface a provider error"),
        }
    };
    server.await.unwrap();
    assert_eq!(error.kind(), ProviderErrorKind::Transport);
    assert_eq!(error.send_state(), UpstreamSendState::Ambiguous);
    assert!(!error.replay_is_safe());
    assert_eq!(error.pre_delivery_retry(), None);
    assert_eq!(
        error.diagnostic().map(|diagnostic| diagnostic.as_str()),
        Some(
            "OpenAI WebSocket disconnected without a closing handshake after payload send; result is ambiguous"
        ),
    );
}

#[tokio::test]
async fn websocket_idle_timeout_diagnosis_survives_ambiguous_send_wrapping() {
    const ACCOUNT_ID: &str = "acct_abrupt_disconnect";
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, ACCOUNT_ID).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server =
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut websocket = accept_codex_test_websocket(stream).await;
            websocket.next().await.unwrap().unwrap();
            websocket.send(Message::Text(json!({
            "type": "response.created", "response": {"id": "resp_idle", "model": "gpt-5.4"},
        }).to_string().into())).await.unwrap();
            websocket
                .send(Message::Text(
                    json!({"type": "response.output_text.delta", "delta": "partial"})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            futures::future::pending::<()>().await;
        });
    let operation = Operation::Generate(generate_with_persisted_session_context(
        ACCOUNT_ID,
        "conversation-idle-timeout",
        "session-idle-timeout",
        "turn-idle-timeout",
    ));
    let mut stream = provider_with_base_url(&store, base_url)
        .execute(
            planned_request("openai", operation),
            context("req_idle_timeout", CancellationToken::new()),
        )
        .await
        .expect("prepare WebSocket provider stream");
    loop {
        let event = stream
            .next()
            .await
            .expect("partial response")
            .expect("valid partial response");
        if event.has_client_event() {
            break;
        }
    }
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(300)).await;
    let error = loop {
        match stream.next().await {
            Some(Ok(_)) => {}
            Some(Err(error)) => break error,
            None => panic!("idle timeout must surface a provider error"),
        }
    };
    server.abort();
    assert_eq!(error.send_state(), UpstreamSendState::Ambiguous);
    assert!(!error.replay_is_safe());
    assert_eq!(error.pre_delivery_retry(), None);
    assert_eq!(
        error.diagnostic().and_then(|diagnostic| diagnostic.code()),
        Some("receive_idle_timeout")
    );
    assert_eq!(
        error.diagnostic().and_then(|diagnostic| diagnostic.stage()),
        Some("receive")
    );
    assert_eq!(
        error.diagnostic().map(|diagnostic| diagnostic.as_str()),
        Some("OpenAI WebSocket receive idle timeout after 300s"),
    );
}

#[tokio::test]
async fn ambiguous_websocket_close_reconnects_and_retains_native_continuation() {
    const ACCOUNT_ID: &str = "acct_websocket_close";
    const CONVERSATION_ID: &str = "conversation-websocket-ambiguous-close";
    const SESSION_ID: &str = "sticky-websocket-session";

    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, ACCOUNT_ID).await;
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

        let (stream, _) = listener
            .accept()
            .await
            .expect("accept new pooled WebSocket");
        let mut websocket = accept_codex_test_websocket(stream).await;
        let _request = websocket
            .next()
            .await
            .expect("fresh WebSocket request")
            .expect("valid fresh WebSocket request");
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.created",
                    "response": {"id": "resp_fresh_retry", "model": "gpt-5.4"}
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("start fresh WebSocket response");
        websocket
            .send(completed_response("resp_fresh_retry"))
            .await
            .expect("complete fresh WebSocket request");

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
            Some(&json!("resp_fresh_retry"))
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
    let operation = Operation::Generate(generate_with_persisted_session_context(
        ACCOUNT_ID,
        CONVERSATION_ID,
        SESSION_ID,
        "thread-first",
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
    let connection = error
        .connection_observation()
        .expect("normal close should retain its connection lifecycle observation");
    assert_eq!(connection.exit_reason(), "normal_close");
    assert!(connection.age_ms() >= connection.idle_ms());

    let second_operation = Operation::Generate(generate_with_persisted_session_context(
        ACCOUNT_ID,
        CONVERSATION_ID,
        SESSION_ID,
        "thread-second",
    ));
    let mut fresh_stream = provider
        .execute(
            planned_request("openai", second_operation),
            context("req_websocket_fresh_retry", CancellationToken::new()),
        )
        .await
        .expect("same session should prepare a fresh WebSocket stream");
    assert_eq!(fresh_stream.metadata().transport().as_str(), "websocket");
    let mut fresh_pool_observation = None;
    let mut recovered_session = None;
    while let Some(event) = fresh_stream.next().await {
        let event = event.expect("fresh WebSocket response");
        if let Some(update) = event.session_update() {
            recovered_session = Some(update.clone());
        }
        if let Some(observation) = event.response_observation()
            && observation.transport().as_str() == "websocket"
        {
            fresh_pool_observation = Some(observation.websocket_pool());
        }
    }
    assert_eq!(fresh_pool_observation, Some(Some(WebSocketPoolKind::New)));
    let recovered_session = recovered_session.expect("recovered session");
    assert_eq!(
        recovered_session.payload().get("continuation_scope"),
        Some(&json!("connection_local"))
    );
    drop(fresh_stream);

    let continuation_operation = Operation::Generate(
        generate_with_session_context(
            "sticky-websocket-session",
            Some("thread-continuation"),
            None,
        )
        .with_provider_session_state(recovered_session),
    );
    let mut continuation_stream = provider
        .execute(
            planned_request("openai", continuation_operation),
            pinned_continuation_context(
                "req_recovered_continuation",
                ACCOUNT_ID,
                "resp_fresh_retry",
                "resp_fresh_retry",
                1,
                ContinuationAttempt::Native,
            ),
        )
        .await
        .expect("recovered pooled socket must support native continuation");
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
        .expect("required warmup must remain on WebSocket");
    assert_eq!(warmup_stream.metadata().transport().as_str(), "websocket");
    drop(warmup_stream);
    server.await.expect("WebSocket and HTTP server");
}

#[tokio::test]
async fn repeated_websocket_failures_exhaust_budget_then_use_http_and_report_http_failure() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_websocket_close").await;
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let base_url = format!(
        "http://{}",
        listener.local_addr().expect("listener address")
    );
    let server = tokio::spawn(async move {
        for code in [1000, 1001, 1011] {
            let (stream, _) = listener.accept().await.expect("accept websocket");
            let mut websocket = accept_codex_test_websocket(stream).await;
            websocket
                .next()
                .await
                .expect("request")
                .expect("valid frame");
            websocket
                .close(Some(CloseFrame {
                    code: CloseCode::from(code),
                    reason: "".into(),
                }))
                .await
                .expect("close websocket");
        }
        for status in [200, 503] {
            let (mut http, _) = listener.accept().await.expect("accept HTTP fallback");
            assert!(
                String::from_utf8_lossy(&capture_http_request(&mut http).await)
                    .starts_with("POST /codex/responses")
            );
            let body = if status == 200 {
                CAPTURE_COMPLETED_SSE
            } else {
                r#"{"error":{"code":"server_error","message":"unavailable"}}"#
            };
            http.write_all(format!("HTTP/1.1 {status} Response\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.expect("HTTP response");
        }
    });
    let provider = provider_with_base_url_and_retry_budget(&store, base_url, 2);
    for index in 0..3 {
        let operation = Operation::Generate(generate_with_persisted_session_context(
            "acct_websocket_close",
            "conversation-repeated-close",
            "repeated-close",
            "turn",
        ));
        let mut stream = provider
            .execute(
                planned_request("openai", operation),
                context(
                    &format!("req_repeated_close_{index}"),
                    CancellationToken::new(),
                ),
            )
            .await
            .expect("prepare stream");
        assert_eq!(stream.metadata().transport().as_str(), "websocket");
        let error = loop {
            match stream.next().await {
                Some(Ok(_)) => {}
                Some(Err(error)) => break error,
                None => panic!("close must fail"),
            }
        };
        assert_eq!(error.send_state(), UpstreamSendState::Ambiguous);
        assert_eq!(error.pre_delivery_retry(), None);
        assert!(!error.replay_is_safe());
    }
    for index in 0..2 {
        let operation = Operation::Generate(generate_with_persisted_session_context(
            "acct_websocket_close",
            "conversation-repeated-close",
            "repeated-close",
            "turn",
        ));
        let mut stream = provider
            .execute(
                planned_request("openai", operation),
                context(
                    &format!("req_http_after_ws_{index}"),
                    CancellationToken::new(),
                ),
            )
            .await
            .expect("prepare HTTP");
        assert_eq!(stream.metadata().transport().as_str(), "http_sse");
        let mut failure = None;
        while let Some(event) = stream.next().await {
            if let Err(error) = event {
                failure = Some(error);
                break;
            }
        }
        if index == 0 {
            assert!(failure.is_none());
        } else {
            let failure = failure.expect("HTTP failure is final transport result");
            assert_eq!(failure.upstream_status(), Some(503));
            assert!(!matches!(
                failure.pre_delivery_retry(),
                Some(
                    PreDeliveryRetry::SameAccountTransportRetry { .. }
                        | PreDeliveryRetry::SameAccountTransportFallback
                )
            ));
        }
    }
    server.await.expect("server");
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
    tokio::time::pause();
    for index in 0..3 {
        tokio::time::advance(Duration::from_secs(5 * 60 * 60)).await;
        let stream = provider
            .execute(
                planned_request("openai", operation()),
                context(
                    &format!("req_active_http_session_{index}"),
                    CancellationToken::new(),
                ),
            )
            .await
            .expect("prepare active session");
        assert_eq!(stream.metadata().transport().as_str(), "http_sse");
    }
    let unrelated = provider
        .execute(
            planned_request(
                "openai",
                Operation::Generate(generate_with_session_context(
                    "unrelated-session",
                    None,
                    None,
                )),
            ),
            context("req_unrelated_session", CancellationToken::new()),
        )
        .await
        .expect("prepare unrelated session");
    assert_eq!(unrelated.metadata().transport().as_str(), "websocket");
    tokio::time::advance(Duration::from_secs(8 * 60 * 60)).await;
    let expired = provider
        .execute(
            planned_request("openai", operation()),
            context("req_expired_http_session", CancellationToken::new()),
        )
        .await
        .expect("prepare expired session");
    assert_eq!(expired.metadata().transport().as_str(), "websocket");
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
async fn downstream_websocket_new_chain_should_override_session_and_attempt_http_fallback() {
    const SESSION_ID: &str = "downstream-required-session";

    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_http_sse_exhausted").await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut opening, _) = listener.accept().await.unwrap();
        let request = capture_http_request(&mut opening).await;
        assert!(String::from_utf8_lossy(&request).starts_with("GET /codex/responses"));
        opening
            .write_all(
                b"HTTP/1.1 426 Upgrade Required\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            )
            .await
            .unwrap();

        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        for id in ["resp_required_session", "resp_required_attempt"] {
            let request = websocket.next().await.unwrap().unwrap();
            let payload: Value = serde_json::from_str(request.to_text().unwrap()).unwrap();
            assert_eq!(payload["store"], false);
            websocket
                .send(Message::Text(
                    json!({
                        "type": "response.completed",
                        "response": {
                            "id": id,
                            "model": "gpt-5.4",
                            "status": "completed",
                            "store": false,
                            "output": [],
                            "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
        }
    });
    let provider = provider_with_base_url(&store, base_url);
    let mut seed = provider
        .execute(
            planned_request(
                "openai",
                Operation::Generate(generate_with_session_context(SESSION_ID, None, None)),
            ),
            context("req_disable_optional_websocket", CancellationToken::new()),
        )
        .await
        .unwrap();
    let error = loop {
        match seed.next().await {
            Some(Ok(_)) => {}
            Some(Err(error)) => break error,
            None => panic!("426 opening must enable session HTTP fallback"),
        }
    };
    assert_eq!(
        error.pre_delivery_retry(),
        Some(PreDeliveryRetry::SameAccountTransportFallback)
    );
    drop(seed);

    for attempt in [
        context("req_required_session", CancellationToken::new()),
        fallback_transport_context("req_required_attempt"),
    ] {
        let payload = ProtocolPayload::json_object(
            "openai",
            Map::from_iter([
                ("model".to_owned(), json!("gpt-5.4")),
                ("input".to_owned(), json!("hello")),
                ("store".to_owned(), json!(false)),
                ("session_id".to_owned(), json!(SESSION_ID)),
            ]),
        )
        .unwrap()
        .with_context(Map::from_iter([(
            "downstream_websocket_connection_id".to_owned(),
            json!("ws_required_session"),
        )]));
        let request = GenerateRequest::from_protocol_payload(payload).with_provider_session_state(
            ProviderSessionState::new(
                "openai",
                Map::from_iter([
                    ("account_id".to_owned(), json!("acct_http_sse_exhausted")),
                    ("conversation_id".to_owned(), json!(SESSION_ID)),
                    ("continuation_scope".to_owned(), json!("persisted")),
                ]),
            )
            .unwrap(),
        );
        let mut stream = provider
            .execute(
                planned_request("openai", Operation::Generate(request)),
                attempt,
            )
            .await
            .unwrap();
        assert_eq!(stream.metadata().transport().as_str(), "websocket");
        let mut session = None;
        while let Some(event) = stream.next().await {
            let event = event.expect("required WebSocket response");
            if let Some(update) = event.session_update() {
                session = Some(update.clone());
            }
        }
        assert_eq!(
            session.unwrap().payload().get("continuation_scope"),
            Some(&json!("connection_local"))
        );
    }
    server.await.unwrap();
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
async fn websocket_turn_state_metadata_close_does_not_authorize_replay() {
    const ACCOUNT_ID: &str = "acct_websocket_metadata_close";
    const TURN_STATE: &str = "turn-state-before-close";

    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, ACCOUNT_ID).await;
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
                    "type": "codex.response.metadata",
                    "headers": {"x-codex-turn-state": TURN_STATE}
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("send response metadata");
        websocket
            .close(Some(CloseFrame {
                code: CloseCode::Normal,
                reason: "".into(),
            }))
            .await
            .expect("close before terminal response");
    });

    let operation = Operation::Generate(generate_with_persisted_session_context(
        ACCOUNT_ID,
        "conversation-metadata-close",
        "session-metadata-close",
        "turn-metadata-close",
    ));
    let mut stream = provider_with_base_url(&store, base_url)
        .execute(
            planned_request("openai", operation),
            context("req_websocket_metadata_close", CancellationToken::new()),
        )
        .await
        .expect("prepare WebSocket provider stream");
    let mut observed_turn_state = false;
    let mut session_update = None;
    let error = loop {
        match stream.next().await {
            Some(Ok(event)) => {
                if let Some(observation) = event.response_observation() {
                    observed_turn_state |= observation.client_headers().iter().any(|header| {
                        header.name().eq_ignore_ascii_case("x-codex-turn-state")
                            && header.value().as_ref() == TURN_STATE.as_bytes()
                    });
                }
                if let Some(update) = event.session_update() {
                    session_update = Some(update.clone());
                }
            }
            Some(Err(error)) => break error,
            None => panic!("metadata close must surface a provider error"),
        }
    };
    server.await.expect("WebSocket server");

    assert!(observed_turn_state);
    assert!(session_update.is_none());
    assert_eq!(error.send_state(), UpstreamSendState::Ambiguous);
    assert!(!error.replay_is_safe());
    assert_eq!(error.pre_delivery_retry(), None);
    assert_eq!(
        error.upstream_code().map(|code| code.as_str()),
        Some("websocket_close_1000")
    );
    assert_eq!(
        error.diagnostic().map(|diagnostic| diagnostic.as_str()),
        Some(
            "OpenAI WebSocket closed before a terminal response (close code 1000); last event type: codex.response.metadata"
        )
    );
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
    assert_eq!(
        body.get("installation_id"),
        body.pointer("/client_metadata/installation_id")
    );
    assert!(
        body.pointer("/client_metadata/x-codex-installation-id")
            .is_none()
    );
    assert!(captured_header_values(&request, "x-codex-installation-id").is_empty());
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
async fn websocket_account_scoping_preserves_ascii_turn_metadata_and_unicode_input() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_scope_same").await;
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let base_url = format!("http://{}", listener.local_addr().expect("address"));
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.expect("accept websocket");
        let mut socket = accept_codex_test_websocket(socket).await;
        let message = socket.next().await.expect("request").expect("valid frame");
        let body: Value = serde_json::from_str(message.to_text().expect("text")).expect("JSON");
        socket.send(Message::Text(json!({
            "type": "response.completed",
            "response": {"id": "resp_ascii_metadata", "model": "gpt-5.4", "status": "completed", "output": []}
        }).to_string().into())).await.expect("complete response");
        body
    });
    // Official Codex keeps embedded turn metadata ASCII even for Unicode workspaces.
    let raw = r#"{"installation_id":"client-installation","workspaces":{"C:\\Users\\\u9879\u76ee\\\ud83d\ude80":{"label":"caf\u00e9","literal":"\\u4e2d","quoted":"\"line\n"}}}"#;
    let input = json!([{"role": "user", "content": "中文正文 🚀"}]);
    let payload = ProtocolPayload::json_object(
        "openai",
        Map::from_iter([
            ("model".to_owned(), json!("gpt-5.4")),
            ("input".to_owned(), input.clone()),
            (
                "client_metadata".to_owned(),
                json!({"x-codex-turn-metadata": raw}),
            ),
        ]),
    )
    .expect("payload")
    .with_context(Map::from_iter([("turn_metadata".to_owned(), json!(raw))]));
    let mut stream = provider_with_base_url(&store, base_url)
        .execute(
            planned_request(
                "openai",
                Operation::Generate(GenerateRequest::from_protocol_payload(payload)),
            ),
            context_with_state_owner("req_ascii_metadata", "acct_scope_same"),
        )
        .await
        .expect("provider stream");
    while let Some(event) = stream.next().await {
        event.expect("successful websocket response");
    }
    let body = server.await.expect("server");
    let encoded = body
        .pointer("/client_metadata/x-codex-turn-metadata")
        .and_then(Value::as_str)
        .expect("turn metadata");
    assert!(encoded.is_ascii(), "embedded header JSON must remain ASCII");
    let mut expected: Value = serde_json::from_str(raw).expect("original metadata");
    expected["installation_id"] = body["client_metadata"]["installation_id"].clone();
    assert_eq!(
        serde_json::from_str::<Value>(encoded).expect("metadata JSON"),
        expected
    );
    assert_eq!(body["input"], input);
}

#[tokio::test]
async fn http_account_scoping_keeps_unicode_metadata_ascii_in_headers_and_body() {
    for owner in ["acct_scope_same", "acct_scope_old"] {
        let raw = r#"{"installation_id":"client-installation","workspaces":{"/tmp/\u4e2d\u6587/\ud83d\ude80":{"label":"caf\u00e9"}}}"#;
        let request = capture_scoped_http_request(
            "req_ascii_http",
            "acct_scope_same",
            owner,
            Map::from_iter([
                ("model".to_owned(), json!("gpt-5.4")),
                ("input".to_owned(), json!("中文正文 🚀")),
                ("turnMetadata".to_owned(), json!(raw)),
                (
                    "client_metadata".to_owned(),
                    json!({"x-codex-turn-metadata": raw}),
                ),
            ]),
            Map::from_iter([("turn_metadata".to_owned(), json!(raw))]),
        )
        .await;
        let body = captured_request_body(&request);
        let headers = captured_header_values(&request, "x-codex-turn-metadata");
        assert_eq!(headers.len(), 1);
        let mut expected: Value = serde_json::from_str(raw).expect("original metadata");
        expected["installation_id"] = body["client_metadata"]["installation_id"].clone();
        for encoded in [
            std::str::from_utf8(&headers[0]).expect("UTF-8 header"),
            body["turnMetadata"].as_str().expect("body turn metadata"),
            body["client_metadata"]["x-codex-turn-metadata"]
                .as_str()
                .expect("client metadata"),
        ] {
            assert!(encoded.is_ascii(), "scoped header JSON must remain ASCII");
            assert_eq!(
                serde_json::from_str::<Value>(encoded).expect("metadata JSON"),
                expected
            );
        }
    }
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
async fn matching_turn_id_should_prefer_an_explicit_client_echo_over_saved_provider_state() {
    let request = capture_turn_state_request(
        "req_use_client_turn_state",
        Some("turn-same"),
        Some("turn-same"),
        Some("client-turn-state"),
    )
    .await;

    assert_eq!(
        captured_header_values(&request, "x-codex-turn-state"),
        vec![b"client-turn-state".to_vec()]
    );
}

#[tokio::test]
async fn new_or_unidentified_turn_should_not_restore_previous_turn_state() {
    for (request_id, previous_turn_id, current_turn_id, client_turn_state) in [
        (
            "req_new_turn_state",
            Some("turn-old"),
            Some("turn-new"),
            Some("stale-client-turn-state"),
        ),
        ("req_unidentified_turn_state", None, Some("turn-new"), None),
        (
            "req_missing_current_turn_state",
            Some("turn-old"),
            None,
            None,
        ),
    ] {
        let request = capture_turn_state_request(
            request_id,
            previous_turn_id,
            current_turn_id,
            client_turn_state,
        )
        .await;
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
async fn thread_spawn_children_should_inherit_the_root_with_independent_scoped_bindings() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_thread_spawn_affinity").await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let provider = provider_with_affinity(&store, Arc::clone(&affinity));
    let thread_spawn = r#"{"subagent_kind":"thread_spawn"}"#;
    let client_key = ClientApiKeyId::new("key_openai_contract").expect("client key");
    let mut keys = Vec::new();

    for (request_id, thread_id, turn_metadata) in [
        ("req_thread_spawn_parent", None, None),
        ("req_thread_spawn_root_thread", Some("parent-session"), None),
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
    ] {
        let operation = Operation::Generate(generate_with_session_context(
            "parent-session",
            thread_id,
            turn_metadata,
        ));
        keys.push(
            provider
                .request_observation(&operation, &client_key)
                .continuation
                .affinity_hash
                .expect("affinity hash"),
        );
        let stream = provider
            .execute(
                planned_request("openai", operation),
                context(request_id, CancellationToken::new()),
            )
            .await
            .expect("prepare provider stream");
        assert_eq!(
            stream.metadata().provider_account_id().as_str(),
            "acct_thread_spawn_affinity"
        );
        drop(stream);
    }

    assert_eq!(keys[0], keys[1], "root thread uses the existing root key");
    assert_eq!(
        keys[0], keys[5],
        "unidentified thread retains root fallback"
    );
    assert_ne!(keys[0], keys[2]);
    assert_ne!(keys[2], keys[3]);
    assert_eq!(keys[2], keys[4], "same child reuses its own key");
    assert_eq!(affinity.binding_count(), 3);
    for (root, client_key) in [
        ("other-root", client_key),
        (
            "parent-session",
            ClientApiKeyId::new("other-client").expect("client key"),
        ),
    ] {
        let operation = Operation::Generate(generate_with_session_context(
            root,
            Some("child-one"),
            Some(thread_spawn),
        ));
        assert_ne!(
            provider
                .request_observation(&operation, &client_key)
                .continuation
                .affinity_hash
                .as_ref(),
            Some(&keys[2]),
            "child identity stays scoped to both root and client"
        );
    }
}

#[tokio::test]
async fn child_busy_failover_should_leave_the_running_parent_and_siblings_on_the_root_account() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_subagent_a").await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let leases = Arc::new(TestLeaseCoordinator::default());
    let operation = |thread_id: &str| {
        Operation::Generate(GenerateRequest::from_protocol_payload(
            ProtocolPayload::json_object(
                "openai",
                Map::from_iter([
                    ("model".to_owned(), json!("gpt-5.4")),
                    (
                        "input".to_owned(),
                        json!([{"role":"user", "content":"keep the complete transcript"}]),
                    ),
                    ("session_id".to_owned(), json!("parent-session")),
                    ("thread_id".to_owned(), json!(thread_id)),
                    ("turnState".to_owned(), json!("old-account-turn-state")),
                ]),
            )
            .expect("payload")
            .with_context(Map::from_iter([("use_websocket".to_owned(), json!(false))])),
        ))
    };
    let created = "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_scope_capture\",\"status\":\"in_progress\"}}\n\n";
    let (parent_url, release_parent, parent_started, parent_server) =
        paused_chunked_sse_server(created.to_owned(), CAPTURE_COMPLETED_SSE.to_owned()).await;
    let parent_provider = provider_with_affinity_and_base_url_and_leases(
        &store,
        Arc::clone(&affinity),
        parent_url,
        Arc::clone(&leases),
    );
    let mut parent = parent_provider
        .execute(
            planned_request("openai", operation("parent-session")),
            context("req_parent_running", CancellationToken::new()),
        )
        .await
        .expect("parent selection");
    let root_account = parent.metadata().provider_account_id().clone();
    let parent_task = tokio::spawn(async move {
        let mut completed = false;
        while let Some(event) = parent.next().await {
            let event = event.expect("parent remains successful on its original account");
            completed |= event
                .canonical_facts()
                .iter()
                .any(|event| matches!(event, GatewayEvent::Completed(_)));
        }
        assert!(completed, "parent completion must be observed");
    });
    timeout(Duration::from_secs(5), parent_started)
        .await
        .expect("parent start timeout")
        .expect("parent started");

    create_account(&store, "acct_subagent_b").await;
    store.set_scheduling(
        "acct_subagent_b",
        None,
        AccountWeight::new(100).expect("weight"),
    );
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            format!("{created}{CAPTURE_COMPLETED_SSE}"),
            "text/event-stream",
        ))
        .expect(2)
        .mount(&server)
        .await;
    let provider = provider_with_affinity_and_base_url_and_leases(
        &store,
        Arc::clone(&affinity),
        server.uri(),
        Arc::clone(&leases),
    );
    let child = provider
        .execute(
            planned_request("openai", operation("child-one")),
            context("req_child_inherit", CancellationToken::new()),
        )
        .await
        .expect("initial child selection");
    assert_eq!(
        child.metadata().provider_account_id(),
        &root_account,
        "root preference outranks B's weight"
    );
    drop(child);

    // 租约层报告 A 已满；既有子线程和首次出现的子线程都能独立选择 B。
    leases
        .busy_accounts
        .lock()
        .expect("busy accounts")
        .insert(root_account.clone());
    for thread in ["child-one", "cold-child"] {
        let mut child = provider
            .execute(
                planned_request("openai", operation(thread)),
                context("req_child_busy", CancellationToken::new()),
            )
            .await
            .expect("child busy fallback");
        assert_eq!(
            child.metadata().provider_account_id().as_str(),
            "acct_subagent_b"
        );
        let mut completed = false;
        while let Some(event) = child.next().await {
            let event = event.expect("child completes on B");
            completed |= event
                .canonical_facts()
                .iter()
                .any(|event| matches!(event, GatewayEvent::Completed(_)));
        }
        assert!(completed, "child completion must be observed");
    }
    assert!(
        !parent_task.is_finished(),
        "child failover must not interrupt the active parent"
    );
    release_parent
        .send(())
        .expect("release parent after child success");
    timeout(Duration::from_secs(5), parent_task)
        .await
        .expect("parent completion timeout")
        .expect("parent task");
    parent_server.await.expect("parent server");
    leases.busy_accounts.lock().expect("busy accounts").clear();

    for (thread, expected) in [
        ("parent-session", "acct_subagent_a"),
        ("child-one", "acct_subagent_b"),
        ("cold-child", "acct_subagent_b"),
        ("new-sibling", "acct_subagent_a"),
    ] {
        let stream = provider
            .execute(
                planned_request("openai", operation(thread)),
                context("req_after_parent_completion", CancellationToken::new()),
            )
            .await
            .expect("selection after parent completion");
        assert_eq!(
            stream.metadata().provider_account_id().as_str(),
            expected,
            "thread {thread}"
        );
        drop(stream);
    }
    assert_eq!(affinity.binding_count(), 4);
    let requests = server.received_requests().await.expect("child requests");
    assert_eq!(requests.len(), 2);
    for request in requests {
        assert!(captured_header_values(&request, "x-codex-turn-state").is_empty());
        assert_eq!(
            captured_request_body(&request).get("input"),
            Some(&json!([{"role":"user", "content":"keep the complete transcript"}]))
        );
        assert_eq!(
            request
                .headers
                .get("chatgpt-account-id")
                .and_then(|header| header.to_str().ok()),
            Some("chatgpt-acct_subagent_b")
        );
    }
    server.verify().await;
}

#[tokio::test]
async fn failed_child_failover_should_preserve_both_existing_bindings() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_subagent_a").await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let leases = Arc::new(TestLeaseCoordinator::default());
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/alpha/search"))
        .respond_with(
            ResponseTemplate::new(500).set_body_json(json!({"error":{"message":"upstream busy"}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let provider = provider_with_affinity_and_base_url_and_leases(
        &store,
        Arc::clone(&affinity),
        server.uri(),
        Arc::clone(&leases),
    );
    for thread in [None, Some("child")] {
        let stream = provider
            .execute(
                planned_request(
                    "openai",
                    Operation::Generate(generate_with_session_context("root", thread, None)),
                ),
                context("req_seed_child_failure", CancellationToken::new()),
            )
            .await
            .expect("initial binding");
        assert_eq!(
            stream.metadata().provider_account_id().as_str(),
            "acct_subagent_a"
        );
        drop(stream);
    }
    create_account(&store, "acct_subagent_b").await;
    leases
        .busy_accounts
        .lock()
        .expect("busy accounts")
        .insert(ProviderAccountId::new("acct_subagent_a").expect("account ID"));
    let search = Operation::Search(StandaloneSearchRequest::from_raw_json(
        RawJsonPayload::new(
            "openai",
            Bytes::from_static(br#"{"id":"root","thread_id":"child","commands":{}}"#),
        )
        .expect("search payload"),
    ));
    let mut child = provider
        .execute(
            planned_provider_endpoint_request("openai", search),
            context("req_failed_child", CancellationToken::new()),
        )
        .await
        .expect("child fallback selection");
    assert_eq!(
        child.metadata().provider_account_id().as_str(),
        "acct_subagent_b"
    );
    let error = loop {
        match child.next().await {
            Some(Err(error)) => break error,
            Some(Ok(_)) => {}
            None => panic!("expected upstream failure"),
        }
    };
    assert_eq!(error.upstream_status(), Some(500));
    drop(child);
    leases.busy_accounts.lock().expect("busy accounts").clear();
    for thread in [None, Some("child")] {
        let stream = provider
            .execute(
                planned_request(
                    "openai",
                    Operation::Generate(generate_with_session_context("root", thread, None)),
                ),
                context("req_after_child_failure", CancellationToken::new()),
            )
            .await
            .expect("binding after failure");
        assert_eq!(
            stream.metadata().provider_account_id().as_str(),
            "acct_subagent_a",
            "failure must not migrate either binding"
        );
        drop(stream);
    }
    assert_eq!(affinity.binding_count(), 2);
    server.verify().await;
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
    assert_eq!(observed_service_tier.as_deref(), Some("default"));
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
        // 失败后的补查只更新观察时间，即使响应满足恢复条件，
        // 也不能覆盖本次推理刚确认的耗尽；恢复由独立的主动刷新判断。
        .respond_with(
            ResponseTemplate::new(200)
                // 验证后台 usage 同步不能把原始的额度错误响应拖到查询完成之后。
                .set_delay(Duration::from_millis(750))
                .set_body_json(json!({
                    "rate_limit": {
                        "allowed": true,
                        "limit_reached": false,
                        "primary_window": {"used_percent": 0, "reset_at": reset_at + 18_000},
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
            let observations = store
                .get_quotas(&[account.id().clone()])
                .await
                .expect("quota observations");
            if observations
                .first()
                .is_some_and(|observation| observation.observed_at > projected_observed_at)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("background usage refresh must complete");
    let observation = store
        .get_quotas(&[account.id().clone()])
        .await
        .expect("read refreshed quota")
        .into_iter()
        .next()
        .expect("authoritative quota projection");
    assert!(observation.observed_at > projected_observed_at);
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
        "a failure follow-up must preserve access even when the reset recovery condition is met"
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

    let client_key_id = ClientApiKeyId::new("key_openai_observation").expect("client key");
    let observation = provider(&store).request_observation(&operation, &client_key_id);

    assert_eq!(observation.request_kind.as_deref(), Some("review"));
    assert_eq!(observation.subagent_kind.as_deref(), Some("worker"));
    assert!(!observation.compact);
    assert!(matches!(operation, Operation::Generate(_)));
}

#[test]
fn request_observation_classifies_prewarm_from_generate_without_rewriting_the_payload() {
    let store = Arc::new(MemoryAccountStore::default());
    let provider = provider(&store);
    let client_key_id = ClientApiKeyId::new("key_prewarm_observation").expect("client key");
    for (generate, request_kind, expected_kind) in [
        (Some(json!(false)), None, Some("prewarm")),
        (Some(json!(false)), Some("prewarm"), Some("prewarm")),
        (Some(json!(false)), Some("review"), Some("prewarm")),
        (Some(json!(true)), Some("prewarm"), None),
        (None, Some("prewarm"), None),
        (Some(json!(null)), Some("prewarm"), None),
        (Some(json!("false")), Some("prewarm"), None),
        (None, Some("review"), Some("review")),
        (None, None, None),
    ] {
        let mut body = Map::from_iter([
            ("model".to_owned(), json!("gpt-test")),
            ("input".to_owned(), json!("hello")),
            ("store".to_owned(), json!(false)),
        ]);
        if let Some(generate) = generate {
            body.insert("generate".to_owned(), generate);
        }
        let payload = ProtocolPayload::json_object("openai", body.clone())
            .expect("OpenAI payload")
            .with_context(Map::from_iter([(
                "turn_metadata".to_owned(),
                Value::String(
                    json!({"request_kind": request_kind, "subagent_kind": "worker"}).to_string(),
                ),
            )]));
        let operation = Operation::Generate(GenerateRequest::from_protocol_payload(payload));

        let observation = provider.request_observation(&operation, &client_key_id);

        assert_eq!(
            observation.request_kind.as_deref(),
            expected_kind,
            "generate={:?}, request_kind={request_kind:?}",
            body.get("generate"),
        );
        assert_eq!(observation.subagent_kind.as_deref(), Some("worker"));
        let Operation::Generate(generation) = operation else {
            unreachable!();
        };
        assert_eq!(generation.protocol_payload().body(), &body);
    }
}

#[test]
fn endpoint_observation_should_read_models_without_rewriting_or_requiring_a_catalog() {
    let store = Arc::new(MemoryAccountStore::default());
    let provider = provider(&store);
    let client_key = ClientApiKeyId::new("key_endpoint_model").expect("client key");
    for model in [
        json!("gpt-image-future"),
        json!("gpt-5.6-sol"),
        json!(null),
        json!(42),
        json!(""),
    ] {
        let body = serde_json::to_vec(&json!({"model":model,"future":9007199254740993_u64}))
            .expect("body");
        let payload =
            RawJsonPayload::new("openai", Bytes::copy_from_slice(&body)).expect("payload");
        let operations = [
            Operation::GenerateImage(ImageRequest::from_raw_json(
                ImageRequestKind::Generation,
                payload.clone(),
            )),
            Operation::GenerateImage(ImageRequest::from_raw_json(
                ImageRequestKind::Edit,
                payload.clone(),
            )),
            Operation::Search(StandaloneSearchRequest::from_raw_json(payload)),
        ];
        for operation in operations {
            let observation = provider.request_observation(&operation, &client_key);
            assert_eq!(
                observation
                    .requested_model
                    .as_ref()
                    .map(PublicModelId::as_str),
                model.as_str().filter(|model| !model.is_empty()),
            );
            let payload = match &operation {
                Operation::GenerateImage(request) => request.payload(),
                Operation::Search(request) => request.payload(),
                _ => unreachable!(),
            };
            assert_eq!(payload.body().as_ref(), body.as_slice());
        }
    }
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

    let client_key_id = ClientApiKeyId::new("key_openai_observation").expect("client key");
    let observation = provider(&store).request_observation(&operation, &client_key_id);

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

    let client_key_id = ClientApiKeyId::new("key_openai_observation").expect("client key");
    let _observation = provider(&store).request_observation(&operation, &client_key_id);

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
    assert_eq!(presentation.max_context_window_tokens(), Some(272_000));
    assert!(presentation.image_input());
    assert!(presentation.agent_tools());
    assert!(presentation.parallel_tool_calls());
    assert!(presentation.search_tool());
    assert!(presentation.image_detail_original());
    assert!(presentation.verbosity());
    assert!(!presentation.hidden());
}

#[tokio::test]
async fn provider_preserves_independent_catalog_context_windows() {
    for (context_window, max_context_window) in [
        (Some(272_000), Some(872_000)),
        (Some(272_000), None),
        (None, Some(872_000)),
        (None, None),
    ] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_context_windows").await;
        let server = MockServer::start().await;
        let mut model = json!({"slug": "gpt-5.6-terra", "display_name": "GPT-5.6-Terra"});
        if let Some(context_window) = context_window {
            model["context_window"] = json!(context_window);
        }
        if let Some(max_context_window) = max_context_window {
            model["max_context_window"] = json!(max_context_window);
        }
        Mock::given(method("GET"))
            .and(path("/codex/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"models": [model]})))
            .expect(1)
            .mount(&server)
            .await;
        let provider = provider_with_base_url(&store, server.uri());

        let capabilities = provider
            .query_model_capabilities()
            .await
            .expect("catalog model capabilities");
        let presentation = capabilities[0].presentation().expect("model presentation");

        assert_eq!(
            (
                presentation.context_window_tokens(),
                presentation.max_context_window_tokens(),
            ),
            (context_window, max_context_window),
        );
    }
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

#[tokio::test]
async fn completed_websocket_response_resets_consecutive_failure_budget() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_websocket_close").await;
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let base_url = format!("http://{}", listener.local_addr().expect("address"));
    let server = tokio::spawn(async move {
        let mut active = None;
        for succeeds in [false, true, false, false, true] {
            if active.is_none() {
                let (stream, _) = listener.accept().await.expect("accept WS");
                active = Some(accept_codex_test_websocket(stream).await);
            }
            let ws = active.as_mut().expect("active connection");
            ws.next().await.expect("request").expect("valid frame");
            if succeeds {
                ws.send(Message::Text(json!({"type":"response.created","response":{"id":"resp_budget_reset","model":"gpt-5.4"}}).to_string().into())).await.expect("created response");
                ws.send(Message::Text(json!({"type":"response.completed","response":{"id":"resp_budget_reset","model":"gpt-5.4","status":"completed","output":[]}}).to_string().into())).await.expect("complete response");
            } else {
                ws.close(None).await.expect("close connection");
                active = None;
            }
        }
    });
    let provider = provider_with_base_url_and_retry_budget(&store, base_url, 2);
    for (index, succeeds) in [false, true, false, false, true].into_iter().enumerate() {
        let operation = Operation::Generate(generate_with_persisted_session_context(
            "acct_websocket_close",
            "conversation-budget-reset",
            "budget-reset",
            "turn",
        ));
        let mut stream = provider
            .execute(
                planned_request("openai", operation),
                context(
                    &format!("req_reset_budget_{index}"),
                    CancellationToken::new(),
                ),
            )
            .await
            .expect("prepare request");
        assert_eq!(
            stream.metadata().transport().as_str(),
            "websocket",
            "request {index}"
        );
        let mut failed = false;
        let mut completed = false;
        while let Some(event) = stream.next().await {
            if let Ok(event) = &event {
                completed |= event
                    .canonical_facts()
                    .iter()
                    .any(|fact| matches!(fact, GatewayEvent::Completed(_)));
            }
            if event.is_err() {
                failed = true;
                break;
            }
        }
        assert_eq!(failed, !succeeds);
        assert_eq!(completed, succeeds);
    }
    server.await.expect("server");
}

#[tokio::test]
async fn connection_limit_rejection_requests_one_provider_retry_and_reconnects_in_pool() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_websocket_close").await;
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let base_url = format!("http://{}", listener.local_addr().expect("address"));
    let server = tokio::spawn(async move {
        for first in [true, false] {
            let (stream, _) = listener.accept().await.expect("accept WS");
            let mut ws = accept_codex_test_websocket(stream).await;
            ws.next().await.expect("request").expect("valid frame");
            let event = if first {
                json!({"type":"error","status":400,"headers":{"x-request-id":"req-limit-retry"},"error":{"code":"websocket_connection_limit_reached","type":"invalid_request_error","message":"connection expired"}})
            } else {
                json!({"type":"response.completed","response":{"id":"resp_reconnected","model":"gpt-5.4","status":"completed","output":[]}})
            };
            if !first {
                ws.send(Message::Text(json!({"type":"response.created","response":{"id":"resp_reconnected","model":"gpt-5.4"}}).to_string().into())).await.expect("created response");
            }
            ws.send(Message::Text(event.to_string().into()))
                .await
                .expect("send response");
        }
    });
    let provider = provider_with_base_url_and_retry_budget(&store, base_url, 1);
    let operation = || {
        Operation::Generate(generate_with_persisted_session_context(
            "acct_websocket_close",
            "conversation-rejected",
            "rejected",
            "turn",
        ))
    };
    let mut stream = provider
        .execute(
            planned_request("openai", operation()),
            context("req_rejected", CancellationToken::new()),
        )
        .await
        .expect("prepare request");
    let error = loop {
        match stream.next().await {
            Some(Ok(_)) => {}
            Some(Err(error)) => break error,
            None => panic!("expected rejection"),
        }
    };
    assert_eq!(error.send_state(), UpstreamSendState::Sent);
    assert!(error.replay_is_safe());
    assert!(
        matches!(error.pre_delivery_retry(), Some(PreDeliveryRetry::SameAccountTransportRetry { retry_index, .. }) if retry_index.get() == 1)
    );
    assert_eq!(error.upstream_status(), Some(400));
    assert_eq!(
        error.upstream_request_id().map(|id| id.as_str()),
        Some("req-limit-retry")
    );
    let visible = error
        .client_visible_upstream_error()
        .expect("original limit error");
    assert_eq!(visible.message(), "connection expired");
    assert_eq!(visible.error_type(), Some("invalid_request_error"));
    assert!(
        error
            .raw_upstream_error()
            .expect("original frame")
            .as_str()
            .contains("req-limit-retry")
    );
    drop(stream);
    let retry_context = context("req_rejected", CancellationToken::new()).with_transport(
        AttemptTransport::Retry(NonZeroU32::new(1).expect("retry index")),
    );
    let mut retry = provider
        .execute(planned_request("openai", operation()), retry_context)
        .await
        .expect("prepare retry");
    let mut pooled = false;
    while let Some(event) = retry.next().await {
        let event = event.expect("retry succeeds");
        pooled |= event.response_observation().is_some_and(|observation| {
            observation.websocket_pool() == Some(WebSocketPoolKind::New)
        });
    }
    assert!(pooled);
    server.await.expect("server");
}

#[tokio::test]
async fn error_body_read_failure_preserves_http_sse_response_facts_without_replay() {
    assert_error_body_read_failure(false).await;
}

#[tokio::test]
async fn error_body_read_failure_preserves_http_json_response_facts_without_replay() {
    assert_error_body_read_failure(true).await;
}

async fn assert_error_body_read_failure(image: bool) {
    for status in [401, 429, 503] {
        for chunked in [false, true] {
            let store = Arc::new(MemoryAccountStore::default());
            create_account(&store, "acct_provider_contract").await;
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
            let base_url = format!("http://{}", listener.local_addr().expect("address"));
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.expect("accept HTTP");
                read_http_request(&mut stream).await;
                let framing = if chunked {
                    "transfer-encoding: chunked"
                } else {
                    "content-length: 4096"
                };
                stream
                    .write_all(format!(
                        "HTTP/1.1 {status} Error\r\n{framing}\r\ncontent-type: application/json\r\nx-request-id: req-body-truncated\r\nretry-after: 99\r\nconnection: close\r\n\r\n"
                    ).as_bytes())
                    .await
                    .expect("headers");
                if chunked {
                    stream.write_all(b"1000\r\n").await.expect("chunk size");
                }
                stream
                    .write_all(b"{\"error\":{\"message\":\"synthetic-private-body")
                    .await
                    .expect("partial body");
                stream.shutdown().await.expect("truncate response");
            });
            let provider = provider_with_base_url_and_retry_budget(&store, base_url, 0);
            let request = if image {
                let payload = RawJsonPayload::new(
                    "openai",
                    Bytes::from_static(br#"{"model":"gpt-image-2","prompt":"test"}"#),
                )
                .expect("image payload");
                planned_provider_endpoint_request(
                    "openai",
                    Operation::GenerateImage(ImageRequest::from_raw_json(
                        ImageRequestKind::Generation,
                        payload,
                    )),
                )
            } else {
                planned_request("openai", http_generate_operation())
            };
            let mut stream = provider
                .execute(
                    request,
                    context("req_body_read_failure", CancellationToken::new()),
                )
                .await
                .expect("prepare");
            let mut observation = None;
            let error = timeout(Duration::from_secs(5), async {
                loop {
                    match stream.next().await {
                        Some(Ok(event)) => {
                            if let Some(current) = event.response_observation() {
                                observation = Some(current.clone());
                            }
                        }
                        Some(Err(error)) => break error,
                        None => panic!("expected body read failure"),
                    }
                }
            })
            .await
            .expect("bounded body read");
            server.await.expect("server");
            assert_eq!(
                error.upstream_status(),
                Some(status),
                "image={image}, chunked={chunked}"
            );
            assert_eq!(
                error.upstream_request_id().map(|id| id.as_str()),
                Some("req-body-truncated")
            );
            assert_eq!(error.kind(), ProviderErrorKind::Transport);
            assert_eq!(error.send_state(), UpstreamSendState::Ambiguous);
            assert!(!error.replay_is_safe());
            assert!(error.pre_delivery_retry().is_none());
            assert!(error.retry_after().is_none());
            assert!(error.client_visible_upstream_response().is_none());
            assert!(error.raw_upstream_error().is_none());
            let diagnostic = error.diagnostic().expect("body read diagnostic");
            assert_eq!(diagnostic.stage(), Some("receive"));
            assert_eq!(diagnostic.code(), Some("body_read_failed"));
            assert!(!diagnostic.as_str().contains("synthetic-private"));
            let observation = observation.expect("known HTTP response facts");
            assert_eq!(observation.status_code(), Some(status));
            assert_eq!(
                observation.request_id().map(|id| id.as_str()),
                Some("req-body-truncated")
            );
        }
    }
}

#[tokio::test]
async fn websocket_failure_headers_override_opening_id_without_changing_raw_events() {
    assert_websocket_failure_headers(
        json!({"X-Request-Id": "req-current-error", "authorization": "synthetic-private-token"}),
        Some("req-current-error"),
        false,
    )
    .await;
}

#[tokio::test]
async fn websocket_failure_without_valid_headers_does_not_claim_opening_request_id() {
    for headers in [
        Value::Null,
        json!({}),
        json!({"x-request-id": " "}),
        json!({"x-request-id": ["invalid"], "x-oai-request-id": "\r\ninvalid"}),
    ] {
        assert_websocket_failure_headers(headers, None, false).await;
    }
}

#[tokio::test]
async fn reused_websocket_response_failed_uses_current_request_id() {
    assert_websocket_failure_headers(
        json!({"x-request-id": "", "X-Oai-Request-Id": "req-reused-error"}),
        Some("req-reused-error"),
        true,
    )
    .await;
}

async fn assert_websocket_failure_headers(headers: Value, request_id: Option<&str>, reuse: bool) {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let base_url = format!("http://{}", listener.local_addr().expect("address"));
    let upstream_error = json!({
        "code":"invalid_request","type":"invalid_request_error","message":"synthetic failure"
    });
    let raw = if reuse {
        json!({
            "type":"response.failed", "status_code":400, "headers":headers,
            "response":{"id":"resp-failed","status":"failed","error":upstream_error}
        })
    } else {
        json!({"type":"error", "status":400, "headers":headers, "error":upstream_error})
    }
    .to_string();
    let server_raw = raw.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept WS");
        let mut ws = crate::transport::accept_codex_test_websocket_with(stream, |_, response| {
            response
                .headers_mut()
                .insert("x-request-id", "req-opening".parse().expect("ID"));
        })
        .await;
        if reuse {
            ws.next()
                .await
                .expect("first request")
                .expect("valid frame");
            ws.send(Message::Text(
                json!({
                    "type":"response.created","response":{"id":"resp-initial","model":"gpt-5.4"}
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("created");
            ws.send(Message::Text(json!({
                "type":"response.completed","response":{"id":"resp-initial","model":"gpt-5.4","status":"completed","output":[]}
            }).to_string().into())).await.expect("completed");
        }
        ws.next().await.expect("request").expect("valid frame");
        ws.send(Message::Text(
            json!({
                "type":"response.metadata","headers":{"x-request-id":"req-metadata"}
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("metadata");
        ws.send(Message::Text(server_raw.into()))
            .await
            .expect("error frame");
    });
    let provider = provider_with_base_url_and_retry_budget(&store, base_url, 0);
    let operation = || {
        Operation::Generate(generate_with_persisted_session_context(
            "acct_provider_contract",
            "conversation-error-headers",
            "session-error-headers",
            "thread",
        ))
    };
    if reuse {
        let mut first = provider
            .execute(
                planned_request("openai", operation()),
                context("req_ws_initial", CancellationToken::new()),
            )
            .await
            .expect("prepare first request");
        timeout(Duration::from_secs(5), async {
            while let Some(event) = first.next().await {
                event.expect("successful first request");
            }
        })
        .await
        .expect("bounded first request");
    }
    let mut stream = provider
        .execute(
            planned_request("openai", operation()),
            context("req_ws_headers", CancellationToken::new()),
        )
        .await
        .expect("prepare");
    let mut observation = None;
    let mut error = timeout(Duration::from_secs(5), async {
        loop {
            match stream.next().await {
                Some(Ok(event)) => {
                    if let Some(current) = event.response_observation() {
                        observation = Some(current.clone());
                    }
                }
                Some(Err(error)) => break error,
                None => panic!("expected error"),
            }
        }
    })
    .await
    .expect("bounded failure");
    server.await.expect("server");
    assert_eq!(
        error.upstream_request_id().map(|id| id.as_str()),
        request_id
    );
    let observation = observation.expect("failure observation");
    assert_eq!(observation.status_code(), Some(400));
    let metadata: Value = serde_json::from_str(
        observation
            .provider_metadata()
            .expect("provider facts")
            .as_json(),
    )
    .expect("metadata JSON");
    assert_eq!(metadata["websocketOpeningRequestId"], "req-opening");
    assert_eq!(observation.request_id().map(|id| id.as_str()), request_id);
    if reuse {
        assert_eq!(observation.websocket_pool(), Some(WebSocketPoolKind::Reuse));
    }
    assert_eq!(error.upstream_status(), Some(400));
    assert_eq!(error.kind(), ProviderErrorKind::InvalidRequest);
    assert_eq!(error.send_state(), UpstreamSendState::Sent);
    assert!(!error.replay_is_safe());
    assert_eq!(error.raw_upstream_error().expect("raw error").as_str(), raw);
    let events = error.take_atomic_client_events();
    let event_type = if reuse { "response.failed" } else { "error" };
    assert_eq!(
        events
            .iter()
            .filter_map(|event| event.wire_event()?.event_type())
            .collect::<Vec<_>>(),
        vec![event_type],
        "original failure stays atomically deliverable",
    );
    let wire = events
        .iter()
        .find_map(|event| event.wire_event()?.raw_sse_frame())
        .expect("raw frame");
    assert_eq!(
        wire.as_ref(),
        format!("event: {event_type}\ndata: {raw}\n\n").as_bytes()
    );
}

#[tokio::test]
async fn connection_limit_payload_survives_exhausted_retry_budget() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let base_url = format!("http://{}", listener.local_addr().expect("address"));
    let raw = r#"{ "type":"error", "status":400, "headers":{"x-request-id":"req-limit-final"}, "error":{"code":"websocket_connection_limit_reached","type":"invalid_request_error","message":"synthetic expired connection"}, "retry_after_seconds":99, "future":9007199254740993 }"#;
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept WS");
        let mut ws = accept_codex_test_websocket(stream).await;
        ws.next().await.expect("request").expect("valid frame");
        ws.send(Message::Text(raw.into()))
            .await
            .expect("limit frame");
        assert!(
            timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "Provider reports recovery intent, never sends a hidden retry"
        );
    });
    let provider = provider_with_base_url_and_retry_budget(&store, base_url, 0);
    let mut stream = provider
        .execute(
            planned_request("openai", generate_operation()),
            context("req_limit_final", CancellationToken::new()),
        )
        .await
        .expect("prepare");
    let mut observation = None;
    let error = timeout(Duration::from_secs(5), async {
        loop {
            match stream.next().await {
                Some(Ok(event)) => {
                    assert!(!event.has_client_event(), "lifetime preflight stays atomic");
                    if let Some(current) = event.response_observation() {
                        observation = Some(current.clone());
                    }
                }
                Some(Err(error)) => break error,
                None => panic!("expected limit rejection"),
            }
        }
    })
    .await
    .expect("bounded rejection");
    server.await.expect("server");
    assert_eq!(error.kind(), ProviderErrorKind::RateLimited);
    assert_eq!(error.send_state(), UpstreamSendState::Sent);
    assert!(error.replay_is_safe());
    assert!(matches!(
        error.pre_delivery_retry(),
        Some(PreDeliveryRetry::SameAccountTransportFallback)
    ));
    assert!(
        error.retry_after().is_none(),
        "raw retry hint must not alter existing lifetime recovery policy"
    );
    assert_eq!(error.upstream_status(), Some(400));
    assert_eq!(
        error.upstream_request_id().map(|id| id.as_str()),
        Some("req-limit-final")
    );
    assert_eq!(error.raw_upstream_error().expect("raw frame").as_str(), raw);
    let visible = error
        .client_visible_upstream_error()
        .expect("original structured error");
    assert_eq!(visible.message(), "synthetic expired connection");
    assert_eq!(visible.error_type(), Some("invalid_request_error"));
    assert!(
        !error
            .diagnostic()
            .expect("safe diagnostic")
            .as_str()
            .contains("synthetic expired")
    );
    assert!(error.client_visible_upstream_response().is_none());
    let observation = observation.expect("failure observation");
    assert_eq!(observation.status_code(), Some(400));
    assert_eq!(
        observation.request_id().map(|id| id.as_str()),
        Some("req-limit-final")
    );
}
