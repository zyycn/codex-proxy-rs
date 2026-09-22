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
use gateway_core::provider_ports::ProviderLeasePort;
use gateway_core::routing::{
    ClientRoutingScope, ConfigRevision, FrozenAccountScope, ModelCapabilities, ModelServiceTier,
    ProviderKind, ProviderModel, PublicModelId, RoutingContext, RuntimeAccount,
    RuntimeAccountDirectory, RuntimeSnapshot, UpstreamModelId,
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

#[tokio::test]
async fn responses_bill_sent_model_and_observe_unpriced_response_without_rewriting_it() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let server = MockServer::start().await;
    let body = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_model\",\"model\":\"gpt-created\"}}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_model\",\"model\":\"gpt-6-sol\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":100,\"output_tokens\":10,\"input_tokens_details\":{\"cached_tokens\":25},\"total_tokens\":110}}}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .expect(1)
        .mount(&server)
        .await;
    let provider = provider_with_base_url(&store, server.uri());
    let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            json!({"model":"gpt-5.6-sol","input":"hello"})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap()
        .with_context(Map::from_iter([("use_websocket".to_owned(), json!(false))])),
    ));
    let mut stream = provider
        .execute(
            planned_request("openai", operation),
            context("req_model", CancellationToken::new()),
        )
        .await
        .unwrap();
    let mut observed = None;
    let mut returned = None;
    let mut costs = Vec::new();
    while let Some(event) = stream.next().await {
        let event = event.expect("upstream response");
        for fact in event.canonical_facts() {
            if let GatewayEvent::CalculatedCost(cost) = fact {
                costs.push(cost.total().amount().scaled());
            }
        }
        if let Some(observation) = event.response_observation() {
            observed = observation.upstream_response_model().map(str::to_owned);
        }
        if let Some(wire) = event.wire_event()
            && wire.data().get("type").and_then(Value::as_str) == Some("response.completed")
        {
            returned = wire
                .data()
                .pointer("/response/model")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
    }
    let requests = server.received_requests().await.expect("upstream requests");
    assert_eq!(captured_request_body(&requests[0])["model"], "gpt-5.4");
    assert_eq!(costs, vec![3_437_500]);
    assert_eq!(observed.as_deref(), Some("gpt-6-sol"));
    assert_eq!(returned, observed);
}

#[tokio::test]
async fn websocket_bills_sent_model_and_observes_internal_model_report() {
    for metadata_type in ["response.metadata", "codex.response.metadata"] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_provider_contract").await;
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let base_url = format!("http://{}", listener.local_addr().expect("address"));
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("accept WebSocket");
            let mut websocket = accept_codex_test_websocket(socket).await;
            let request = websocket.next().await.expect("request").expect("frame");
            let request: Value =
                serde_json::from_str(request.to_text().expect("text")).expect("request JSON");
            assert_eq!(request["model"], "gpt-5.4");
            for event in [
                json!({"type":"response.created","response":{"id":"resp_model","model":"gpt-created"}}),
                json!({"type":metadata_type,"headers":{"X-OpenAI-Model":["gpt-server-report"]}}),
                json!({"type":"response.completed","response":{"id":"resp_model","model":"gpt-6-astra","status":"completed","output":[],"usage":{"input_tokens":100,"output_tokens":10,"input_tokens_details":{"cached_tokens":25},"total_tokens":110}}}),
            ] {
                websocket
                    .send(Message::Text(event.to_string().into()))
                    .await
                    .expect("response");
            }
        });
        let payload = ProtocolPayload::json_object(
            "openai",
            Map::from_iter([
                ("model".to_owned(), json!("gpt-5.6-sol")),
                ("input".to_owned(), json!("hello")),
            ]),
        )
        .expect("payload")
        .with_context(Map::from_iter([("use_websocket".to_owned(), json!(true))]));
        let mut stream = provider_with_base_url(&store, base_url)
            .execute(
                planned_request(
                    "openai",
                    Operation::Generate(GenerateRequest::from_protocol_payload(payload)),
                ),
                context("req_ws_model", CancellationToken::new()),
            )
            .await
            .expect("provider stream");
        let mut observed = None;
        let mut returned = None;
        let mut costs = Vec::new();
        while let Some(event) = stream.next().await {
            let event = event.expect("provider event");
            for fact in event.canonical_facts() {
                if let GatewayEvent::CalculatedCost(cost) = fact {
                    costs.push(cost.total().amount().scaled());
                }
            }
            if let Some(observation) = event.response_observation() {
                observed = observation.upstream_response_model().map(str::to_owned);
            }
            if let Some(wire) = event.wire_event()
                && wire.data().get("type").and_then(Value::as_str) == Some("response.completed")
            {
                returned = wire
                    .data()
                    .pointer("/response/model")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
        }
        server.await.expect("server task");
        assert_eq!(costs, vec![3_437_500]);
        assert_eq!(
            observed.as_deref(),
            Some("gpt-server-report"),
            "{metadata_type}"
        );
        assert_eq!(returned.as_deref(), Some("gpt-6-astra"));
    }
}

#[tokio::test]
async fn selected_proxy_location_overrides_global_and_reloads_without_mutating_client_payload() {
    use gateway_core::account::{OutboundProxy, RequestLocation};
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_provider_contract";
    create_account(&store, account_id).await;
    let first_proxy = MockServer::start().await;
    let second_proxy = MockServer::start().await;
    for proxy in [&first_proxy, &second_proxy] {
        Mock::given(method("POST"))
            .and(path("/codex/responses"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(CAPTURE_COMPLETED_SSE),
            )
            .mount(proxy)
            .await;
    }
    // 目标不可解析；收到请求证明使用的是账号代理，而非测试机默认出口。
    let provider = provider_with_base_url(&store, "http://upstream.invalid".to_owned());
    let original = json!({"model":"gpt-5.4", "input":[
        {"role":"user", "content":[{"type":"input_text", "text":"<environment_context><timezone>UTC</timezone></environment_context>"}], "internal_chat_message_metadata_passthrough":{"content_item_kinds":["environments.environment_context"], "create_time":1789293131.822}},
        {"role":"user", "content":[{"type":"input_text", "text":"<environment_context><timezone>UTC</timezone></environment_context>"}], "internal_chat_message_metadata_passthrough":{"content_item_kinds":["user.text"]}}
    ], "tools":[{"type":"web_search"}]});
    let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", original.as_object().unwrap().clone())
            .unwrap()
            .with_context(Map::from_iter([("use_websocket".to_owned(), json!(false))])),
    ));
    for (index, (proxy, location, global_enabled, expected)) in [
        (&first_proxy, Some(json!({"country":"JP", "region":"Tokyo", "city":"Tokyo", "timezone":"Asia/Tokyo"})), true, Some("Asia/Tokyo")),
        (&second_proxy, Some(json!({"country":"US", "region":"New York", "city":"New York", "timezone":"America/New_York"})), true, Some("America/New_York")),
        (&second_proxy, None, true, Some("Pacific/Auckland")),
        (&second_proxy, None, false, None),
        (&first_proxy, Some(json!({"country":"JP", "region":"Tokyo", "city":"Tokyo", "timezone":"Asia/Tokyo"})), false, Some("Asia/Tokyo")),
    ].into_iter().enumerate() {
        let location = location.map(|value| serde_json::from_value::<RequestLocation>(value).unwrap());
        store.set_egress(account_id, Some(OutboundProxy::parse(&proxy.uri()).unwrap()), location);
        let mut stream = provider.execute(planned_request("openai", operation.clone()), context_with_state_owner_and_location(&format!("req_proxy_location_{index}"), account_id, global_enabled.then(global_request_location))).await.expect("prepare");
        while let Some(event) = stream.next().await { event.expect("proxy response"); }
        let requests = proxy.received_requests().await.unwrap();
        let body = captured_request_body(requests.last().expect("request reached selected proxy"));
        assert_eq!(body.pointer("/tools/0/user_location/timezone"), expected.map(|timezone| json!(timezone)).as_ref());
        let expected = expected.unwrap_or("UTC");
        assert_eq!(body.pointer("/input/0/content/0/text"), Some(&json!(format!("<environment_context><timezone>{expected}</timezone></environment_context>"))));
        assert_eq!(body.pointer("/input/1/content/0/text"), original.pointer("/input/1/content/0/text"));
        assert_eq!(body.pointer("/input/0/internal_chat_message_metadata_passthrough/create_time"), Some(&json!(1789293131.822)));
    }
    assert_eq!(first_proxy.received_requests().await.unwrap().len(), 2);
    assert_eq!(second_proxy.received_requests().await.unwrap().len(), 3);
}

const OFFICIAL_FIXTURE: &[u8] =
    include_bytes!("../transport/fixtures/official_models_snapshot.json");

#[tokio::test]
async fn replay_compatibility_should_remove_only_reasoning_status_on_both_transports() {
    let input = json!([
        {"type":"message","role":"user","status":"completed","content":[{"type":"input_text","text":"hello"}]},
        {"type":"reasoning","id":"rs_replay","status":"completed","summary":[],"content":[],"encrypted_content":"test-cipher","extension":{"status":"keep","content":[1]}},
        {"type":"function_call","call_id":"call_replay","name":"echo","arguments":"{}","status":"completed"},
        {"type":"function_call_output","call_id":"call_replay","output":{"status":"keep","content":[1]},"status":"completed"},
        {"type":"tool_search_output","call_id":"call_search","status":"completed","tools":[]},
        {"type":"reasoning","status":"in_progress","summary":[],"encrypted_content":"second-test-cipher"},
        {"type":"future_item","status":"keep","content":[1]},
        "opaque-item"
    ]);
    let mut expected = input.clone();
    for index in [1, 5] {
        expected[index]
            .as_object_mut()
            .unwrap()
            .shift_remove("status");
    }
    for websocket in [false, true] {
        let actual = capture_replay_compatibility_request(input.clone(), false, websocket).await;
        // 比较序列化结果，同时保护未修改字段的顺序。
        assert_eq!(actual["input"].to_string(), expected.to_string());
    }
}

#[tokio::test]
async fn replay_compatibility_should_keep_encrypted_history_when_removing_nonempty_content() {
    let input = json!([
        {"type":"reasoning","id":"rs_replay","summary":[{"type":"summary_text","text":"keep summary"}],"content":[{"type":"reasoning_text","text":"synthetic replay"}],"encrypted_content":"test-cipher","extension":9007199254740993_u64},
        {"type":"compaction","id":"cmp_replay","content":[{"type":"text","text":"keep compaction"}],"encrypted_content":"compaction-test-cipher"},
        {"type":"message","role":"assistant","content":[{"type":"output_text","text":"keep answer"}]}
    ]);
    let mut expected = input.clone();
    expected[0].as_object_mut().unwrap().shift_remove("content");
    for websocket in [false, true] {
        let actual = capture_replay_compatibility_request(input.clone(), false, websocket).await;
        assert_eq!(actual["input"].to_string(), expected.to_string());
    }
}

#[tokio::test]
async fn replay_compatibility_should_preserve_plaintext_only_and_unrecognized_content_shapes() {
    let input = json!([
        {"type":"reasoning","summary":[],"content":[{"type":"reasoning_text","text":"only history"}]},
        {"type":"reasoning","summary":[],"content":[1],"encrypted_content":""},
        {"type":"reasoning","summary":[],"content":[1],"encrypted_content":"  "},
        {"type":"reasoning","summary":[],"content":[1],"encrypted_content":null},
        {"type":"reasoning","summary":[],"content":[1],"encrypted_content":42},
        {"type":"reasoning","summary":[],"content":{"status":"keep"},"encrypted_content":"test-cipher"},
        {"type":"reasoning","summary":[],"content":"keep","encrypted_content":"test-cipher"}
    ]);
    for websocket in [false, true] {
        let actual = capture_replay_compatibility_request(input.clone(), false, websocket).await;
        assert_eq!(actual["input"].to_string(), input.to_string());
    }
}

#[tokio::test]
async fn replay_compatibility_should_leave_normal_official_history_unchanged() {
    let input = json!([
        {"type":"reasoning","id":"rs_empty","summary":[],"content":[],"encrypted_content":"test-cipher"},
        {"type":"reasoning","id":"rs_null","summary":[],"content":null,"encrypted_content":"test-cipher"},
        {"type":"reasoning","id":"rs_absent","summary":[],"encrypted_content":"test-cipher"},
        {"type":"message","role":"user","content":[{"type":"input_text","text":"do not change status/content"}]},
        {"type":"custom_tool_call","call_id":"call_custom","status":"completed","name":"custom","input":"keep"},
        {"type":"tool_search_call","call_id":"call_search","status":"completed","arguments":{}},
        {"type":"mcp_call","status":"completed","output":"keep"},
        {"summary":[],"content":[1],"status":"keep"},
        {"type":"future_item","content":[1],"status":"keep"},
        null,
        "opaque-item"
    ]);
    for websocket in [false, true] {
        let actual = capture_replay_compatibility_request(input.clone(), false, websocket).await;
        assert_eq!(actual["input"].to_string(), input.to_string());
    }
}

#[tokio::test]
async fn replay_compatibility_should_not_apply_codex_rules_to_api_key_accounts() {
    let input = json!([{
        "type":"reasoning","id":"rs_api","status":"completed","summary":[],
        "content":[{"type":"reasoning_text","text":"API-specific history"}],
        "encrypted_content":"test-cipher"
    }]);
    for websocket in [false, true] {
        let actual = capture_replay_compatibility_request(input.clone(), true, websocket).await;
        assert_eq!(actual["input"].to_string(), input.to_string());
    }
}

#[tokio::test]
async fn replay_compatibility_should_preserve_non_array_input_without_guessing() {
    for input in [
        Value::Null,
        json!({"type":"reasoning","status":"keep"}),
        json!(7),
    ] {
        let actual = capture_replay_compatibility_request(input.clone(), false, false).await;
        assert_eq!(actual["input"], input);
    }
}

async fn capture_replay_compatibility_request(
    input: Value,
    api_key: bool,
    websocket: bool,
) -> Value {
    let http = MockServer::start().await;
    let (base_url, websocket_server) = if websocket {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let task =
            tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut websocket = accept_codex_test_websocket(stream).await;
                let frame = websocket.next().await.unwrap().unwrap();
                let body: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
                websocket.send(Message::Text(json!({
                "type":"response.completed","response":{
                    "id":"resp_replay","model":"gpt-5.4","status":"completed","output":[],
                    "usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}
                }
            }).to_string().into())).await.unwrap();
                body
            });
        (base_url, Some(task))
    } else {
        Mock::given(method("POST"))
            .and(path(if api_key {
                "/responses"
            } else {
                "/codex/responses"
            }))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(CAPTURE_COMPLETED_SSE, "text/event-stream"),
            )
            .expect(1)
            .mount(&http)
            .await;
        (http.uri(), None)
    };
    let store = Arc::new(MemoryAccountStore::default());
    if api_key {
        store
            .seed_api_key(
                "acct_provider_contract",
                base_url.clone(),
                if websocket {
                    provider_openai::credential::ApiKeyTransport::PreferWebsocket
                } else {
                    provider_openai::credential::ApiKeyTransport::Http
                },
            )
            .await;
    } else {
        create_account(&store, "acct_provider_contract").await;
    }
    let original = json!({
        "model":"gpt-5.4","store":false,"stream":true,"input":input,
        "instructions":"Preserve the original instructions.",
        "tools":[{"type":"function","name":"echo","parameters":{"type":"object","properties":{"status":{"type":"string"},"content":{"type":"array"}}}}],
        "future_field":{"status":"keep","content":[1]},
        "client_metadata":{"extension":{"status":"keep","content":[1]}}
    });
    let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", original.as_object().unwrap().clone())
            .unwrap()
            .with_context(Map::from_iter([(
                "use_websocket".to_owned(),
                json!(websocket),
            )])),
    ));
    let provider = provider_with_base_url(&store, base_url);
    let mut stream = provider
        .execute(
            planned_request("openai", operation),
            context("req_replay_compatibility", CancellationToken::new()),
        )
        .await
        .unwrap();
    while let Some(event) = stream.next().await {
        event.expect("successful upstream completion");
    }
    let actual = if let Some(server) = websocket_server {
        timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap()
    } else {
        let requests = http.received_requests().await.unwrap();
        assert_eq!(
            requests.len(),
            1,
            "compatibility must run before the first send, without retry"
        );
        captured_request_body(&requests[0])
    };
    for field in ["model", "store", "instructions", "tools", "future_field"] {
        assert_eq!(
            actual[field].to_string(),
            original[field].to_string(),
            "unexpected change in {field}"
        );
    }
    assert_eq!(
        actual["client_metadata"]["extension"],
        original["client_metadata"]["extension"]
    );
    actual
}

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
        client_kind: provider_openai::transport::profile::selection::ClientKind::Desktop,
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
    let (provider, quota, _) = provider_and_quota_with_runtime_ports(
        store,
        session_affinity,
        base_url,
        leases,
        stream_max_retries,
        Arc::new(MemoryCooldownPort::new()),
        crate::support::runtime_policy(),
    );
    (provider, quota)
}

fn provider_and_quota_with_runtime_ports(
    store: &Arc<MemoryAccountStore>,
    session_affinity: Arc<MemorySessionAffinity>,
    base_url: String,
    leases: Arc<TestLeaseCoordinator>,
    stream_max_retries: u32,
    cooldowns: Arc<MemoryCooldownPort>,
    policy: Arc<dyn gateway_core::provider_ports::ProviderRuntimePolicyPort>,
) -> (
    CodexProvider,
    Arc<CodexCredentialQuotaService>,
    Arc<CodexWebSocketPool>,
) {
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
        cooldowns,
        Arc::clone(&leases) as Arc<dyn ProviderLeasePort>,
        policy,
    ));
    let account_feedback = Arc::new(AccountFeedbackStats::default());
    let selector = Arc::new(CodexCredentialSelector::new(
        ProviderKind::new("openai").expect("provider"),
        store.repository(),
        leases,
        session_affinity,
        Arc::new(MemorySessionExclusions::default()),
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
        Arc::clone(&websocket_pool),
        stream_max_retries,
    )
    .expect("official OpenAI provider");
    (provider, quota, websocket_pool)
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
        "acct_ws_quota_a",
        "acct_ws_quota_b",
        "acct_ws_midstream_overload",
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

fn global_request_location() -> gateway_core::account::RequestLocation {
    gateway_core::account::RequestLocation {
        country: "NZ".to_owned(),
        region: "Auckland".to_owned(),
        city: "Auckland".to_owned(),
        timezone: "Pacific/Auckland".parse().expect("valid timezone"),
    }
}

fn context(request_id: &str, cancellation: CancellationToken) -> AttemptContext {
    context_with_fast_policy(request_id, cancellation, false)
}

fn context_with_fast_policy(
    request_id: &str,
    cancellation: CancellationToken,
    disable_fast: bool,
) -> AttemptContext {
    context_with_pricing(request_id, cancellation, disable_fast, Default::default())
}

fn context_with_pricing(
    request_id: &str,
    cancellation: CancellationToken,
    disable_fast: bool,
    pricing: gateway_core::metering::PricingOverrides,
) -> AttemptContext {
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new(request_id).expect("request id"),
            ClientApiKeyId::new("key_openai_contract").expect("client key id"),
        )
        .with_disable_fast(disable_fast)
        .with_pricing(Arc::new(pricing))
        .with_request_location(Some(global_request_location())),
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
        )
        .with_request_location(Some(global_request_location())),
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
    context_with_state_owner_and_location(
        request_id,
        owner_account_id,
        Some(global_request_location()),
    )
}

fn context_with_state_owner_and_location(
    request_id: &str,
    owner_account_id: &str,
    location: Option<gateway_core::account::RequestLocation>,
) -> AttemptContext {
    let owner = ProviderAccountStateOwner::new(
        ProviderKind::new("openai").expect("provider"),
        ProviderAccountId::new(owner_account_id).expect("owner account id"),
    );
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new(request_id).expect("request id"),
            ClientApiKeyId::new("key_openai_contract").expect("client key id"),
        )
        .with_request_location(location),
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
        )
        .with_request_location(Some(global_request_location())),
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
        )
        .with_request_location(Some(global_request_location())),
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
        )
        .with_request_location(Some(global_request_location())),
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

#[tokio::test]
async fn provider_should_send_the_request_snapshot_location_to_the_upstream() {
    let body = json!({
        "model": "gpt-5.4",
        "input": [{
            "role": "user",
            "content": [{"type": "input_text", "text": "<environment_context><timezone>UTC</timezone></environment_context>"}],
            "internal_chat_message_metadata_passthrough": {
                "content_item_kinds": ["environments.environment_context"],
                "create_time": 1789293131.822
            }
        }],
        "tools": [{"type": "web_search"}]
    }).as_object().expect("request object").clone();
    let captured = capture_scoped_http_request(
        "req_configured_location",
        "acct_provider_contract",
        "acct_provider_contract",
        body,
        Map::new(),
    )
    .await;
    let body = captured_request_body(&captured);
    assert_eq!(
        body.pointer("/tools/0/user_location"),
        Some(&json!({
            "type": "approximate", "country": "NZ", "region": "Auckland", "city": "Auckland", "timezone": "Pacific/Auckland"
        }))
    );
    assert_eq!(
        body.pointer("/input/0/content/0/text"),
        Some(&json!(
            "<environment_context><timezone>Pacific/Auckland</timezone></environment_context>"
        ))
    );
    assert_eq!(
        body.pointer("/input/0/internal_chat_message_metadata_passthrough/create_time"),
        Some(&json!(1789293131.822))
    );
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
async fn quota_snapshot_race_reloads_and_sends_the_request_once() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let original = store.account("acct_provider_contract").expect("account");
    let observed_at = SystemTime::now();
    store
        .apply_quota_access(QuotaAccessChange {
            account_id: original.id().clone(),
            expected_revision: original.revision(),
            state: QuotaState::allowed(observed_at),
        })
        .await
        .expect("initial quota");
    store.on_credential_load(Arc::new(move |store, id, count| {
        Box::pin(async move {
            if count == 1 {
                let account = store.account(id.as_str()).expect("account");
                store
                    .apply_quota_access(QuotaAccessChange {
                        account_id: id.clone(),
                        expected_revision: account.revision(),
                        state: QuotaState::allowed(observed_at + Duration::from_millis(1)),
                    })
                    .await?;
            }
            Ok(())
        })
    }));
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer at-acct_provider_contract"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(CAPTURE_COMPLETED_SSE, "text/event-stream"),
        )
        .expect(1)
        .mount(&upstream)
        .await;
    let leases = Arc::new(TestLeaseCoordinator::default());
    let provider = provider_with_affinity_and_base_url_and_leases(
        &store,
        Arc::new(MemorySessionAffinity::default()),
        upstream.uri(),
        Arc::clone(&leases),
    );
    let mut stream = provider
        .execute(
            planned_request("openai", http_generate_operation()),
            context("req_quota_snapshot_race", CancellationToken::new()),
        )
        .await
        .expect("concurrent quota observation must not fail prepare");
    assert_eq!(stream.metadata().provider_account_id(), original.id());
    assert_eq!(store.credential_loads(), 2);
    assert_eq!(leases.requests.lock().expect("leases").len(), 1);
    let mut completed = false;
    while let Some(event) = stream.next().await {
        let event = event.expect("upstream event");
        completed |= event
            .wire_event()
            .is_some_and(|wire| wire.data()["type"] == "response.completed");
    }
    assert!(
        completed,
        "the successful upstream response must reach the client stream"
    );
    upstream.verify().await;
}

#[tokio::test]
async fn snapshot_retry_rechecks_account_availability_without_sending() {
    for change in ["disabled", "revoked", "exhausted"] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_provider_contract").await;
        store.on_credential_load(Arc::new(move |store, id, count| {
            Box::pin(async move {
                assert_eq!(count, 1, "an unavailable account cannot be selected again");
                let account = store.account(id.as_str()).expect("account");
                match change {
                    "disabled" => store.set_enabled(id, false).await?,
                    "revoked" => {
                        store
                            .apply_state_change(gateway_core::account::AccountStateChange {
                                account_id: id.clone(),
                                expected_revision: account.revision(),
                                credential_state: CredentialState::Invalid,
                                observed_at: SystemTime::now(),
                                error_reason: None,
                                message: None,
                            })
                            .await?
                    }
                    "exhausted" => {
                        store
                            .apply_quota_access(QuotaAccessChange {
                                account_id: id.clone(),
                                expected_revision: account.revision(),
                                state: QuotaState::exhausted(
                                    QuotaEvidence::UsageLimitReached,
                                    SystemTime::now(),
                                    None,
                                ),
                            })
                            .await?;
                    }
                    _ => unreachable!("test case"),
                }
                Ok(())
            })
        }));
        let upstream = MockServer::start().await;
        let leases = Arc::new(TestLeaseCoordinator::default());
        let provider = provider_with_affinity_and_base_url_and_leases(
            &store,
            Arc::new(MemorySessionAffinity::default()),
            upstream.uri(),
            Arc::clone(&leases),
        );
        let Err(error) = provider
            .execute(
                planned_request("openai", http_generate_operation()),
                context("req_snapshot_unavailable", CancellationToken::new()),
            )
            .await
        else {
            panic!("{change} account must not produce an upstream stream");
        };
        let expected = if change == "exhausted" {
            ProviderErrorKind::QuotaExhausted
        } else {
            ProviderErrorKind::NoEligibleAccount
        };
        assert_eq!(error.kind(), expected, "{change}");
        assert_eq!(error.send_state(), UpstreamSendState::NotSent);
        assert!(leases.requests.lock().expect("leases").is_empty());
        assert!(
            upstream
                .received_requests()
                .await
                .expect("requests")
                .is_empty()
        );
    }
}

#[tokio::test]
async fn snapshot_retry_cannot_switch_a_disabled_native_continuation_account() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    create_account(&store, "acct_scope_new").await;
    store.on_credential_load(Arc::new(|store, id, count| {
        Box::pin(async move {
            assert_eq!(id.as_str(), "acct_provider_contract");
            assert_eq!(count, 1);
            store.set_enabled(id, false).await
        })
    }));
    let upstream = MockServer::start().await;
    let leases = Arc::new(TestLeaseCoordinator::default());
    let provider = provider_with_affinity_and_base_url_and_leases(
        &store,
        Arc::new(MemorySessionAffinity::default()),
        upstream.uri(),
        Arc::clone(&leases),
    );
    let Err(error) = provider
        .execute(
            planned_request("openai", http_generate_operation()),
            pinned_continuation_context(
                "req_snapshot_pinned",
                "acct_provider_contract",
                "client-previous",
                "upstream-previous",
                1,
                ContinuationAttempt::Native,
            ),
        )
        .await
    else {
        panic!("native continuation cannot move to the healthy second account");
    };
    assert_eq!(error.kind(), ProviderErrorKind::NoEligibleAccount);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    assert!(leases.requests.lock().expect("leases").is_empty());
    assert!(
        upstream
            .received_requests()
            .await
            .expect("requests")
            .is_empty()
    );
}

#[tokio::test]
async fn repeated_snapshot_conflicts_are_bounded_and_report_the_selection_stage() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    store.on_credential_load(Arc::new(|store, id, count| {
        Box::pin(async move {
            assert!(count <= 4, "snapshot conflict retries must be bounded");
            let account = store.account(id.as_str()).expect("account");
            let observed_at = account
                .quota()
                .observed_at()
                .unwrap_or_else(SystemTime::now)
                + Duration::from_millis(1);
            store
                .apply_quota_access(QuotaAccessChange {
                    account_id: id.clone(),
                    expected_revision: account.revision(),
                    state: QuotaState::allowed(observed_at),
                })
                .await?;
            Ok(())
        })
    }));
    let upstream = MockServer::start().await;
    let leases = Arc::new(TestLeaseCoordinator::default());
    let provider = provider_with_affinity_and_base_url_and_leases(
        &store,
        Arc::new(MemorySessionAffinity::default()),
        upstream.uri(),
        Arc::clone(&leases),
    );
    let Err(error) = provider
        .execute(
            planned_request("openai", http_generate_operation()),
            context("req_snapshot_conflicts", CancellationToken::new()),
        )
        .await
    else {
        panic!("continuously changing snapshots must not be sent");
    };
    assert_eq!(
        error.kind(),
        ProviderErrorKind::ProviderInfrastructureUnavailable
    );
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    let diagnostic = error.diagnostic().expect("selection diagnostic");
    assert_eq!(diagnostic.stage(), Some("account_selection"));
    assert_eq!(diagnostic.code(), Some("account_snapshot_conflict"));
    assert_eq!(store.credential_loads(), 4);
    assert!(leases.requests.lock().expect("leases").is_empty());
    assert!(
        upstream
            .received_requests()
            .await
            .expect("requests")
            .is_empty()
    );
}

#[tokio::test]
async fn api_websocket_precheck_and_selection_share_the_snapshot_retry_budget() {
    let upstream = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_api_key(
            "acct_provider_contract",
            upstream.uri(),
            provider_openai::credential::ApiKeyTransport::PreferWebsocket,
        )
        .await;
    store.on_credential_load(Arc::new(|store, id, count| {
        Box::pin(async move {
            assert!(
                count <= 6,
                "both credential checks must share one retry budget"
            );
            // 第 1、4 次在传输预检冲突，第 3、6 次在最终候选校验冲突。
            if matches!(count, 1 | 3 | 4 | 6) {
                let account = store.account(id.as_str()).expect("account");
                let observed_at = account
                    .quota()
                    .observed_at()
                    .unwrap_or_else(SystemTime::now)
                    + Duration::from_millis(1);
                store
                    .apply_quota_access(QuotaAccessChange {
                        account_id: id.clone(),
                        expected_revision: account.revision(),
                        state: QuotaState::allowed(observed_at),
                    })
                    .await?;
            }
            Ok(())
        })
    }));
    let leases = Arc::new(TestLeaseCoordinator::default());
    let provider = provider_with_affinity_and_base_url_and_leases(
        &store,
        Arc::new(MemorySessionAffinity::default()),
        upstream.uri(),
        Arc::clone(&leases),
    );
    let warmup = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            json!({"model":"gpt-5.4","input":[],"store":false,"generate":false})
                .as_object()
                .expect("object")
                .clone(),
        )
        .expect("warmup"),
    ));
    let Err(error) = provider
        .execute(
            planned_request("openai", warmup),
            context("req_api_snapshot_conflicts", CancellationToken::new()),
        )
        .await
    else {
        panic!("mixed snapshot conflicts must stop before WebSocket connection");
    };
    assert_eq!(
        error.kind(),
        ProviderErrorKind::ProviderInfrastructureUnavailable
    );
    assert_eq!(
        error.diagnostic().and_then(|diagnostic| diagnostic.code()),
        Some("account_snapshot_conflict")
    );
    assert_eq!(store.credential_loads(), 6);
    assert!(leases.requests.lock().expect("leases").is_empty());
    assert!(
        upstream
            .received_requests()
            .await
            .expect("requests")
            .is_empty()
    );
}

async fn exhaust_account_quota(store: &Arc<MemoryAccountStore>, account_id: &str) {
    let account = store.account(account_id).expect("test account");
    store
        .apply_quota_access(QuotaAccessChange {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            state: QuotaState::exhausted(QuotaEvidence::UsageLimitReached, SystemTime::now(), None),
        })
        .await
        .expect("exhaust account quota");
}

#[tokio::test]
async fn exhausted_account_pool_returns_usage_limit_before_http_or_websocket_network_io() {
    let store = Arc::new(MemoryAccountStore::default());
    for id in ["acct_provider_contract", "acct_scope_new"] {
        create_account(&store, id).await;
        exhaust_account_quota(&store, id).await;
    }
    // 范围外的可用账号不能掩盖当前 Client Key 的额度耗尽。
    create_account(&store, "acct_outside_scope").await;
    let server = MockServer::start().await;
    let provider = provider_with_base_url(&store, server.uri());
    for operation in [http_generate_operation(), generate_operation()] {
        let Err(error) = provider
            .execute(
                planned_request("openai", operation),
                context("req_exhausted_pool", CancellationToken::new()),
            )
            .await
        else {
            panic!("exhausted pool must fail before opening an upstream stream");
        };
        assert_eq!(error.kind(), ProviderErrorKind::QuotaExhausted);
        assert_eq!(error.send_state(), UpstreamSendState::NotSent);
        assert_eq!(error.upstream_status(), None);
        let detail = error.client_visible_upstream_error().expect("quota detail");
        assert_eq!(detail.error_type(), Some("usage_limit_reached"));
        assert_eq!(detail.code(), Some("usage_limit_reached"));
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn exhausted_account_pool_does_not_mask_a_remaining_healthy_account() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    exhaust_account_quota(&store, "acct_provider_contract").await;
    create_account(&store, "acct_scope_new").await;
    let server = MockServer::start().await;
    let stream = provider_with_base_url(&store, server.uri())
        .execute(
            planned_request("openai", http_generate_operation()),
            context("req_partial_exhaustion", CancellationToken::new()),
        )
        .await
        .expect("remaining account is selected");
    assert_eq!(
        stream.metadata().provider_account_id().as_str(),
        "acct_scope_new"
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn exhausted_account_pool_classification_preserves_other_unavailability_causes() {
    for enabled in [true, false] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account_with_enabled(&store, "acct_provider_contract", enabled).await;
        exhaust_account_quota(&store, "acct_provider_contract").await;
        if enabled {
            create_account(&store, "acct_scope_new").await;
            let account = store.account("acct_scope_new").expect("test account");
            store
                .apply_state_change(gateway_core::account::AccountStateChange {
                    account_id: account.id().clone(),
                    expected_revision: account.revision(),
                    credential_state: CredentialState::Expired,
                    observed_at: SystemTime::now(),
                    error_reason: None,
                    message: None,
                })
                .await
                .expect("expire other account");
        }
        let Err(error) = provider(&store)
            .execute(
                planned_request("openai", http_generate_operation()),
                context("req_mixed_unavailability", CancellationToken::new()),
            )
            .await
        else {
            panic!("unavailable accounts cannot execute");
        };
        assert_eq!(error.kind(), ProviderErrorKind::NoEligibleAccount);
    }
}

#[tokio::test]
async fn exhausted_pinned_account_keeps_quota_cause_for_native_continuation_recovery() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    exhaust_account_quota(&store, "acct_provider_contract").await;
    create_account(&store, "acct_scope_new").await;
    let Err(error) = provider(&store)
        .execute(
            planned_request("openai", http_generate_operation()),
            pinned_continuation_context(
                "req_exhausted_pin",
                "acct_provider_contract",
                "client-previous",
                "upstream-previous",
                1,
                ContinuationAttempt::Native,
            ),
        )
        .await
    else {
        panic!("native continuation cannot use the other account before recovery");
    };
    assert_eq!(error.kind(), ProviderErrorKind::QuotaExhausted);
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
                            GatewayEvent::CalculatedCost(cost) => Some((index, cost.clone())),
                            _ => None,
                        })
                })
                .collect::<Vec<_>>();
            assert_eq!(costs.len(), 1, "model={model}");
            let (cost_index, cost) = costs[0].clone();
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
    image_metering_events_with_pricing(kind, response, model, Default::default()).await
}

async fn image_metering_events_with_pricing(
    kind: ImageRequestKind,
    response: &[u8],
    model: &str,
    pricing: gateway_core::metering::PricingOverrides,
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
            context_with_pricing("req_image_usage", CancellationToken::new(), false, pricing),
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
async fn image_custom_prices_apply_text_and_image_rates_from_the_frozen_request() {
    let pricing = serde_json::from_value(json!({"openai":{"gpt-image-2":{
        "multiplierBps":20000,"bands":{
            "standard":{"input":"2","output":"0","cacheRead":"0","cacheWrite":"0"},
            "image":{"input":"8","output":"30","cacheRead":"2","cacheWrite":"0"}
        }
    }}}))
    .unwrap();
    let response = serde_json::to_vec(&json!({"data":[{"b64_json":"AAEC"}],"usage":{
        "input_tokens":100,"input_tokens_details":{"text_tokens":20,"image_tokens":80},
        "output_tokens":10,"output_tokens_details":{"text_tokens":0,"image_tokens":10},"total_tokens":110
    }})).unwrap();
    let events = image_metering_events_with_pricing(
        ImageRequestKind::Generation,
        &response,
        "gpt-image-2",
        pricing,
    )
    .await;
    let cost = events
        .iter()
        .flat_map(|event| event.canonical_facts())
        .find_map(|event| match event {
            GatewayEvent::CalculatedCost(cost) => Some(cost.clone()),
            _ => None,
        })
        .unwrap()
        .into_estimate();
    assert_eq!(cost.total().unwrap().amount().canonical(), "0.00196");
    let breakdown = cost.breakdown().unwrap();
    assert_eq!(breakdown.input_amount().amount().canonical(), "0.00008");
    assert_eq!(
        breakdown.image().unwrap().input_amount.amount().canonical(),
        "0.00128"
    );
    assert_eq!(breakdown.custom_multiplier_bps(), 20000);
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
async fn websocket_midstream_error_frame_surfaces_upstream_message_after_delivery() {
    const ACCOUNT_ID: &str = "acct_ws_midstream_overload";
    const RESPONSE_ID: &str = "resp_midstream_overload";
    const OVERLOAD_MESSAGE: &str = "Our servers are currently overloaded. Please try again later.";

    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, ACCOUNT_ID).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept WebSocket");
        let mut websocket = accept_codex_test_websocket(stream).await;
        let _request = websocket
            .next()
            .await
            .expect("WebSocket request")
            .expect("valid WebSocket request");
        // 真实生产观察：OpenAI 在流已开始后发送带原话的 `error` 帧再断连。
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.created",
                    "response": {"id": RESPONSE_ID, "model": "gpt-5.4", "status": "in_progress"}
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("send response.created");
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.in_progress",
                    "response": {"id": RESPONSE_ID, "model": "gpt-5.4", "status": "in_progress"}
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("send response.in_progress");
        tokio::time::sleep(Duration::from_millis(1_600)).await;
        websocket
            .send(Message::Text(
                json!({
                    "type": "error",
                    "error": {
                        "type": "service_unavailable_error",
                        "code": "server_is_overloaded",
                        "message": OVERLOAD_MESSAGE,
                        "param": null
                    },
                    "sequence_number": 2
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("send error frame");
        // 不发送任何终止事件，直接断开，模拟上游发完错误帧后的真实行为。
    });

    let provider = provider_with_base_url(&store, base_url);
    let mut stream = provider
        .execute(
            planned_request("openai", generate_operation()),
            context("req_ws_midstream_overload", CancellationToken::new()),
        )
        .await
        .expect("prepare midstream-failure stream");
    let mut failure = loop {
        match stream.next().await {
            Some(Ok(_)) => {}
            Some(Err(error)) => break error,
            None => panic!("midstream error frame must surface a typed failure"),
        }
    };
    server.abort();

    assert_eq!(
        failure.kind(),
        ProviderErrorKind::UpstreamCapacityUnavailable
    );
    assert_eq!(failure.send_state(), UpstreamSendState::Sent);
    // 交给 Core 的失败必须保留上游原话：客户端只能靠它知道失败原因。
    let visible = failure
        .client_visible_upstream_error()
        .expect("overload frame must carry a client-visible upstream error");
    assert_eq!(visible.code(), Some("server_is_overloaded"));
    assert_eq!(visible.message(), OVERLOAD_MESSAGE);
    // 原始错误帧必须随失败一起交给交付边界（SSE 侧据此翻译成 response.failed）。
    let atomic = failure.take_atomic_client_events();
    let error_frame = atomic
        .iter()
        .find_map(|event| {
            let wire = event.wire_event()?;
            (wire.event_type() == Some("error")).then(|| wire.data().clone())
        })
        .expect("atomic batch must carry the upstream error frame");
    assert_eq!(
        error_frame
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str),
        Some(OVERLOAD_MESSAGE)
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
async fn websocket_opening_account_rejection_keeps_replay_safe_without_transport_retry() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_ws_quota_a").await;
    create_account(&store, "acct_ws_quota_b").await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut opening, _) = listener.accept().await.unwrap();
        let request = capture_http_request(&mut opening).await;
        assert!(String::from_utf8_lossy(&request).starts_with("GET /codex/responses"));
        // 额度耗尽拒绝通常携带小时级的 retry-after；同账号重试注定再次命中。
        let body = r#"{"error":{"message":"You have reached your usage limit.","type":"rate_limit_error"}}"#;
        opening
            .write_all(
                format!(
                    "HTTP/1.1 429 Too Many Requests\r\nretry-after: 129600\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });

    let provider = provider_with_base_url(&store, base_url);
    let mut stream = provider
        .execute(
            planned_request("openai", generate_operation()),
            context("req_ws_quota_rotation", CancellationToken::new()),
        )
        .await
        .expect("prepare quota-rejected stream");
    let error = loop {
        match stream.next().await {
            Some(Ok(_)) => {}
            Some(Err(error)) => break error,
            None => panic!("quota rejection must surface as a terminal error"),
        }
    };
    server.await.expect("upstream server");

    assert_eq!(error.kind(), ProviderErrorKind::RateLimited);
    assert_eq!(error.upstream_status(), Some(429));
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    assert!(
        error.replay_is_safe(),
        "before-payload rejection is replay safe"
    );
    // 回放安全的账号级拒绝必须把换号决策留给 Core，不得钉死同账号传输重试
    // （旧行为会携带小时级 retry-after 的同账号重试标记，请求必然超时）。
    assert_eq!(error.pre_delivery_retry(), None);
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
        body.pointer("/client_metadata/x-codex-installation-id")
    );
    assert!(body.pointer("/client_metadata/installation_id").is_none());
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
    // 官方 Codex 在工作区包含 Unicode 时也保持内嵌 turn metadata 为 ASCII。
    let raw = r#"{"installation_id":"client-installation","workspaces":{"C:\\Users\\\u9879\u76ee\\\ud83d\ude80":{"label":"caf\u00e9","literal":"\\u4e2d","quoted":"\"line\n"}}}"#;
    let input = json!([{"role": "user", "content": "中文正文 🚀"}]);
    let payload = ProtocolPayload::json_object(
        "openai",
        Map::from_iter([
            ("model".to_owned(), json!("gpt-5.4")),
            ("input".to_owned(), input.clone()),
            (
                "client_metadata".to_owned(),
                json!({
                    "x-codex-turn-metadata": raw,
                    "x-codex-installation-id": "client-installation",
                    "installation_id": "client-legacy-installation",
                    "installationId": "client-camel-installation"
                }),
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
    let installation_id = body["client_metadata"]["x-codex-installation-id"]
        .as_str()
        .expect("account installation ID");
    assert_ne!(installation_id, "client-installation");
    for alias in ["installation_id", "installationId"] {
        assert_eq!(body["client_metadata"][alias], installation_id);
    }
    let encoded = body
        .pointer("/client_metadata/x-codex-turn-metadata")
        .and_then(Value::as_str)
        .expect("turn metadata");
    assert!(encoded.is_ascii(), "embedded header JSON must remain ASCII");
    let mut expected: Value = serde_json::from_str(raw).expect("original metadata");
    expected["installation_id"] = json!(installation_id);
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
        let installation_id = body["client_metadata"]["x-codex-installation-id"]
            .as_str()
            .expect("account installation ID");
        assert_ne!(installation_id, "client-installation");
        assert!(body.pointer("/client_metadata/installation_id").is_none());
        assert!(body.pointer("/client_metadata/installationId").is_none());
        let headers = captured_header_values(&request, "x-codex-turn-metadata");
        assert_eq!(headers.len(), 1);
        let mut expected: Value = serde_json::from_str(raw).expect("original metadata");
        expected["installation_id"] = json!(installation_id);
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
            plan_type: None,
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
            (
                "input".to_owned(),
                json!([{"role": "user", "content": "second request"}]),
            ),
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
        Some(&json!([{"role": "user", "content": "second request"}]))
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
    assert_eq!(observed_service_tier.as_deref(), Some("priority"));
    assert_eq!(upstream_service_tier.as_deref(), Some("default"));
}

#[tokio::test]
async fn responses_should_observe_and_bill_the_outbound_service_tier_on_both_transports() {
    for use_websocket in [false, true] {
        for (disable_fast, requested, reported, expected_tier, expected_cost) in [
            (
                false,
                Some("priority"),
                Some("default"),
                Some("priority"),
                Some(6_875_000),
            ),
            (
                false,
                Some("priority"),
                None,
                Some("priority"),
                Some(6_875_000),
            ),
            (
                false,
                Some("default"),
                Some("priority"),
                Some("default"),
                Some(3_437_500),
            ),
            (false, None, Some("priority"), None, Some(3_437_500)),
            (
                true,
                Some("priority"),
                Some("priority"),
                Some("default"),
                Some(3_437_500),
            ),
            (true, Some("fast"), None, Some("default"), Some(3_437_500)),
            (
                true,
                Some("default"),
                None,
                Some("default"),
                Some(3_437_500),
            ),
            (true, None, None, None, Some(3_437_500)),
            (true, Some("flex"), None, Some("flex"), Some(1_720_000)),
            (true, Some("ultrafast"), None, Some("ultrafast"), None),
        ] {
            let store = Arc::new(MemoryAccountStore::default());
            create_account(&store, "acct_provider_contract").await;
            let mut created =
                json!({"type":"response.created","response":{"id":"resp_tier","model":"gpt-5.4"}});
            let mut completed = json!({
                "type":"response.completed",
                "response":{
                    "id":"resp_tier","model":"gpt-5.4","status":"completed","output":[],
                    "usage":{"input_tokens":100,"output_tokens":10,"input_tokens_details":{"cached_tokens":25,"cache_write_tokens":0},"total_tokens":110}
                }
            });
            if let Some(reported) = reported {
                created["response"]["service_tier"] = json!("auto");
                completed["response"]["service_tier"] = json!(reported);
            }
            let response_frames = format!(
                "event: response.created\ndata: {created}\n\nevent: response.completed\ndata: {completed}\n\n"
            );
            let (base_url, http_server, websocket_server) = if use_websocket {
                let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
                let base_url = format!("http://{}", listener.local_addr().expect("address"));
                let server = tokio::spawn(async move {
                    let (socket, _) = listener.accept().await.expect("accept WebSocket");
                    let mut websocket = crate::transport::accept_codex_test_websocket_with(
                        socket,
                        |request, response| {
                            let expected_hint = expected_tier.map_or_else(
                                || "model=gpt-5.4".to_owned(),
                                |tier| format!("model=gpt-5.4;tier={tier}"),
                            );
                            assert_eq!(request.headers()["x-codex-routing-hint"], expected_hint);
                            response.headers_mut().insert(
                                "sec-websocket-extensions",
                                "permessage-deflate".parse().unwrap(),
                            );
                        },
                    )
                    .await;
                    let request = websocket.next().await.expect("request").expect("frame");
                    let request: Value = serde_json::from_str(request.to_text().expect("text"))
                        .expect("request JSON");
                    for event in [created, completed] {
                        websocket
                            .send(Message::Text(event.to_string().into()))
                            .await
                            .expect("response");
                    }
                    request
                });
                (base_url, None, Some(server))
            } else {
                let server = MockServer::start().await;
                Mock::given(method("POST"))
                    .and(path("/codex/responses"))
                    .respond_with(
                        ResponseTemplate::new(200)
                            .set_body_raw(response_frames.clone(), "text/event-stream"),
                    )
                    .expect(1)
                    .mount(&server)
                    .await;
                (server.uri(), Some(server), None)
            };
            let mut body = Map::from_iter([
                ("model".to_owned(), json!("gpt-5.4")),
                ("input".to_owned(), json!("hello")),
                (
                    "metadata".to_owned(),
                    json!({"service_tier":"priority","text":"fast priority"}),
                ),
            ]);
            if let Some(requested) = requested {
                body.insert("service_tier".to_owned(), json!(requested));
            }
            let payload = ProtocolPayload::json_object("openai", body.clone())
                .expect("payload")
                .with_context(Map::from_iter([(
                    "use_websocket".to_owned(),
                    json!(use_websocket),
                )]));
            let operation = Operation::Generate(GenerateRequest::from_protocol_payload(payload));
            let original = operation.clone();
            let mut stream = provider_with_base_url(&store, base_url)
                .execute(
                    planned_request("openai", operation.clone()),
                    context_with_fast_policy(
                        "req_service_tier",
                        CancellationToken::new(),
                        disable_fast,
                    ),
                )
                .await
                .expect("provider stream");
            let mut observations = Vec::new();
            let mut costs = Vec::new();
            let mut raw_response = Vec::new();
            while let Some(event) = stream.next().await {
                let event = event.expect("provider event");
                if let Some(observation) = event.response_observation() {
                    observations.push(observation.clone());
                }
                for fact in event.canonical_facts() {
                    if let GatewayEvent::CalculatedCost(cost) = fact {
                        costs.push(cost.total().amount().scaled());
                    }
                }
                if let Some(frame) = event.wire_event().and_then(|wire| wire.raw_sse_frame()) {
                    raw_response.extend_from_slice(frame);
                }
            }
            let outbound = if let Some(server) = http_server {
                server.verify().await;
                let requests = server.received_requests().await.expect("upstream requests");
                let expected_hint = expected_tier.map_or_else(
                    || "model=gpt-5.4".to_owned(),
                    |tier| format!("model=gpt-5.4;tier={tier}"),
                );
                assert_eq!(requests[0].headers["x-codex-routing-hint"], expected_hint);
                captured_request_body(&requests[0])
            } else {
                websocket_server
                    .expect("WebSocket server")
                    .await
                    .expect("server task")
            };
            assert_eq!(
                outbound.get("service_tier").and_then(Value::as_str),
                expected_tier
            );
            assert_eq!(operation, original);
            assert_eq!(outbound["metadata"], body["metadata"]);
            assert!(!observations.is_empty());
            for observation in &observations {
                assert_eq!(
                    observation.service_tier(),
                    expected_tier,
                    "WebSocket={use_websocket}"
                );
            }
            let metadata: Value = serde_json::from_str(
                observations
                    .last()
                    .expect("final observation")
                    .provider_metadata()
                    .expect("metadata")
                    .as_json(),
            )
            .expect("metadata JSON");
            assert_eq!(
                metadata.get("requestedServiceTier").and_then(Value::as_str),
                expected_tier
            );
            assert_eq!(
                metadata.get("upstreamServiceTier").and_then(Value::as_str),
                reported
            );
            assert_eq!(
                costs,
                expected_cost.into_iter().collect::<Vec<_>>(),
                "WebSocket={use_websocket}, requested={requested:?}, reported={reported:?}"
            );
            assert_eq!(raw_response, response_frames.as_bytes());
        }
    }
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
            plan_type: None,
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
async fn capacity_http_and_websocket_opening_rejections_use_bounded_business_retry() {
    for use_websocket in [false, true] {
        for status in [400, 429, 503] {
            let store = Arc::new(MemoryAccountStore::default());
            let account_id = "acct_provider_contract";
            create_account(&store, account_id).await;
            let server = MockServer::start().await;
            let body = json!({"error": {
                "code": match status {
                    429 => Some("slow_down"),
                    503 => Some("server_is_overloaded"),
                    _ => None,
                },
                "type": "server_error",
                "message": "Selected model is at capacity. Please try a different model.",
                "extension": {"preserved": true}
            }});
            Mock::given(method(if use_websocket { "GET" } else { "POST" }))
                .and(path("/codex/responses"))
                .respond_with(
                    ResponseTemplate::new(status)
                        .insert_header("retry-after", "129600")
                        .set_body_json(body.clone()),
                )
                .expect(1)
                .mount(&server)
                .await;
            let operation = if use_websocket {
                generate_operation()
            } else {
                http_generate_operation()
            };
            let mut stream = provider_with_base_url(&store, server.uri())
                .execute(
                    planned_request("openai", operation),
                    context("req_capacity", CancellationToken::new()),
                )
                .await
                .expect("prepare stream");
            let error = loop {
                match stream.next().await {
                    Some(Ok(_)) => {}
                    Some(Err(error)) => break error,
                    None => panic!("expected capacity rejection"),
                }
            };
            assert_eq!(error.kind(), ProviderErrorKind::UpstreamCapacityUnavailable);
            assert_eq!(error.upstream_status(), Some(status));
            assert!(error.replay_is_safe());
            assert!(
                matches!(error.pre_delivery_retry(), Some(PreDeliveryRetry::SameAccountTransientRetry {
                max_retries, initial_delay, max_delay,
            }) if max_retries.get() == 3 && initial_delay == Duration::from_secs(8) && max_delay == Duration::from_secs(8))
            );
            assert!(provider_openai::openai_failure_affects_account_score(
                &error
            ));
            assert_eq!(
                serde_json::from_str::<Value>(
                    error.raw_upstream_error().expect("original error").as_str()
                )
                .expect("JSON"),
                body
            );
            let response = error
                .client_visible_upstream_response()
                .expect("client response");
            assert_eq!(response.status(), status);
            assert_eq!(response.body().as_ref(), body.to_string().as_bytes());
            let account = store.account(account_id).expect("account");
            assert_eq!(account.quota().access(), QuotaAccessState::Unknown);
            assert_eq!(account.credential_state(), CredentialState::Ready);
        }
    }
}

fn capacity_websocket_operation() -> Operation {
    let payload = ProtocolPayload::json_object(
        "openai",
        Map::from_iter([
            ("model".to_owned(), json!("gpt-5.4")),
            ("input".to_owned(), json!("hello")),
            ("store".to_owned(), json!(false)),
            ("session_id".to_owned(), json!("capacity-feedback-session")),
        ]),
    )
    .expect("WebSocket payload")
    .with_context(Map::from_iter([(
        "downstream_websocket_connection_id".to_owned(),
        json!("ws_capacity_feedback"),
    )]));
    Operation::Generate(
        GenerateRequest::from_protocol_payload(payload).with_provider_session_state(
            ProviderSessionState::new(
                "openai",
                Map::from_iter([
                    ("account_id".to_owned(), json!("acct_provider_contract")),
                    (
                        "conversation_id".to_owned(),
                        json!("capacity-feedback-session"),
                    ),
                    ("continuation_scope".to_owned(), json!("persisted")),
                ]),
            )
            .expect("WebSocket session scope"),
        ),
    )
}

fn provider_with_capacity_tracking(
    store: &Arc<MemoryAccountStore>,
    base_url: String,
    cooldowns: Arc<MemoryCooldownPort>,
) -> (CodexProvider, Arc<CodexWebSocketPool>) {
    let (provider, _, pool) = provider_and_quota_with_runtime_ports(
        store,
        Arc::new(MemorySessionAffinity::default()),
        base_url,
        Arc::new(TestLeaseCoordinator::default()),
        0,
        cooldowns,
        crate::support::StaticFreezePolicy::policy_port(
            gateway_core::provider_ports::ProviderFreezePolicy::try_new(
                true, 12, 600, 7_200, true, None, true,
            )
            .expect("freeze policy"),
        ),
    );
    (provider, pool)
}

#[tokio::test]
async fn capacity_feedback_only_counts_overload_rejections_and_excludes_diagnostic_probes() {
    use gateway_core::provider_ports::ProviderCooldownPort as _;

    for websocket in [false, true] {
        for (status, code, capacity) in [
            (429, "slow_down", true),
            (503, "server_is_overloaded", true),
            (500, "server_error", false),
            (502, "server_error", false),
            (503, "service_unavailable_error", false),
        ] {
            for diagnostic in [false, true] {
                let store = Arc::new(MemoryAccountStore::default());
                let account_id = "acct_provider_contract";
                create_account(&store, account_id).await;
                let account = store.account(account_id).expect("account");
                let cooldowns = Arc::new(MemoryCooldownPort::new());
                let server = MockServer::start().await;
                Mock::given(method(if websocket { "GET" } else { "POST" }))
                    .and(path("/codex/responses"))
                    .respond_with(
                        ResponseTemplate::new(status).set_body_json(json!({"error": {
                            "code": code,
                            "message": "Upstream temporarily unavailable"
                        }})),
                    )
                    .expect(2)
                    .mount(&server)
                    .await;
                let (provider, _) =
                    provider_with_capacity_tracking(&store, server.uri(), Arc::clone(&cooldowns));
                // 同时验证窗口证据已过期与尚未过期：探测不能重建峰值，也不能改写原计数。
                for existing_evidence in [None, Some((4, 20))] {
                    if let Some((count, peak)) = existing_evidence {
                        for _ in 0..count {
                            cooldowns
                                .record_capacity_failure(
                                    account.id(),
                                    Duration::from_secs(600),
                                    peak,
                                )
                                .await
                                .expect("seed normal-request evidence");
                        }
                    }
                    let before = cooldowns.capacity_evidence(account.id());
                    let attempt = if diagnostic {
                        diagnostic_context("req_capacity_feedback_probe", account_id)
                    } else {
                        context("req_capacity_feedback_business", CancellationToken::new())
                    };
                    let operation = if websocket {
                        capacity_websocket_operation()
                    } else {
                        http_generate_operation()
                    };
                    let mut stream = provider
                        .execute(planned_request("openai", operation), attempt)
                        .await
                        .expect("prepare upstream attempt");
                    let error = loop {
                        match stream.next().await {
                            Some(Ok(_)) => {}
                            Some(Err(error)) => break error,
                            None => panic!("expected upstream rejection"),
                        }
                    };
                    assert_eq!(
                        error.upstream_status(),
                        Some(status),
                        "unexpected upstream error: {error:?}"
                    );
                    let after = cooldowns.capacity_evidence(account.id());
                    if diagnostic || !capacity {
                        assert_eq!(
                            after, before,
                            "only explicit overload from ordinary requests may change capacity evidence: {status}/{code}"
                        );
                    } else {
                        assert_eq!(
                            after.map(|(count, _)| count),
                            Some(before.map_or(1, |(count, _)| count + 1))
                        );
                    }
                }
            }
        }
    }
}

#[tokio::test]
async fn local_websocket_connection_cancellation_does_not_supply_capacity_evidence() {
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_provider_contract";
    create_account(&store, account_id).await;
    let account = store.account(account_id).expect("account");
    let cooldowns = Arc::new(MemoryCooldownPort::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let base_url = format!("http://{}", listener.local_addr().expect("address"));
    let (provider, pool) =
        provider_with_capacity_tracking(&store, base_url, Arc::clone(&cooldowns));
    let operation = capacity_websocket_operation();
    let mut stream = provider
        .execute(
            planned_request("openai", operation),
            context("req_capacity_local", CancellationToken::new()),
        )
        .await
        .expect("prepare WebSocket attempt");
    let attempt = tokio::spawn(async move {
        loop {
            match stream.next().await {
                Some(Ok(_)) => {}
                Some(Err(error)) => break error,
                None => panic!("cancelled opening must fail"),
            }
        }
    });
    let (mut opening, _) = timeout(Duration::from_secs(5), listener.accept())
        .await
        .expect("opening deadline")
        .expect("opening");
    read_http_request(&mut opening).await;
    // 复现账号更新驱逐正在建连的连接，未收到任何上游容量拒绝。
    pool.evict_account(account_id).await;
    let error = timeout(Duration::from_secs(5), attempt)
        .await
        .expect("cancelled attempt deadline")
        .expect("attempt task");
    assert_eq!(error.kind(), ProviderErrorKind::Unavailable);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    assert_eq!(
        error.diagnostic().and_then(|diagnostic| diagnostic.code()),
        Some("shared_connect_failed")
    );
    assert_eq!(cooldowns.capacity_evidence(account.id()), None);
    pool.shutdown().await;
}

#[tokio::test]
async fn websocket_usage_limit_rejection_preserves_quota_state_for_account_rotation() {
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_provider_contract";
    create_account(&store, account_id).await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/codex/responses"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "129600")
            .set_body_json(json!({"error": {"type": "usage_limit_reached", "message": "limit reached", "resets_at": 1_900_000_000}})))
        .expect(1).mount(&server).await;
    let mut stream = provider_with_base_url(&store, server.uri())
        .execute(
            planned_request("openai", generate_operation()),
            context("req_ws_quota", CancellationToken::new()),
        )
        .await
        .expect("prepare stream");
    let error = loop {
        match stream.next().await {
            Some(Ok(_)) => {}
            Some(Err(error)) => break error,
            None => panic!("expected quota rejection"),
        }
    };
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    assert!(error.replay_is_safe());
    assert_eq!(error.pre_delivery_retry(), None);
    assert_eq!(error.retry_after(), Some(Duration::from_secs(129_600)));
    assert_eq!(error.kind(), ProviderErrorKind::QuotaExhausted);
    assert_eq!(
        store.account(account_id).expect("account").quota().access(),
        QuotaAccessState::Exhausted
    );
}

#[tokio::test]
async fn capacity_feedback_in_stream_only_counts_explicit_overload() {
    use gateway_core::provider_ports::{ProviderCooldownKind, ProviderCooldownPort as _};

    for (use_websocket, (code, message, expected_kind, scored)) in
        [false, true].into_iter().flat_map(|websocket| {
            [
                (
                    "server_is_overloaded",
                    "Selected model is at capacity. Please try a different model.",
                    ProviderErrorKind::UpstreamCapacityUnavailable,
                    true,
                ),
                (
                    "slow_down",
                    "slow_down",
                    ProviderErrorKind::UpstreamCapacityUnavailable,
                    true,
                ),
                (
                    "invalid_prompt",
                    "Invalid prompt: we've limited access to this content for safety reasons.",
                    ProviderErrorKind::InvalidRequest,
                    false,
                ),
                (
                    "server_error",
                    "An internal server error occurred.",
                    ProviderErrorKind::Unavailable,
                    true,
                ),
                (
                    "unknown_error",
                    "Unrecognized upstream failure.",
                    ProviderErrorKind::Unavailable,
                    false,
                ),
            ]
            .map(|case| (websocket, case))
        })
    {
        for semantic_output in [false, true] {
            let store = Arc::new(MemoryAccountStore::default());
            create_account(&store, "acct_provider_contract").await;
            let account = store.account("acct_provider_contract").expect("account");
            let cooldowns = Arc::new(MemoryCooldownPort::new());
            // 距离冻结阈值只差一次，验证非容量错误不会把可调度账号推入冷却。
            for _ in 0..11 {
                cooldowns
                    .record_capacity_failure(account.id(), Duration::from_secs(600), 20)
                    .await
                    .expect("seed capacity evidence");
            }
            let before = cooldowns.capacity_evidence(account.id());
            let mut events = vec![
                json!({"type": "response.created", "response": {"id": "resp_capacity", "model": "gpt-5.4"}}),
            ];
            if semantic_output {
                events.push(json!({"type": "response.output_text.delta", "output_index": 0, "content_index": 0, "delta": "hello"}));
            }
            let original = json!({"type": "response.failed", "response": {"id": "resp_capacity", "error": {"code": code, "message": message}}});
            events.push(original.clone());
            let (base_url, _http_server, websocket_server) = if use_websocket {
                let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
                let base_url = format!("http://{}", listener.local_addr().expect("address"));
                let server = tokio::spawn(async move {
                    let (socket, _) = listener.accept().await.expect("accept WebSocket");
                    let mut websocket = accept_codex_test_websocket(socket).await;
                    websocket.next().await.expect("request").expect("frame");
                    for event in events {
                        websocket
                            .send(Message::Text(event.to_string().into()))
                            .await
                            .expect("response");
                    }
                });
                (base_url, None, Some(server))
            } else {
                let server = MockServer::start().await;
                let frames = events
                    .iter()
                    .map(|event| {
                        format!(
                            "event: {}\ndata: {event}\n\n",
                            event["type"].as_str().expect("type")
                        )
                    })
                    .collect::<String>();
                Mock::given(method("POST"))
                    .and(path("/codex/responses"))
                    .respond_with(
                        ResponseTemplate::new(200).set_body_raw(frames, "text/event-stream"),
                    )
                    .expect(1)
                    .mount(&server)
                    .await;
                (server.uri(), Some(server), None)
            };
            let operation = if use_websocket {
                generate_operation()
            } else {
                http_generate_operation()
            };
            let (provider, pool) =
                provider_with_capacity_tracking(&store, base_url, Arc::clone(&cooldowns));
            let mut stream = provider
                .execute(
                    planned_request("openai", operation),
                    context("req_capacity_stream", CancellationToken::new()),
                )
                .await
                .expect("prepare stream");
            let mut client_events = Vec::new();
            let mut error = loop {
                match stream.next().await {
                    Some(Ok(event)) => {
                        if event.has_client_event() {
                            client_events.push(event);
                        }
                    }
                    Some(Err(error)) => break error,
                    None => panic!("expected upstream failure"),
                }
            };
            let capacity = expected_kind == ProviderErrorKind::UpstreamCapacityUnavailable;
            assert_eq!(
                error.kind(),
                expected_kind,
                "unexpected error for {code}, websocket={use_websocket}, semantic_output={semantic_output}: {error:?}"
            );
            assert_eq!(error.replay_is_safe(), capacity && !semantic_output);
            assert_eq!(error.upstream_status(), None);
            assert_eq!(
                error.pre_delivery_retry().is_some(),
                capacity && !semantic_output
            );
            assert_eq!(
                provider_openai::openai_failure_affects_account_score(&error),
                scored
            );
            let cooldown = cooldowns.read(account.id()).await.expect("read cooldown");
            if capacity {
                assert_eq!(
                    cooldowns
                        .capacity_evidence(account.id())
                        .map(|(count, _)| count),
                    Some(12)
                );
                assert_eq!(
                    cooldown.expect("capacity cooldown").kind(),
                    ProviderCooldownKind::CapacityFreezeProbe
                );
            } else {
                assert_eq!(
                    cooldowns.capacity_evidence(account.id()),
                    before,
                    "non-capacity error {code} changed capacity evidence"
                );
                assert!(
                    cooldown.is_none(),
                    "non-capacity error {code} froze the account"
                );
            }
            assert_eq!(
                serde_json::from_str::<Value>(
                    error.raw_upstream_error().expect("raw error").as_str()
                )
                .expect("JSON"),
                original
            );
            if !semantic_output {
                assert!(client_events.is_empty());
            }
            client_events.extend(error.take_atomic_client_events());
            let wire = client_events
                .last()
                .and_then(|event| event.wire_event())
                .expect("failure wire");
            assert_eq!(wire.data(), &original);
            if let Some(server) = websocket_server {
                server.await.expect("server");
            }
            pool.shutdown().await;
        }
    }
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
            plan_type: None,
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
    assert_eq!(
        presentation.service_tiers(),
        [ModelServiceTier::new(
            "priority",
            "Fast",
            "Priority processing."
        )]
    );

    let scope = FrozenAccountScope::new(
        Arc::new(RuntimeAccountDirectory::new(BTreeMap::from([(
            ProviderAccountId::new("acct_presentation").expect("account"),
            RuntimeAccount::new(
                ProviderKind::new("openai").expect("provider"),
                BTreeSet::new(),
            ),
        )]))),
        ClientRoutingScope::all_accounts(),
    );
    let native = provider
        .query_client_model_catalog(&scope, "codex", "0.154.0")
        .await
        .expect("native catalog")
        .expect("supported");
    let original: Value = serde_json::from_slice(OFFICIAL_FIXTURE).expect("fixture");
    let document: Value = serde_json::from_slice(match &native[0].content {
        gateway_core::routing::ProviderModelContent::Native(payload) => payload.body(),
        _ => panic!("expected native model"),
    })
    .expect("document");
    assert_eq!(document, original["models"][0]);
    assert!(
        server
            .received_requests()
            .await
            .expect("requests")
            .iter()
            .any(|request| request.url.query() == Some("client_version=0.154.0"))
    );
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

#[tokio::test]
async fn api_key_native_endpoints_preserve_bodies_headers_and_own_base_url() {
    for prefix in ["", "/v1", "/custom/v2"] {
        let upstream = MockServer::start().await;
        let oauth = MockServer::start().await;
        let store = Arc::new(MemoryAccountStore::default());
        store
            .seed_api_key(
                "acct_provider_contract",
                format!("{}{prefix}", upstream.uri()),
                provider_openai::credential::ApiKeyTransport::Http,
            )
            .await;
        let provider = provider_with_base_url(&store, oauth.uri());
        let body = br#"{ "model":"unlisted-model", "future":9007199254740993, "prompt":"first", "prompt":"last" }"#;
        let response = br#"{ "output":"opaque", "future":9007199254740993, "usage":{"input_tokens":3,"output_tokens":4,"total_tokens":7} }"#;
        for (index, endpoint) in ["/images/generations", "/images/edits", "/alpha/search"]
            .into_iter()
            .enumerate()
        {
            Mock::given(method("POST"))
                .and(path(format!("{prefix}{endpoint}")))
                .and(header("authorization", "Bearer sk-api-test-only"))
                .and(body_bytes(body.to_vec()))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header("x-upstream-feature", "opaque")
                        .set_body_raw(response.to_vec(), "application/json"),
                )
                .expect(1)
                .mount(&upstream)
                .await;
            let payload = RawJsonPayload::new("openai", Bytes::copy_from_slice(body)).unwrap();
            let operation = match index {
                0 => Operation::GenerateImage(ImageRequest::from_raw_json(
                    ImageRequestKind::Generation,
                    payload,
                )),
                1 => Operation::GenerateImage(ImageRequest::from_raw_json(
                    ImageRequestKind::Edit,
                    payload,
                )),
                _ => Operation::Search(StandaloneSearchRequest::from_raw_json(payload)),
            };
            let mut stream = provider
                .execute(
                    planned_provider_endpoint_request("openai", operation),
                    context(
                        &format!("req_api_endpoint_{index}"),
                        CancellationToken::new(),
                    ),
                )
                .await
                .expect("API account eligible without a model catalog");
            let mut raw = None;
            let mut header_preserved = false;
            let mut usage = false;
            while let Some(event) = stream.next().await {
                let event = event.expect("raw endpoint response");
                if let Some(body) = event.wire_event().and_then(|wire| wire.raw_json_body()) {
                    raw = Some(body.clone());
                }
                if let Some(observation) = event.response_observation() {
                    header_preserved |= observation.client_headers().iter().any(|header| {
                        header.name() == "x-upstream-feature"
                            && header.value().as_ref() == b"opaque"
                    });
                }
                usage |= event
                    .canonical_facts()
                    .iter()
                    .any(|fact| matches!(fact, GatewayEvent::Usage(_)));
            }
            assert_eq!(raw.as_deref(), Some(response.as_slice()));
            assert!(header_preserved);
            if index < 2 {
                assert!(usage, "image usage uses the shared accounting path");
            }
        }
        assert!(oauth.received_requests().await.unwrap().is_empty());
        for request in upstream.received_requests().await.unwrap() {
            for name in ["cookie", "chatgpt-account-id"] {
                assert!(!request.headers.contains_key(name));
            }
            assert_eq!(request.headers["originator"], "codex_cli_rs");
            assert_eq!(request.headers["version"], "0.144.0");
            assert_eq!(
                request.headers["user-agent"],
                wire_profile().snapshot().user_agent()
            );
        }
        upstream.verify().await;
    }
}

#[tokio::test]
async fn api_key_responses_forward_lite_memgen_and_native_compaction_without_catalog_gates() {
    let upstream = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_api_key(
            "acct_provider_contract",
            upstream.uri(),
            provider_openai::credential::ApiKeyTransport::Http,
        )
        .await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"data":[{"id":"different-model"}]})),
        )
        .expect(1)
        .mount(&upstream)
        .await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .and(header("x-openai-internal-codex-responses-lite", "true"))
        .and(header("x-openai-memgen-request", "true"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(CAPTURE_COMPLETED_SSE, "text/event-stream"),
        )
        .expect(1)
        .mount(&upstream)
        .await;
    let provider = provider(&store);
    provider.query_model_capabilities().await.unwrap();
    let body = json!({"model":"gpt-5.4","input":[{"type":"compaction_trigger"}],"reasoning":{"effort":"future-effort"},"tools":[{"type":"future-tool"}],"future_option":{"nested":true}});
    let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", body.as_object().unwrap().clone())
            .unwrap()
            .with_context(Map::from_iter([
                ("responses_lite".to_owned(), json!("true")),
                ("memgen_request".to_owned(), json!("true")),
            ])),
    ));
    let mut stream = provider
        .execute(
            planned_request("openai", operation),
            context("req_api_native_features", CancellationToken::new()),
        )
        .await
        .expect("upstream judges capabilities");
    while let Some(event) = stream.next().await {
        event.expect("response");
    }
    let requests = upstream.received_requests().await.unwrap();
    let request = requests
        .iter()
        .find(|request| request.method == "POST")
        .unwrap();
    let captured: Value = serde_json::from_slice(&request.body).unwrap();
    for field in ["input", "reasoning", "tools", "future_option"] {
        assert_eq!(captured[field], body[field]);
    }
    upstream.verify().await;
}

const API_KEY_DOWNSTREAM_HEADERS: &[&str] = &[
    "cf-future-proxy-field",
    "x-forwarded-for",
    "x-stainless-runtime",
    "origin",
    "referer",
    "sec-ch-ua",
    "sec-fetch-site",
    "x-grok-turn-idx",
    "x-xai-future-field",
    "session_id",
    "x-openai-actor-authorization",
    "authorization",
    "cookie",
    "chatgpt-account-id",
];

const API_KEY_BUSINESS_HEADERS: &[&str] = &[
    "session-id",
    "thread-id",
    "x-codex-future",
    "x-openai-internal-future",
];

fn generate_with_downstream_headers() -> Operation {
    let mut headers: Vec<_> = API_KEY_DOWNSTREAM_HEADERS
        .iter()
        .chain(API_KEY_BUSINESS_HEADERS)
        .map(|name| json!([name, STANDARD.encode(b"downstream-value")]))
        .collect();
    headers.push(json!(["x-business-extension", STANDARD.encode(b"keep")]));
    for name in ["user-agent", "originator", "version"] {
        headers.push(json!([name, STANDARD.encode(b"downstream-profile")]));
    }
    Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            json!({"model": "gpt-5.4", "input": "hello"})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap()
        .with_context(Map::from_iter([(
            "opaque_request_headers".to_owned(),
            json!(headers),
        )])),
    ))
}

#[tokio::test]
async fn api_key_default_http_uses_own_prefix_plain_json_and_only_own_authentication() {
    for prefix in ["", "/v1", "/custom/v2"] {
        let upstream = MockServer::start().await;
        let oauth = MockServer::start().await;
        let store = Arc::new(MemoryAccountStore::default());
        store
            .seed_api_key(
                "acct_provider_contract",
                format!("{}{prefix}", upstream.uri()),
                provider_openai::credential::ApiKeyTransport::Http,
            )
            .await;
        // 后台发现不协商客户端版本；客户端目录独立请求并按实际版本缓存。
        for query in [None, Some("client_version=1.0.0")] {
            Mock::given(method("GET"))
                .and(path(format!("{prefix}/models")))
                .and(move |request: &wiremock::Request| request.url.query() == query)
                .and(header("authorization", "Bearer sk-api-test-only"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(json!({"data":[{"id":"gpt-5.4"}]})),
                )
                .expect(1)
                .mount(&upstream)
                .await;
        }
        Mock::given(method("POST"))
            .and(path(format!("{prefix}/responses")))
            .and(header("authorization", "Bearer sk-api-test-only"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(CAPTURE_COMPLETED_SSE, "text/event-stream"),
            )
            .expect(1)
            .mount(&upstream)
            .await;
        let (provider, quota) = provider_and_quota_with_affinity_and_base_url_and_leases(
            &store,
            Arc::new(MemorySessionAffinity::default()),
            oauth.uri(),
            Arc::new(TestLeaseCoordinator::default()),
            DEFAULT_STREAM_MAX_RETRIES as u32,
        );
        provider
            .query_model_capabilities()
            .await
            .expect("standard API catalog");
        let catalog = provider
            .query_client_model_catalog(&contract_account_scope(), "codex", "1.0.0")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(catalog.len(), 1);
        assert!(matches!(
            catalog[0].content,
            gateway_core::routing::ProviderModelContent::Adapted(_)
        ));
        let mut stream = provider
            .execute(
                planned_request("openai", generate_with_downstream_headers()),
                context("req_api_http", CancellationToken::new()),
            )
            .await
            .expect("scheduled API account");
        while let Some(event) = stream.next().await {
            event.expect("completed API response");
        }
        quota.synchronize().await.expect("skip API account quota");
        assert!(oauth.received_requests().await.unwrap().is_empty());
        let requests = upstream.received_requests().await.unwrap();
        let request = requests
            .iter()
            .find(|request| request.method == "POST")
            .unwrap();
        assert!(!request.headers.contains_key("content-encoding"));
        for name in API_KEY_DOWNSTREAM_HEADERS
            .iter()
            .filter(|name| **name != "authorization")
        {
            assert!(!request.headers.contains_key(*name), "leaked {name}");
        }
        assert_eq!(request.headers["x-business-extension"], "keep");
        for name in API_KEY_BUSINESS_HEADERS {
            assert_eq!(request.headers[*name], "downstream-value", "lost {name}");
        }
        for header in ["cookie", "chatgpt-account-id"] {
            assert!(!request.headers.contains_key(header), "unexpected {header}");
        }
        assert_eq!(request.headers["originator"], "codex_cli_rs");
        assert_eq!(request.headers["version"], "0.144.0");
        assert_eq!(
            request.headers["user-agent"],
            wire_profile().snapshot().user_agent()
        );
        let model_requests = requests
            .iter()
            .filter(|request| request.method == "GET")
            .collect::<Vec<_>>();
        assert_eq!(model_requests.len(), 2);
        for query in [None, Some("client_version=1.0.0")] {
            let model_request = model_requests
                .iter()
                .find(|request| request.url.query() == query)
                .expect("separate background and client catalog requests");
            assert_eq!(
                model_request.headers["authorization"],
                "Bearer sk-api-test-only"
            );
            assert_eq!(
                model_request.headers["user-agent"],
                wire_profile().snapshot().user_agent()
            );
            assert_eq!(model_request.headers["originator"], "codex_cli_rs");
            assert_eq!(model_request.headers["version"], "0.144.0");
            for header in ["cookie", "chatgpt-account-id"] {
                assert!(
                    !model_request.headers.contains_key(header),
                    "unexpected catalog {header}"
                );
            }
        }
        let body: Value = serde_json::from_slice(&request.body).expect("ordinary JSON");
        assert_eq!(body["stream"], true);
        assert_eq!(body["model"], "gpt-5.4");
        upstream.verify().await;
    }
}

#[tokio::test]
async fn disabled_api_key_diagnostic_preserves_authentication_and_transport_constraints() {
    let upstream = MockServer::start().await;
    let oauth = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_provider_contract";
    store
        .seed_api_key(
            account_id,
            upstream.uri(),
            provider_openai::credential::ApiKeyTransport::Http,
        )
        .await;
    let account = store.account(account_id).expect("API account");
    store
        .set_enabled(account.id(), false)
        .await
        .expect("disable API account");
    Mock::given(method("POST"))
        .and(path("/responses"))
        .and(header("authorization", "Bearer sk-api-test-only"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(
                format!(
                    "event: response.created\ndata: {{\"type\":\"response.created\",\"response\":{{\"id\":\"resp_scope_capture\",\"model\":\"gpt-5.4\"}}}}\n\n{CAPTURE_COMPLETED_SSE}"
                ),
                "text/event-stream",
            ),
        )
        .expect(1)
        .mount(&upstream)
        .await;
    let provider = provider_with_base_url(&store, oauth.uri());
    let mut stream = provider
        .execute(
            planned_request("openai", generate_operation()),
            diagnostic_context("req_disabled_api_http", account_id),
        )
        .await
        .expect("disabled API account diagnostic");
    let mut completed = false;
    while let Some(event) = stream.next().await {
        completed |= event
            .expect("API response")
            .canonical_facts()
            .iter()
            .any(|event| matches!(event, GatewayEvent::Completed(_)));
    }
    assert!(completed);

    let warmup = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            json!({"model":"gpt-5.4","input":[],"store":false,"generate":false})
                .as_object()
                .expect("warmup object")
                .clone(),
        )
        .expect("warmup payload"),
    ));
    let search = Operation::Search(StandaloneSearchRequest::from_raw_json(
        RawJsonPayload::new(
            "openai",
            Bytes::from_static(br#"{"id":"disabled-api-search","commands":{}}"#),
        )
        .expect("search payload"),
    ));
    let result = provider
        .execute(
            planned_request("openai", warmup),
            diagnostic_context("req_disabled_api_restricted", account_id),
        )
        .await;
    let Err(error) = result else {
        panic!("diagnostic must preserve authentication and transport restrictions")
    };
    assert_eq!(error.kind(), ProviderErrorKind::NoEligibleAccount);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    Mock::given(method("POST"))
        .and(path("/alpha/search"))
        .and(header("authorization", "Bearer sk-api-test-only"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"output":"ok"})))
        .expect(1)
        .mount(&upstream)
        .await;
    let mut stream = provider
        .execute(
            planned_provider_endpoint_request("openai", search),
            diagnostic_context("req_disabled_api_search", account_id),
        )
        .await
        .expect("Search uses the same diagnostic account");
    while let Some(event) = stream.next().await {
        event.expect("Search response");
    }
    assert!(oauth.received_requests().await.unwrap().is_empty());
    assert_eq!(upstream.received_requests().await.unwrap().len(), 2);
    assert!(!store.account(account_id).expect("API account").enabled());
}

#[tokio::test]
async fn api_key_http_account_is_rejected_before_websocket_warmup_or_old_revision_continuation() {
    let upstream = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_api_key(
            "acct_provider_contract",
            upstream.uri(),
            provider_openai::credential::ApiKeyTransport::Http,
        )
        .await;
    let provider = provider(&store);
    let warmup = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            json!({"model":"gpt-5.4","input":[],"store":false,"generate":false})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap(),
    ));
    assert!(
        provider
            .execute(
                planned_request("openai", warmup),
                diagnostic_context("req_api_warmup", "acct_provider_contract")
            )
            .await
            .is_err()
    );
    let Operation::Generate(generate) = generate_operation() else {
        panic!("generate")
    };
    let stale = generate.with_provider_session_state(ProviderSessionState::new("openai", json!({"account_id":"acct_provider_contract","conversation_id":"old","credential_revision":9,"continuation_scope":"persisted"}).as_object().unwrap().clone()).unwrap());
    assert!(
        provider
            .execute(
                planned_request("openai", Operation::Generate(stale)),
                diagnostic_context("req_api_stale", "acct_provider_contract")
            )
            .await
            .is_err()
    );
    assert!(upstream.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn api_key_websocket_uses_api_path_and_bearer_without_oauth_identity() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/custom/v2", listener.local_addr().unwrap());
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_api_key(
            "acct_provider_contract",
            base,
            provider_openai::credential::ApiKeyTransport::PreferWebsocket,
        )
        .await;
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket =
            crate::transport::accept_codex_test_websocket_with(stream, |request, _| {
                assert_eq!(request.uri().path(), "/custom/v2/responses");
                assert_eq!(
                    request.headers()["authorization"],
                    "Bearer sk-api-test-only"
                );
                assert!(!request.headers().contains_key("chatgpt-account-id"));
                assert!(!request.headers().contains_key("cookie"));
                assert_eq!(request.headers()["originator"], "codex_cli_rs");
                assert_eq!(request.headers()["version"], "0.144.0");
                assert_eq!(
                    request.headers()["user-agent"],
                    wire_profile().snapshot().user_agent()
                );
                for name in API_KEY_DOWNSTREAM_HEADERS
                    .iter()
                    .filter(|name| **name != "authorization")
                {
                    assert!(!request.headers().contains_key(*name), "leaked {name}");
                }
                assert_eq!(request.headers()["x-business-extension"], "keep");
                for name in API_KEY_BUSINESS_HEADERS {
                    assert_eq!(request.headers()[*name], "downstream-value", "lost {name}");
                }
            })
            .await;
        let frame = websocket.next().await.unwrap().unwrap();
        let payload: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
        assert_eq!(payload["type"], "response.create");
        websocket.send(Message::Text(json!({"type":"response.completed","response":{"id":"resp_api_ws","model":"gpt-5.4","status":"completed","output":[],"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}).to_string().into())).await.unwrap();
    });
    let provider = provider(&store);
    let mut stream = provider
        .execute(
            planned_request("openai", generate_with_downstream_headers()),
            diagnostic_context("req_api_ws", "acct_provider_contract"),
        )
        .await
        .unwrap();
    while let Some(event) = stream.next().await {
        event.expect("API WebSocket completion");
    }
    timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}

fn quota_continuation_operation(use_websocket: bool) -> Operation {
    Operation::Generate(
        GenerateRequest::from_protocol_payload(
            ProtocolPayload::json_object(
                "openai",
                json!({
                    "model": "gpt-5.4", "input": [{"role":"user","content":"continue"}],
                    "previous_response_id": "resp_previous",
                    "session_id": "quota-replay", "thread_id": "turn",
                })
                .as_object()
                .unwrap()
                .clone(),
            )
            .unwrap()
            .with_context(Map::from_iter([(
                "use_websocket".to_owned(),
                json!(use_websocket),
            )])),
        )
        .with_provider_session_state(
            generate_with_persisted_session_context(
                "acct_provider_contract",
                "conversation-quota",
                "quota-replay",
                "turn",
            )
            .provider_session_state("openai")
            .unwrap()
            .clone(),
        ),
    )
}

#[tokio::test]
async fn quota_continuation_opening_rejection_projects_replay_without_losing_upstream_facts() {
    for use_websocket in [false, true] {
        for (status, code, projects_replay) in [
            (429, "usage_limit_reached", true),
            (402, "insufficient_quota", true),
            (429, "quota_exceeded", false),
            (429, "rate_limit_exceeded", false),
            (429, "slow_down", false),
            (429, "unknown_error", false),
        ] {
            let store = Arc::new(MemoryAccountStore::default());
            create_account(&store, "acct_provider_contract").await;
            let server = MockServer::start().await;
            let original =
                json!({"error": {"type": code, "code": code, "message": "upstream rejection"}});
            Mock::given(method(if use_websocket { "GET" } else { "POST" }))
                .and(path("/codex/responses"))
                .respond_with(
                    ResponseTemplate::new(status)
                        .insert_header("retry-after", "129600")
                        .insert_header("x-request-id", "req-upstream-quota")
                        .set_body_json(&original),
                )
                .expect(1)
                .mount(&server)
                .await;
            let mut stream = provider_with_base_url_and_retry_budget(&store, server.uri(), 0)
                .execute(
                    planned_request("openai", quota_continuation_operation(use_websocket)),
                    context("req_quota_replay", CancellationToken::new())
                        .with_continuation_attempt(ContinuationAttempt::Native),
                )
                .await
                .expect("prepare continuation");
            let mut error = loop {
                match stream.next().await {
                    Some(Ok(event)) => assert!(!event.has_client_event()),
                    Some(Err(error)) => break error,
                    None => panic!("expected rejection"),
                }
            };
            assert_eq!(error.upstream_status(), Some(status));
            assert_eq!(error.retry_after(), Some(Duration::from_secs(129_600)));
            assert_eq!(
                serde_json::from_str::<Value>(error.raw_upstream_error().unwrap().as_str())
                    .unwrap(),
                original
            );
            assert!(error.take_atomic_client_events().is_empty());
            let response = error.client_visible_upstream_response().unwrap();
            let client: Value = serde_json::from_slice(response.body()).unwrap();
            if projects_replay {
                assert_eq!(error.upstream_code().map(|code| code.as_str()), Some(code));
                assert_eq!(error.kind(), ProviderErrorKind::QuotaExhausted);
                assert_eq!(
                    error.continuation_recovery_disposition(),
                    Some(ContinuationRecoveryDisposition::ClientReplayRequired)
                );
                assert_eq!(response.status(), 400);
                assert_eq!(client["error"]["code"], "previous_response_not_found");
                assert!(
                    !response
                        .headers()
                        .iter()
                        .any(|header| header.name() == "retry-after")
                );
                assert!(
                    response
                        .headers()
                        .iter()
                        .any(|header| header.name() == "x-request-id"
                            && header.value().as_ref() == b"req-upstream-quota")
                );
                assert_eq!(
                    store
                        .account("acct_provider_contract")
                        .unwrap()
                        .quota()
                        .access(),
                    QuotaAccessState::Exhausted
                );
            } else {
                assert_ne!(
                    error.continuation_recovery_disposition(),
                    Some(ContinuationRecoveryDisposition::ClientReplayRequired)
                );
                assert_eq!(response.status(), 429);
                assert_eq!(client, original);
            }
        }
    }
}

#[tokio::test]
async fn quota_continuation_stream_rejection_only_projects_before_delivery() {
    for use_websocket in [false, true] {
        for semantic_output in [false, true] {
            for native_continuation in [false, true] {
                let store = Arc::new(MemoryAccountStore::default());
                create_account(&store, "acct_provider_contract").await;
                let mut events = vec![
                    json!({"type":"response.created","response":{"id":"resp_quota","model":"gpt-5.4"}}),
                ];
                if semantic_output {
                    events.push(json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"hello"}));
                }
                let original = json!({"type":"response.failed","response":{"id":"resp_quota","error":{"code":"usage_limit_reached","message":"You have reached your usage limit."}}});
                events.push(original.clone());
                let (base_url, _http_server, websocket_server) = if use_websocket {
                    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                    let base_url = format!("http://{}", listener.local_addr().unwrap());
                    let server = tokio::spawn(async move {
                        let (socket, _) = listener.accept().await.unwrap();
                        let mut ws = accept_codex_test_websocket(socket).await;
                        ws.next().await.unwrap().unwrap();
                        for event in events {
                            ws.send(Message::Text(event.to_string().into()))
                                .await
                                .unwrap();
                        }
                    });
                    (base_url, None, Some(server))
                } else {
                    let server = MockServer::start().await;
                    let frames = events
                        .iter()
                        .map(|event| {
                            format!(
                                "event: {}\ndata: {event}\n\n",
                                event["type"].as_str().unwrap()
                            )
                        })
                        .collect::<String>();
                    Mock::given(method("POST"))
                        .and(path("/codex/responses"))
                        .respond_with(
                            ResponseTemplate::new(200)
                                .insert_header("x-request-id", "req-quota-stream")
                                .set_body_raw(frames, "text/event-stream"),
                        )
                        .expect(1)
                        .mount(&server)
                        .await;
                    (server.uri(), Some(server), None)
                };
                let operation = if native_continuation {
                    quota_continuation_operation(use_websocket)
                } else if use_websocket {
                    generate_operation()
                } else {
                    http_generate_operation()
                };
                let attempt = context("req_quota_stream", CancellationToken::new())
                    .with_continuation_attempt(if native_continuation {
                        ContinuationAttempt::Native
                    } else {
                        ContinuationAttempt::None
                    });
                let mut stream = provider_with_base_url(&store, base_url)
                    .execute(planned_request("openai", operation), attempt)
                    .await
                    .unwrap();
                let mut client_events = Vec::new();
                let mut error = loop {
                    match stream.next().await {
                        Some(Ok(event)) => {
                            if event.has_client_event() {
                                client_events.push(event);
                            }
                        }
                        Some(Err(error)) => break error,
                        None => panic!("expected quota failure"),
                    }
                };
                assert_eq!(error.kind(), ProviderErrorKind::QuotaExhausted);
                assert_eq!(error.send_state(), UpstreamSendState::Sent);
                assert_eq!(error.replay_is_safe(), !semantic_output);
                assert_eq!(
                    serde_json::from_str::<Value>(error.raw_upstream_error().unwrap().as_str())
                        .unwrap(),
                    original
                );
                client_events.extend(error.take_atomic_client_events());
                if native_continuation && !semantic_output {
                    assert!(
                        client_events.is_empty(),
                        "raw quota error must not override replay projection"
                    );
                    assert_eq!(
                        error.continuation_recovery_disposition(),
                        Some(ContinuationRecoveryDisposition::ClientReplayRequired)
                    );
                    assert_eq!(
                        error.client_visible_upstream_error().unwrap().code(),
                        Some("previous_response_not_found")
                    );
                    if !use_websocket {
                        assert_eq!(
                            error.upstream_request_id().unwrap().as_str(),
                            "req-quota-stream"
                        );
                        assert!(
                            error
                                .client_visible_upstream_response()
                                .unwrap()
                                .headers()
                                .iter()
                                .any(|header| header.name() == "x-request-id"
                                    && header.value().as_ref() == b"req-quota-stream")
                        );
                    }
                } else {
                    assert_ne!(
                        error.continuation_recovery_disposition(),
                        Some(ContinuationRecoveryDisposition::ClientReplayRequired)
                    );
                    assert_eq!(
                        client_events.last().unwrap().wire_event().unwrap().data(),
                        &original
                    );
                }
                if let Some(server) = websocket_server {
                    server.await.unwrap();
                }
            }
        }
    }
}

#[tokio::test]
async fn quota_continuation_after_structural_commit_preserves_original_failure() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let original = json!({"type":"response.failed","response":{"id":"resp_quota","error":{"code":"usage_limit_reached","message":"limit reached"}}});
    let (base_url, release, _, server) = paused_chunked_sse_server(
        "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_quota\",\"model\":\"gpt-5.4\"}}\n\n".to_owned(),
        format!("event: response.failed\ndata: {original}\n\n"),
    ).await;
    let mut stream = provider_with_base_url(&store, base_url)
        .execute(
            planned_request("openai", quota_continuation_operation(false)),
            context("req_quota_commit", CancellationToken::new())
                .with_continuation_attempt(ContinuationAttempt::Native),
        )
        .await
        .unwrap();
    timeout(Duration::from_secs(3), async {
        while let Some(event) = stream.next().await {
            if event.unwrap().has_client_event() {
                return;
            }
        }
        panic!("expected grace commit");
    })
    .await
    .expect("bounded grace");
    release.send(()).unwrap();
    let mut delivered_failure = false;
    let error = loop {
        match stream.next().await {
            Some(Ok(event)) => {
                delivered_failure |= event
                    .wire_event()
                    .is_some_and(|wire| wire.data() == &original);
            }
            Some(Err(error)) => break error,
            None => panic!("expected failure"),
        }
    };
    assert!(delivered_failure);
    assert!(!error.replay_is_safe());
    assert_ne!(
        error.continuation_recovery_disposition(),
        Some(ContinuationRecoveryDisposition::ClientReplayRequired)
    );
    server.await.unwrap();
}

#[tokio::test]
async fn quota_continuation_full_client_replay_selects_another_account() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let full_input = json!([
        {"role":"user","content":"hello"},
        {"role":"assistant","content":"first answer"},
        {"role":"user","content":"continue"},
    ]);
    let expected_input = full_input.clone();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut ws = accept_codex_test_websocket(socket).await;
        ws.next().await.unwrap().unwrap();
        for event in [
            json!({"type":"response.created","response":{"id":"resp_previous","model":"gpt-5.4"}}),
            json!({"type":"response.completed","response":{"id":"resp_previous","model":"gpt-5.4","status":"completed","output":[]}}),
        ] {
            ws.send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        let delta: Value =
            serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(delta["previous_response_id"], "resp_previous");
        assert_eq!(
            delta["input"],
            json!([{"role":"user","content":"continue"}])
        );
        ws.send(Message::Text(json!({"type":"error","status":429,"error":{"type":"usage_limit_reached","code":"usage_limit_reached","message":"You have reached your usage limit."}}).to_string().into())).await.unwrap();
        let (socket, _) = listener.accept().await.unwrap();
        let mut ws = accept_codex_test_websocket(socket).await;
        let replay: Value =
            serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert!(replay.get("previous_response_id").is_none());
        assert_eq!(replay["input"], expected_input);
        for event in [
            json!({"type":"response.created","response":{"id":"resp_replayed","model":"gpt-5.4"}}),
            json!({"type":"response.completed","response":{"id":"resp_replayed","model":"gpt-5.4","status":"completed","output":[]}}),
        ] {
            ws.send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
    });
    let provider = provider_with_base_url(&store, base_url);
    let first = Operation::Generate(generate_with_persisted_session_context(
        "acct_provider_contract",
        "conversation-quota",
        "quota-replay",
        "turn",
    ));
    let mut stream = provider
        .execute(
            planned_request("openai", first),
            context("req_quota_first", CancellationToken::new()),
        )
        .await
        .unwrap();
    assert_eq!(
        stream.metadata().provider_account_id().as_str(),
        "acct_provider_contract"
    );
    while let Some(event) = stream.next().await {
        event.unwrap();
    }
    drop(stream);
    let mut stream = provider
        .execute(
            planned_request("openai", quota_continuation_operation(true)),
            context("req_quota_delta", CancellationToken::new())
                .with_continuation_attempt(ContinuationAttempt::Native),
        )
        .await
        .unwrap();
    let mut error = loop {
        match stream.next().await {
            Some(Ok(event)) => assert!(!event.has_client_event()),
            Some(Err(error)) => break error,
            None => panic!("expected quota rejection"),
        }
    };
    drop(stream);
    assert_eq!(
        error.client_visible_upstream_error().unwrap().code(),
        Some("previous_response_not_found")
    );
    assert!(error.take_atomic_client_events().is_empty());
    assert_eq!(
        store
            .account("acct_provider_contract")
            .unwrap()
            .quota()
            .access(),
        QuotaAccessState::Exhausted
    );
    create_account(&store, "acct_affinity_switch_b").await;
    // 客户端重建完整历史，保留同一会话标识；选号必须跳过刚刚耗尽的原账号。
    let replay = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", json!({"model":"gpt-5.4","session_id":"quota-replay","thread_id":"turn","input":full_input}).as_object().unwrap().clone()).unwrap(),
    ));
    let mut stream = provider
        .execute(
            planned_request("openai", replay),
            context("req_quota_full", CancellationToken::new()),
        )
        .await
        .unwrap();
    assert_eq!(
        stream.metadata().provider_account_id().as_str(),
        "acct_affinity_switch_b"
    );
    let mut completed = false;
    while let Some(event) = stream.next().await {
        completed |= event
            .unwrap()
            .canonical_facts()
            .iter()
            .any(|fact| matches!(fact, GatewayEvent::Completed(_)));
    }
    assert!(completed);
    timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn fast_policy_changes_preserve_the_websocket_continuation_and_meter_each_outbound_tier() {
    const ACCOUNT: &str = "acct_provider_contract";
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, ACCOUNT).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut websocket =
            crate::transport::accept_codex_test_websocket_with(socket, |request, response| {
                // 提示头只在首次握手发送；后续档位由各自 response.create 正文指定。
                assert_eq!(
                    request.headers()["x-codex-routing-hint"],
                    "model=gpt-5.4;tier=priority"
                );
                response.headers_mut().insert(
                    "sec-websocket-extensions",
                    "permessage-deflate".parse().unwrap(),
                );
            })
            .await;
        for (index, tier) in ["priority", "default", "priority"].into_iter().enumerate() {
            let message = websocket.next().await.unwrap().unwrap();
            let frame: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
            assert_eq!(frame["type"], "response.create");
            assert_eq!(frame["service_tier"], tier);
            assert_eq!(
                frame.get("previous_response_id").cloned(),
                index
                    .checked_sub(1)
                    .map(|previous| json!(format!("resp_fast_turn_{previous}")))
            );
            websocket.send(Message::Text(json!({
                "type":"response.created", "response":{"id":format!("resp_fast_turn_{index}"),"model":"gpt-5.4"}
            }).to_string().into())).await.unwrap();
            websocket.send(Message::Text(json!({
                "type":"response.completed", "response":{
                    "id":format!("resp_fast_turn_{index}"), "model":"gpt-5.4", "status":"completed", "output":[],
                    "service_tier":"priority", "usage":{"input_tokens":100,"output_tokens":10,"input_tokens_details":{"cached_tokens":25,"cache_write_tokens":0},"total_tokens":110}
                }
            }).to_string().into())).await.unwrap();
        }
        assert!(
            timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "all three turns must use the same upstream connection"
        );
    });
    let provider = provider_with_base_url(&store, base_url);
    let mut session_state = ProviderSessionState::new(
        "openai",
        Map::from_iter([
            ("account_id".to_owned(), json!(ACCOUNT)),
            (
                "conversation_id".to_owned(),
                json!("fast-policy-continuation"),
            ),
            ("continuation_scope".to_owned(), json!("connection_local")),
        ]),
    )
    .unwrap();
    for (index, disable_fast) in [false, true, false].into_iter().enumerate() {
        let previous = index
            .checked_sub(1)
            .map(|index| format!("resp_fast_turn_{index}"));
        let mut body =
            json!({"model":"gpt-5.4","input":"next turn","service_tier":"priority","store":false});
        if let Some(previous) = &previous {
            body["previous_response_id"] = json!(previous);
        }
        let operation = Operation::Generate(
            GenerateRequest::from_protocol_payload(
                ProtocolPayload::json_object("openai", body.as_object().unwrap().clone())
                    .unwrap()
                    // 与客户端 WebSocket 一致，首轮也禁止按快路径预算降级到 HTTP。
                    .with_context(Map::from_iter([
                        ("use_websocket".to_owned(), json!(true)),
                        (
                            "downstream_websocket_connection_id".to_owned(),
                            json!("ws_fast_policy_continuation"),
                        ),
                    ])),
            )
            .with_provider_session_state(session_state.clone()),
        );
        let account = ProviderAccountId::new(ACCOUNT).unwrap();
        let provider_kind = ProviderKind::new("openai").unwrap();
        let key = ClientApiKeyId::new("key_openai_contract").unwrap();
        let binding = previous.as_ref().map(|previous| {
            ContinuationBinding::Pinned(NativeContinuationPin::new(
                PreviousResponseId::new(previous),
                PreviousResponseId::new(previous),
                key.clone(),
                provider_kind.clone(),
                account.clone(),
            ))
        });
        let context = AttemptContext::new(
            RequestAttemptContext::new(
                ModelRequestId::new(format!("req_fast_turn_{index}")).unwrap(),
                key,
            )
            .with_disable_fast(disable_fast),
            NonZeroU32::new(1).unwrap(),
            SystemTime::now() + Duration::from_secs(30),
            account_policy(),
            AccountAttemptContext::new(
                BTreeSet::new(),
                None,
                Some(ProviderAccountStateOwner::new(provider_kind, account)),
            )
            .with_account_scope(contract_account_scope()),
            binding,
            CancellationToken::new(),
        )
        .with_continuation_attempt(if previous.is_some() {
            ContinuationAttempt::Native
        } else {
            ContinuationAttempt::None
        });
        let mut stream = provider
            .execute(planned_request("openai", operation), context)
            .await
            .unwrap();
        let mut costs = Vec::new();
        let mut pool = None;
        let mut observed_tier = None;
        while let Some(event) = timeout(Duration::from_secs(5), stream.next())
            .await
            .unwrap()
        {
            let event = event.unwrap();
            if let Some(update) = event.session_update() {
                session_state = update.clone();
            }
            if let Some(observation) = event.response_observation() {
                pool = observation.websocket_pool().or(pool);
                observed_tier = observation
                    .service_tier()
                    .map(str::to_owned)
                    .or(observed_tier);
            }
            for fact in event.canonical_facts() {
                if let GatewayEvent::CalculatedCost(cost) = fact {
                    costs.push(cost.total().amount().scaled());
                }
            }
        }
        assert_eq!(
            pool,
            Some(if index == 0 {
                WebSocketPoolKind::New
            } else {
                WebSocketPoolKind::Reuse
            })
        );
        assert_eq!(
            observed_tier.as_deref(),
            Some(if disable_fast { "default" } else { "priority" })
        );
        assert_eq!(
            costs,
            vec![if disable_fast { 3_437_500 } else { 6_875_000 }]
        );
    }
    server.await.unwrap();
}
