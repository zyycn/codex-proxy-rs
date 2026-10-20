//! 验证插件上游适配的认证、续接、事件交付与计费边界

use std::{
    num::NonZeroU32,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime},
};

use bytes::Bytes;
use futures::{StreamExt as _, future::BoxFuture};
use gateway_admin::model::plugins::instances::{PluginCapabilityBinding, PluginFailurePolicy};
use gateway_core::{
    account::{AccountSelectionPolicy, CredentialRevision, OutboundProxy, ProviderAccountId},
    engine::{
        AccountAttemptContext, AttemptContext, ModelRequestId, RequestAttemptContext,
        middleware::MiddlewareHeader,
        provider::{EventStream, ProviderCallMetadata},
        upstream_adapter::{UpstreamAccountConnection, UpstreamAdapterInvocation},
    },
    error::{ProviderError, ProviderErrorKind},
    event::GatewayEvent,
    identity::ProviderKind,
    lifecycle::CancellationToken,
    metering::{CalculatedCost, Usage},
    operation::{GenerateRequest, Operation, ProtocolPayload, ProviderSessionState},
    policy::ClientApiKeyId,
    routing::extensions::{ExtensionPreparationPort, ExtensionSetReference},
    routing::{ConfigRevision, UpstreamModelId},
    upstream::{UpstreamSendState, UpstreamTransport},
};
use gateway_plugin_sdk::{Capability, Contributions, Stage};
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

struct Account {
    id: ProviderAccountId,
    revision: u64,
    failures: AtomicUsize,
}
impl Account {
    fn new() -> Arc<Self> {
        Self::with_identity("acct_one", 1)
    }
    fn with_identity(id: &str, revision: u64) -> Arc<Self> {
        Arc::new(Self {
            id: ProviderAccountId::new(id).unwrap(),
            revision,
            failures: AtomicUsize::new(0),
        })
    }
}
impl UpstreamAccountConnection for Account {
    fn account_id(&self) -> &ProviderAccountId {
        &self.id
    }
    fn credential_revision(&self) -> CredentialRevision {
        CredentialRevision::new(self.revision).unwrap()
    }
    fn authentication_kind(&self) -> &str {
        "oauth"
    }
    fn outbound_proxy(&self) -> Option<&OutboundProxy> {
        None
    }
    fn authorization(&self) -> Result<Vec<MiddlewareHeader>, ProviderError> {
        Ok(vec![MiddlewareHeader::new(
            "authorization",
            Bytes::from_static(b"Bearer fixture-native-token"),
        )])
    }
    fn calculate_cost(&self, _: Option<&str>, usage: &Usage) -> Option<CalculatedCost> {
        assert_eq!(usage.input_tokens, Some(7));
        CalculatedCost::from_usd_ticks(123).ok()
    }
    fn record_failure(&self, error: ProviderError) -> BoxFuture<'_, ProviderError> {
        self.failures.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { error })
    }
}

fn configuration(base: &str, streaming: bool) -> Value {
    json!({
        "upstream_registration":{"adapters":[{"id":"example","provider":"openai","base_url":format!("{base}/adapter/"),"paths":[{"path":"responses","purpose":"inference"}],"authentication_kinds":["oauth"],"transport":if streaming {"http_sse"} else {"http_json"},"protocol":"openai"}]},
        "upstream_callbacks":[{"method":if streaming {"host.upstream.http.do_stream"} else {"host.upstream.http.do"},"params":{"method":"POST","path":"responses","headers":[["content-type",b"application/json".to_vec()]]},"body":"request"}],
        "upstream_events":[
            {"event":{"facts":[{"type":"started","id":"response-one","model":"native-model"}]}},
            {"event":{"facts":[{"type":"usage","usage":{"input_tokens":7,"output_tokens":2}},{"type":"completed","id":"response-one","model":"native-model","reason":"stop"}]},"wire":if streaming {"sse"} else {"http"},"continuation":{"scope":"persisted","upstream_response_id":"private-upstream-id","state":{"cursor":"opaque"}}}
        ]
    })
}

async fn setup(
    config: Value,
) -> (
    tempfile::TempDir,
    Arc<super::Store>,
    gateway_plugin_runtime::PluginRuntime,
) {
    setup_with_limits(config, gateway_plugin_runtime::RpcLimits::default()).await
}

async fn setup_with_limits(
    config: Value,
    limits: gateway_plugin_runtime::RpcLimits,
) -> (
    tempfile::TempDir,
    Arc<super::Store>,
    gateway_plugin_runtime::PluginRuntime,
) {
    let contributes = Contributions::from([crate::support::contribution(
        Capability::UpstreamAdapter,
        vec![Stage::Upstream],
        vec!["openai".into()],
        vec!["openai".into()],
    )]);
    let (cache, store, runtime) =
        super::setup_with_limits(contributes, Default::default(), limits).await;
    {
        let mut snapshot = store.snapshot.lock().unwrap();
        let instance = &mut snapshot.instances[0];
        instance.configuration = config;
        instance.bindings = vec![PluginCapabilityBinding {
            contribution: "test.example.upstreamAdapter".into(),
            stage: "upstream".into(),
            order: 0,
            failure_policy: PluginFailurePolicy::Reject,
            client_key_ids: vec![],
            account_group_ids: vec![],
            provider_ids: vec!["openai".into()],
            models: vec![],
            event: None,
            identity_bindings: vec![],
        }];
    }
    (cache, store, runtime)
}

fn context(
    runtime: &gateway_plugin_runtime::PluginRuntime,
    generation: &ExtensionSetReference,
    key: &str,
) -> AttemptContext {
    context_with_fast_mode(
        runtime,
        generation,
        key,
        gateway_core::account::FastMode::Default,
    )
}

fn context_with_fast_mode(
    runtime: &gateway_plugin_runtime::PluginRuntime,
    generation: &ExtensionSetReference,
    key: &str,
    mode: gateway_core::account::FastMode,
) -> AttemptContext {
    context_with_body_limit(runtime, generation, key, mode, 64 * 1024 * 1024)
}

fn context_with_body_limit(
    runtime: &gateway_plugin_runtime::PluginRuntime,
    generation: &ExtensionSetReference,
    key: &str,
    mode: gateway_core::account::FastMode,
    bytes: usize,
) -> AttemptContext {
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_one").unwrap(),
            ClientApiKeyId::new(key).unwrap(),
        )
        .with_upstream_adapters(runtime.execution_registry().upstream_adapters(generation))
        .with_fast_mode(mode)
        .with_request_body_limit(bytes),
        NonZeroU32::new(1).unwrap(),
        SystemTime::now() + Duration::from_secs(5),
        AccountSelectionPolicy::new(
            gateway_core::account::RotationStrategy::RoundRobin,
            std::num::NonZeroU32::new(1).unwrap(),
        ),
        AccountAttemptContext::new(Default::default(), None, None),
        None,
        CancellationToken::new(),
    )
}

fn execute(
    runtime: &gateway_plugin_runtime::PluginRuntime,
    generation: &ExtensionSetReference,
    account: Arc<Account>,
    key: &str,
    previous: Option<ProviderSessionState>,
) -> EventStream {
    let context = context(runtime, generation, key);
    execute_with_context(account, context, previous)
}

fn execute_with_context(
    account: Arc<Account>,
    context: AttemptContext,
    previous: Option<ProviderSessionState>,
) -> EventStream {
    let provider = ProviderKind::new("openai").unwrap();
    let model = UpstreamModelId::new("native-model").unwrap();
    let adapter = context
        .upstream_adapter(&provider, &model)
        .unwrap()
        .unwrap();
    let metadata = ProviderCallMetadata::new(
        provider,
        model,
        account.id.clone(),
        UpstreamTransport::new(adapter.transport()).unwrap(),
    );
    let mut operation = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            json!({"model":"native-model","input":"hello"})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap(),
    ));
    if let Some(previous) = previous {
        operation = operation.with_provider_session_state(previous);
    }
    adapter.execute(UpstreamAdapterInvocation {
        operation,
        headers: vec![],
        context,
        metadata,
        account,
    })
}

#[tokio::test]
async fn adapter_preserves_three_state_fast_in_request_metadata() {
    use gateway_core::account::FastMode;
    for mode in [FastMode::Default, FastMode::Enabled, FastMode::Disabled] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/adapter/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount(&server)
            .await;
        let mut config = configuration(&server.uri(), false);
        config["expected_fast_mode"] = json!(mode.as_str());
        let (cache, _, runtime) = setup(config).await;
        let generation =
            ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
                .await
                .unwrap();
        let mut stream = execute_with_context(
            Account::new(),
            context_with_fast_mode(&runtime, &generation, "key-one", mode),
            None,
        );
        let mut completed = 0;
        while let Some(event) = stream.next().await {
            completed += event
                .unwrap()
                .canonical_facts()
                .iter()
                .filter(|fact| matches!(fact, GatewayEvent::Completed(_)))
                .count();
        }
        assert_eq!(completed, 1);
        drop(stream);
        drop(generation);
        runtime.shutdown().await;
        super::wait_until_empty(cache.path()).await;
    }
}

#[tokio::test]
async fn adapter_http_and_sse_are_cold_authenticated_and_settled_once() {
    for streaming in [false, true] {
        let server = MockServer::start().await;
        let body = if streaming {
            "data: {\"id\":\"response-one\"}\n\n"
        } else {
            "{\"id\":\"response-one\"}"
        };
        Mock::given(method("POST"))
            .and(path("/adapter/responses"))
            .and(header("authorization", "Bearer fixture-native-token"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .expect(1)
            .mount(&server)
            .await;
        let (cache, _, runtime) = setup(configuration(&server.uri(), streaming)).await;
        let generation =
            ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
                .await
                .unwrap();
        let account = Account::new();
        let mut stream = execute(&runtime, &generation, Arc::clone(&account), "key-one", None);
        assert!(
            server.received_requests().await.unwrap().is_empty(),
            "构造冷流不能先发送业务请求"
        );
        let mut costs = 0;
        let mut completed = 0;
        let mut state = None;
        while let Some(event) = stream.next().await {
            let mut event = event.unwrap();
            for fact in event.canonical_facts() {
                costs += usize::from(matches!(fact, GatewayEvent::CalculatedCost(_)));
                completed += usize::from(matches!(fact, GatewayEvent::Completed(_)));
            }
            if event.session_update().is_some() {
                state = event.take_session_update();
            }
        }
        assert_eq!((costs, completed), (1, 1));
        let state = state.unwrap();
        assert_eq!(state.extension_owner().unwrap().adapter_id, "example");
        assert_eq!(account.failures.load(Ordering::SeqCst), 0);
        drop(stream);
        drop(generation);
        runtime.shutdown().await;
        super::wait_until_empty(cache.path()).await;
    }
}

#[tokio::test]
async fn adapter_sse_outlives_rpc_budget_and_cancellation_closes_its_resources() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    for cancel in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (started, receiving) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            let head_end = loop {
                if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    break end + 4;
                }
                let count = socket.read(&mut buffer).await.unwrap();
                assert!(count > 0 && request.len() < 16 * 1024);
                request.extend_from_slice(&buffer[..count]);
            };
            let length: usize = std::str::from_utf8(&request[..head_end])
                .unwrap()
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .unwrap()
                .1
                .trim()
                .parse()
                .unwrap();
            while request.len() < head_end + length {
                let count = socket.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                request.extend_from_slice(&buffer[..count]);
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n: heartbeat\n\n")
                .await
                .unwrap();
            started.send(()).unwrap();
            for _ in 0..6 {
                tokio::select! {
                    result = socket.read(&mut buffer) => {
                        assert_eq!(result.unwrap(), 0);
                        return false;
                    }
                    () = tokio::time::sleep(Duration::from_millis(200)) => {}
                }
                if socket.write_all(b": heartbeat\n\n").await.is_err() {
                    return false;
                }
            }
            socket.shutdown().await.unwrap();
            true
        });
        let mut config = configuration(&base, true);
        // 此夹具汇总 HTTP 正文后产出规范事件，不把多个心跳伪装成一个 SSE 帧
        config["upstream_events"][1]
            .as_object_mut()
            .unwrap()
            .remove("wire");
        let (cache, _, runtime) = setup_with_limits(
            config,
            gateway_plugin_runtime::RpcLimits {
                maximum_call_timeout: Duration::from_millis(500),
                ..Default::default()
            },
        )
        .await;
        let generation =
            ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
                .await
                .unwrap();
        let account = Account::new();
        let context = context(&runtime, &generation, "key-one");
        let cancellation = context.cancellation().clone();
        let mut stream = execute_with_context(account.clone(), context, None);
        let consume = async {
            let mut completed = 0;
            while let Some(event) = stream.next().await {
                completed += event?
                    .canonical_facts()
                    .iter()
                    .filter(|fact| matches!(fact, GatewayEvent::Completed(_)))
                    .count();
            }
            Ok::<_, ProviderError>(completed)
        };
        let interrupt = async {
            receiving.await.unwrap();
            tokio::time::sleep(Duration::from_millis(800)).await;
            if cancel {
                cancellation.cancel();
            }
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(consume, interrupt)
        })
        .await
        .unwrap();
        if cancel {
            assert_eq!(result.unwrap_err().kind(), ProviderErrorKind::Cancelled);
        } else {
            assert_eq!(result.unwrap(), 1);
        }
        drop(stream);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), server)
                .await
                .unwrap()
                .unwrap(),
            !cancel
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            while Arc::strong_count(&account) != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        drop(generation);
        runtime.shutdown().await;
        super::wait_until_empty(cache.path()).await;
    }
}

#[tokio::test]
async fn adapter_preserves_parsed_and_opaque_websocket_messages() {
    use gateway_plugin_sdk::call::model::{WireEvent, WirePayload};
    let messages = [
        "{ \"type\": \"future.event\", \"value\": 1e3 }".to_owned(),
        "opaque future message".to_owned(),
        format!("{{\"value\":{}0{}}}", "[".repeat(140), "]".repeat(140)),
    ];
    let mut config = configuration("http://127.0.0.1:1", false);
    config["upstream_callbacks"] = json!([]);
    config["upstream_registration"]["adapters"][0]["transport"] = json!("websocket");
    let mut events = vec![config["upstream_events"][0].clone()];
    for message in &messages {
        events.push(json!({"event":{"facts":[],"wire":WireEvent {
            protocol: "openai".into(),
            payload: WirePayload::WebSocket {
                event: None, data: serde_json::from_str(message).ok(), message: message.as_bytes().to_vec(),
            },
        }}}));
    }
    events.push(json!({"event":config["upstream_events"][1]["event"].clone()}));
    config["upstream_events"] = json!(events);
    let (cache, _, runtime) = setup(config).await;
    let generation = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
        .await
        .unwrap();
    let mut stream = execute(&runtime, &generation, Account::new(), "key-one", None);
    let mut received = Vec::new();
    while let Some(event) = stream.next().await {
        if let Some(wire) = event.unwrap().wire_event() {
            assert!(wire.raw_sse_frame().is_none());
            received.push(wire.raw_websocket_message().unwrap().to_owned());
        }
    }
    assert_eq!(received, messages);
    drop(stream);
    drop(generation);
    runtime.shutdown().await;
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn managed_http_failure_keeps_host_reason_and_error_kind() {
    let mut config = configuration("http://127.0.0.1:1", false);
    config["upstream_callbacks"][0]["params"]["method"] = json!("invalid method");
    let (cache, _, runtime) = setup(config).await;
    let generation = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
        .await
        .unwrap();
    let mut stream = execute(&runtime, &generation, Account::new(), "key-one", None);
    let error = stream.next().await.unwrap().unwrap_err();
    let raw: Value = serde_json::from_str(error.raw_upstream_error().unwrap().as_str()).unwrap();
    assert_eq!(raw["fault"]["details"]["kind"], "invalid_request");
    assert_eq!(raw["fault"]["details"]["reason"], "method");
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    drop(stream);
    drop(generation);
    runtime.shutdown().await;
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn adapter_preserves_observation_updates_and_measures_only_real_output() {
    let mut config = configuration("http://127.0.0.1:1", true);
    config["upstream_callbacks"] = json!([]);
    config["upstream_events"] = json!([
        {"event":{"facts":[{"type":"started","id":"response-one","model":"native-model"},
            {"type":"content_added","index":0,"kind":"reasoning"}]},
         "observation":{"transport":"http_sse","http_version":"HTTP/2","status_code":200,
            "request_id":"upstream-494","upstream_response_model":"real-model",
            "client_headers":[{"name":"x-bytes","value":[255]},{"name":"x-bytes","value":[254]}],
            "provider_metadata":"{ \"future\": 1e3 }","timings":{"connect_ms":5,"headers_ms":8}}},
        {"event":{"facts":[{"type":"reasoning_delta","index":0,"text":""}]}},
        {"event":{"facts":[{"type":"reasoning_delta","index":0,"text":"thinking"}]}},
        {"event":{"facts":[{"type":"content_added","index":1,"kind":"text"},{"type":"text_delta","index":1,"text":"answer"}]}},
        {"event":{"facts":[]},"observation":{"transport":"http_sse","service_tier":"priority",
            "timings":{"upstream_response_ms":1000,"upstream_api_overhead_ms":1.25,"upstream_engine_ms":998.75}}},
        {"event":{"facts":[{"type":"usage","usage":{"input_tokens":7,"output_tokens":2}},
            {"type":"completed","id":"response-one","model":"native-model","reason":"stop"}]},
         "observation":{"transport":"http_sse","timings":{"first_token_ms":41}}}
    ]);
    let (cache, _, runtime) = setup(config).await;
    let generation = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
        .await
        .unwrap();
    let context = context(&runtime, &generation, "key-one");
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut stream = execute_with_context(Account::new(), context, None);
    let mut observations = Vec::new();
    while let Some(event) = stream.next().await {
        observations.push(event.unwrap().response_observation().unwrap().clone());
    }
    assert_eq!(observations.len(), 6);
    assert_eq!(observations[0].timings().first_token_ms, None);
    assert_eq!(observations[1].timings().first_token_ms, None);
    let first_output = observations[2].timings().first_token_ms.unwrap();
    assert!(first_output >= 50, "首字计时必须包括调用前的等待");
    assert_eq!(
        observations[2].timings().first_reasoning_ms,
        Some(first_output)
    );
    assert!(observations[3].timings().first_text_ms.unwrap() >= first_output);
    assert_eq!(observations[3].timings().upstream_response_ms, None);
    let last = observations.last().unwrap();
    assert_eq!(last.timings().first_token_ms, Some(41));
    assert_eq!(last.timings().connect_ms, Some(5));
    assert_eq!(last.timings().headers_ms, Some(8));
    assert_eq!(last.timings().upstream_response_ms, Some(1000));
    assert_eq!(last.timings().upstream_api_overhead_ms, Some(1.25));
    assert_eq!(last.timings().upstream_engine_ms, Some(998.75));
    assert_eq!(last.status_code(), Some(200));
    assert_eq!(last.http_version().unwrap().as_str(), "HTTP/2");
    assert_eq!(last.request_id().unwrap().as_str(), "upstream-494");
    assert_eq!(last.upstream_response_model(), Some("real-model"));
    assert_eq!(last.service_tier(), Some("priority"));
    assert_eq!(
        last.provider_metadata().unwrap().as_json(),
        "{ \"future\": 1e3 }"
    );
    assert_eq!(last.client_headers().len(), 2);
    assert_eq!(last.client_headers()[0].value().as_ref(), [255]);
    assert_eq!(last.client_headers()[1].value().as_ref(), [254]);
    drop(stream);
    drop(generation);
    runtime.shutdown().await;
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn managed_http_preserves_byte_headers_trailers_and_version_for_plugins() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for streaming in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let upstream = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                assert!(socket.read_buf(&mut request).await.unwrap() > 0);
            }
            socket.write_all(b"HTTP/1.1 200 OK\r\nX-Bytes: \xff\r\nX-Bytes: other\r\nTransfer-Encoding: chunked\r\nTrailer: x-tail\r\n\r\n").await.unwrap();
            let body = if streaming { "data: {}\n\n" } else { "{}" };
            socket
                .write_all(format!("{:x}\r\n{body}\r\n", body.len()).as_bytes())
                .await
                .unwrap();
            socket
                .write_all(b"0\r\nx-tail: \xfe\r\nx-tail: final\r\n\r\n")
                .await
                .unwrap();
        });
        let records = tempfile::tempdir().unwrap();
        let marker = records.path().join("http.jsonl");
        let mut config = configuration(&base, streaming);
        config["upstream_callbacks"][0]["body"] = json!("");
        config["upstream_http_marker"] = json!(marker);
        let (cache, _, runtime) = setup(config).await;
        let generation =
            ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
                .await
                .unwrap();
        let mut stream = execute(&runtime, &generation, Account::new(), "key-one", None);
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
        upstream.await.unwrap();
        let observations: Vec<Value> = std::fs::read_to_string(&marker)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(observations[0]["http_version"], "HTTP/1.1");
        let headers = observations[0]["headers"].as_array().unwrap();
        assert!(headers.contains(&json!(["x-bytes", [255]])));
        assert!(headers.contains(&json!(["x-bytes", b"other".to_vec()])));
        let trailers = &observations.last().unwrap()["trailers"];
        assert_eq!(
            trailers,
            &json!([["x-tail", [254]], ["x-tail", b"final".to_vec()]])
        );
        drop(stream);
        drop(generation);
        runtime.shutdown().await;
        super::wait_until_empty(cache.path()).await;
    }
}

#[tokio::test]
async fn adapter_accepts_service_tier_resolved_by_the_terminal_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(1)
        .mount(&server)
        .await;
    let mut config = configuration(&server.uri(), false);
    config["upstream_events"][0]["observation"] =
        json!({"transport":"http_json","service_tier":"auto"});
    config["upstream_events"][1]["observation"] =
        json!({"transport":"http_json","service_tier":"default"});
    let (cache, _, runtime) = setup(config).await;
    let generation = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
        .await
        .unwrap();
    let mut stream = execute(&runtime, &generation, Account::new(), "key-one", None);
    let mut tiers = Vec::new();
    let mut completed = false;
    while let Some(event) = stream.next().await {
        let event = event.unwrap();
        tiers.push(
            event
                .response_observation()
                .unwrap()
                .service_tier()
                .unwrap()
                .to_owned(),
        );
        completed |= event
            .canonical_facts()
            .iter()
            .any(|fact| matches!(fact, GatewayEvent::Completed(_)));
    }
    assert_eq!(tiers, ["auto", "default"]);
    assert!(completed);
    drop(stream);
    drop(generation);
    runtime.shutdown().await;
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn adapter_failure_preserves_upstream_diagnostics_and_native_feedback() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).set_body_string("{}"))
        .expect(1)
        .mount(&server)
        .await;
    let mut config = configuration(&server.uri(), false);
    config["upstream_events"] = json!([{
        "event": {"facts": []},
        "observation":{"transport":"http_json","status_code":429,"timings":{"upstream_response_ms":230}},
        "failure": {
            "kind": "rate_limited", "status": 429, "retry_after_ms": 1500,
            "message": "fixture rate limit", "code": "rate_limit_exceeded"
        }
    }]);
    let (cache, _, runtime) = setup(config).await;
    let generation = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
        .await
        .unwrap();
    let account = Account::new();
    let mut stream = execute(&runtime, &generation, Arc::clone(&account), "key-one", None);
    let mut error = stream.next().await.unwrap().unwrap_err();
    assert_eq!(error.kind(), ProviderErrorKind::RateLimited);
    assert_eq!(error.send_state(), UpstreamSendState::Sent);
    assert_eq!(error.upstream_status(), Some(429));
    assert_eq!(
        error.upstream_code().unwrap().as_str(),
        "rate_limit_exceeded"
    );
    assert_eq!(error.retry_after(), Some(Duration::from_millis(1500)));
    let observations = error.take_atomic_client_events();
    let observation = observations[0].response_observation().unwrap();
    assert_eq!(observation.status_code(), Some(429));
    assert_eq!(observation.timings().upstream_response_ms, Some(230));
    let snapshot = error.stable_snapshot();
    let raw: Value = serde_json::from_str(snapshot.raw_upstream_error().unwrap().as_str()).unwrap();
    assert_eq!(raw["message"], "fixture rate limit");
    assert_eq!(raw["code"], "rate_limit_exceeded");
    assert_eq!(raw["status"], 429);
    let diagnostic = snapshot.diagnostic().unwrap();
    assert_eq!(diagnostic.code(), Some("plugin_upstream_failure"));
    assert!(!diagnostic.as_str().contains("fixture rate limit"));
    assert!(!format!("{snapshot:?}").contains("fixture rate limit"));
    assert!(generation.is_ready(), "上游 429 不能停止插件");
    assert!(generation.can_serve());
    assert_eq!(account.failures.load(Ordering::SeqCst), 1);
    drop(stream);
    assert_eq!(Arc::strong_count(&account), 1);
    drop(generation);
    runtime.shutdown().await;
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn adapter_overrides_auth_headers_and_rejects_stale_continuation() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(2)
        .mount(&server)
        .await;
    let (cache, _, runtime) = setup(configuration(&server.uri(), false)).await;
    let generation = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
        .await
        .unwrap();
    let mut stream = execute(&runtime, &generation, Account::new(), "key-one", None);
    let mut state = None;
    while let Some(event) = stream.next().await {
        state = event.unwrap().take_session_update().or(state);
    }
    drop(stream);
    for (key, account) in [
        ("key-two", Account::new()),
        ("key-one", Account::with_identity("acct_other", 1)),
        ("key-one", Account::with_identity("acct_one", 2)),
    ] {
        let error = execute(&runtime, &generation, account, key, state.clone())
            .next()
            .await
            .unwrap()
            .err()
            .unwrap();
        assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    }
    drop(generation);
    runtime.shutdown().await;
    super::wait_until_empty(cache.path()).await;

    let mut config = configuration(&server.uri(), false);
    config["upstream_callbacks"][0]["params"]["headers"] =
        json!([["Authorization", b"injected".to_vec()]]);
    let (cache, _, runtime) = setup(config).await;
    let generation = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
        .await
        .unwrap();
    let mut stream = execute(&runtime, &generation, Account::new(), "key-one", None);
    while let Some(event) = stream.next().await {
        event.unwrap();
    }
    drop(stream);
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].headers.get("authorization").unwrap(),
        "injected"
    );
    drop(generation);
    runtime.shutdown().await;
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn adapter_does_not_publish_completed_when_rpc_fails_after_terminal_frame() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;
    let mut config = configuration(&server.uri(), false);
    config["upstream_tail_error"] = json!(true);
    let (cache, _, runtime) = setup(config).await;
    let generation = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
        .await
        .unwrap();
    let mut stream = execute(&runtime, &generation, Account::new(), "key-one", None);
    stream.next().await.unwrap().unwrap();
    let error = stream.next().await.unwrap().err().unwrap();
    assert_eq!(error.kind(), ProviderErrorKind::Unavailable);
    assert_eq!(error.send_state(), UpstreamSendState::Sent);
    let mut error = error;
    let raw: Value = serde_json::from_str(error.raw_upstream_error().unwrap().as_str()).unwrap();
    assert_eq!(raw["source"], "plugin_upstream_adapter");
    assert_eq!(raw["fault"]["message"], "fixture terminal fault");
    assert_eq!(error.diagnostic().unwrap().stage(), Some("plugin_rpc"));
    assert!(
        !error
            .diagnostic()
            .unwrap()
            .as_str()
            .contains("fixture terminal fault")
    );
    assert!(!format!("{error:?}").contains("fixture terminal fault"));
    let atomic = error.take_atomic_client_events();
    assert!(
        atomic
            .iter()
            .any(|event| event.response_observation().is_some())
    );
    let facts = atomic
        .into_iter()
        .flat_map(|event| event.into_parts().0)
        .collect::<Vec<_>>();
    assert_eq!(facts.len(), 2);
    assert!(matches!(facts[0], GatewayEvent::Usage(_)));
    assert!(matches!(facts[1], GatewayEvent::CalculatedCost(_)));
    drop(stream);
    drop(generation);
    runtime.shutdown().await;
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn overlapping_adapter_declarations_keep_first_match_without_blocking_preparation() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/adapter/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(1)
        .mount(&server)
        .await;
    let mut config = configuration(&server.uri(), false);
    let mut duplicate = config["upstream_registration"]["adapters"][0].clone();
    duplicate["base_url"] = json!("https://unused.invalid/adapter/");
    config["upstream_registration"]["adapters"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    let (cache, _, runtime) = setup(config).await;
    let generation = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
        .await
        .unwrap();
    let mut stream = execute(&runtime, &generation, Account::new(), "key-one", None);
    while let Some(event) = stream.next().await {
        event.unwrap();
    }
    drop(stream);
    drop(generation);
    runtime.shutdown().await;
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn adapter_registration_preserves_plugin_targets_with_typed_provider_decoding() {
    for target in [
        "../responses",
        "https://elsewhere.invalid/responses",
        "responses?other=1",
        "%2e%2e/responses",
        "/responses",
    ] {
        let mut config = configuration("https://example.invalid", false);
        config["upstream_registration"]["adapters"][0]["paths"][0]["path"] = json!(target);
        let (_, store, runtime) = setup(config).await;
        let snapshot = store.snapshot.lock().unwrap().clone();
        assert!(
            gateway_admin::ports::plugins::PluginPreparation::prepare(&runtime, snapshot)
                .await
                .is_ok()
        );
        runtime.shutdown().await;
    }
    let mut config = configuration("https://example.invalid", false);
    config["upstream_registration"]["adapters"][0]["provider"] = json!("custom");
    let (_, store, runtime) = setup(config).await;
    let snapshot = store.snapshot.lock().unwrap().clone();
    assert!(
        gateway_admin::ports::plugins::PluginPreparation::prepare(&runtime, snapshot)
            .await
            .is_err()
    );
    runtime.shutdown().await;
}

#[tokio::test]
#[expect(
    clippy::result_large_err,
    reason = "握手回调的错误类型由 tungstenite 固定"
)]
async fn websocket_continuation_reuses_exact_connection_and_consumes_each_handle_once() {
    use futures::SinkExt as _;
    use tokio_tungstenite::tungstenite::Message;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_hdr_async(
            socket,
            |request: &tokio_tungstenite::tungstenite::handshake::server::Request, mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                assert_eq!(
                    request.headers()["authorization"],
                    "Bearer fixture-native-token"
                );
                response.headers_mut().append("x-bytes", http::HeaderValue::from_static("handshake-value"));
                Ok(response)
            },
        )
        .await
        .unwrap();
        for _ in 0..2 {
            assert!(matches!(
                ws.next().await.unwrap().unwrap(),
                Message::Text(_)
            ));
            ws.send(Message::Text("{}".into())).await.unwrap();
        }
        assert!(ws.next().await.is_none_or(|result| result.is_err()));
    });
    let records = tempfile::tempdir().unwrap();
    let marker = records.path().join("handshake.jsonl");
    let mut config = configuration(&base, false);
    config["upstream_http_marker"] = json!(marker);
    config["upstream_registration"]["adapters"][0]["transport"] = json!("websocket");
    config["upstream_events"][1]["continuation"]["scope"] = json!("connection_local");
    config["upstream_callbacks"] = json!([
        {"method":"host.upstream.websocket.open","params":{"path":"responses","headers":[]}},
        {"method":"host.upstream.websocket.send","params":{"kind":"text"},"body":"request"},
        {"method":"host.upstream.websocket.read","params":{}}
    ]);
    let (cache, _, runtime) = setup(config).await;
    let generation = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
        .await
        .unwrap();
    let mut first = execute(&runtime, &generation, Account::new(), "key-one", None);
    let mut state = None;
    while let Some(event) = first.next().await {
        state = event.unwrap().take_session_update().or(state);
    }
    drop(first);
    let state = state.unwrap();
    assert!(state.extension_owner().unwrap().connection_local);
    let error = execute(
        &runtime,
        &generation,
        Account::new(),
        "key-two",
        Some(state.clone()),
    )
    .next()
    .await
    .unwrap()
    .err()
    .unwrap();
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    let mut second = execute(
        &runtime,
        &generation,
        Account::new(),
        "key-one",
        Some(state.clone()),
    );
    while let Some(event) = second.next().await {
        event.unwrap();
    }
    drop(second);
    let handshakes: Vec<Value> = std::fs::read_to_string(&marker)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|value| value["status"] == 101)
        .collect();
    assert_eq!(handshakes.len(), 2);
    assert_eq!(handshakes[0], handshakes[1]);
    assert_eq!(handshakes[0]["http_version"], "HTTP/1.1");
    assert!(
        handshakes[0]["headers"]
            .as_array()
            .unwrap()
            .contains(&json!(["x-bytes", b"handshake-value".to_vec()]))
    );

    let error = execute(
        &runtime,
        &generation,
        Account::new(),
        "key-one",
        Some(state),
    )
    .next()
    .await
    .unwrap()
    .err()
    .unwrap();
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    drop(generation);
    runtime.shutdown().await;
    super::wait_until_empty(cache.path()).await;
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn websocket_adapter_can_send_while_a_read_callback_is_pending() {
    use futures::SinkExt as _;
    use tokio_tungstenite::tungstenite::Message;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
        assert_eq!(
            socket.next().await.unwrap().unwrap(),
            Message::Text("control".into())
        );
        socket.send(Message::Text("{}".into())).await.unwrap();
        assert!(socket.next().await.is_none_or(|result| result.is_err()));
    });
    let mut config = configuration(&base, false);
    config["upstream_registration"]["adapters"][0]["transport"] = json!("websocket");
    config["upstream_events"][1]["continuation"]["scope"] = json!("connection_local");
    config["upstream_callbacks"] = json!([
        {"method":"host.upstream.websocket.open","params":{"path":"responses","headers":[]}},
        [
            {"method":"host.upstream.websocket.read","params":{}},
            {"method":"host.upstream.websocket.send","params":{"kind":"text"},"body":"control"}
        ]
    ]);
    let (cache, _, runtime) = setup(config).await;
    let generation = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
        .await
        .unwrap();
    let mut stream = execute(&runtime, &generation, Account::new(), "key-one", None);
    let mut continuation = None;
    tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(event) = stream.next().await {
            continuation = event.unwrap().take_session_update().or(continuation.take());
        }
    })
    .await
    .unwrap();
    assert!(
        continuation
            .unwrap()
            .extension_owner()
            .unwrap()
            .connection_local
    );
    drop(stream);
    drop(generation);
    runtime.shutdown().await;
    super::wait_until_empty(cache.path()).await;
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn cancelling_adapter_body_closes_pending_websocket_and_releases_selected_account() {
    use tokio_tungstenite::tungstenite::Message;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (sent, received) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
        assert!(matches!(
            socket.next().await.unwrap().unwrap(),
            Message::Text(_)
        ));
        sent.send(()).unwrap();
        assert!(socket.next().await.is_none_or(|result| result.is_err()));
    });
    let mut config = configuration(&base, false);
    config["upstream_registration"]["adapters"][0]["transport"] = json!("websocket");
    config["upstream_callbacks"] = json!([
        {"method":"host.upstream.websocket.open","params":{"path":"responses","headers":[]}},
        {"method":"host.upstream.websocket.send","params":{"kind":"text"},"body":"request"},
        {"method":"host.upstream.websocket.read","params":{}}
    ]);
    let (cache, _, runtime) = setup(config).await;
    let generation = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
        .await
        .unwrap();
    let account = Account::new();
    let mut stream = execute(&runtime, &generation, account.clone(), "key-one", None);
    let mut reading = Box::pin(stream.next());
    tokio::select! {
        result = &mut reading => panic!("上游等待期间不应提前产生结果：{}", result.is_some()),
        result = tokio::time::timeout(Duration::from_secs(3), received) => result.unwrap().unwrap(),
    }
    drop(reading);
    drop(stream);
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while Arc::strong_count(&account) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(generation);
    runtime.shutdown().await;
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn usage_without_response_model_still_uses_native_account_pricing() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;
    let mut config = configuration(&server.uri(), false);
    config["upstream_events"][0]["event"]["facts"][0]["model"] = Value::Null;
    config["upstream_events"][1]["event"]["facts"][1]["model"] = Value::Null;
    let (_, _, runtime) = setup(config).await;
    let generation = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
        .await
        .unwrap();
    let mut stream = execute(&runtime, &generation, Account::new(), "key-one", None);
    let mut costs = 0;
    while let Some(event) = stream.next().await {
        costs += event
            .unwrap()
            .canonical_facts()
            .iter()
            .filter(|fact| matches!(fact, GatewayEvent::CalculatedCost(_)))
            .count();
    }
    assert_eq!(costs, 1);
    drop(stream);
    drop(generation);
    runtime.shutdown().await;
}

#[tokio::test]
async fn core_published_adapter_uses_normal_attempt_usage_and_cost_ledger() {
    use crate::support::environment::Environment;
    let Some(mut environment) = Environment::create_command().await else {
        eprintln!("SKIP: plugin integration environment absent");
        return;
    };
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("authorization", "Bearer fixture-native-token"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(1)
        .mount(&server)
        .await;
    let account = environment.account(None).await;
    let group = environment.account_group_with_account(&account).await;
    let key_id = "key_adapter_ledger";
    environment
        .client_key_with_limits_and_groups(
            key_id,
            "sk-adapter-ledger-fixture",
            gateway_core::policy::RateLimits::unlimited(),
            vec![group],
        )
        .await;
    environment
        .install_plugin(configuration(&server.uri(), false))
        .await;
    environment.install_plugin(json!({
        "plugin_id":"test.adapter-ledger-caller",
        "command_registration":{"commands":[{"name":"run","description":"执行模型"}]},
        "command_nested_model_fixture":{
            "request":{"client_key_id":key_id,"model":crate::support::native::MODEL,"protocol":"openai","operation":"generate","provider":"openai","account_id":account.as_str()},
            "body":{"model":crate::support::native::MODEL,"input":"adapter ledger"}
        },
        "command_result":{"stdout":"ok","stderr":"","exit_code":0}
    })).await;
    let (runtime, core) = environment.command_plane().await;
    let snapshot = environment
        .store
        .admin_ports()
        .plugins()
        .load_instances()
        .await
        .unwrap();
    let caller = snapshot
        .instances
        .iter()
        .find(|instance| instance.configuration.get("command_registration").is_some())
        .unwrap();
    let commands = runtime.prepare_command_line().await.unwrap();
    environment.store.start_command_line_writes().unwrap();
    let result = commands.execute(&caller.id, "run", &[]).await.unwrap();
    assert_eq!(result.stdout, "ok");
    commands.shutdown().await;
    environment
        .store
        .shutdown_command_line_writes()
        .await
        .unwrap();
    environment
        .assert_upstream_accounting(key_id, account.as_str())
        .await;
    drop(core);
    drop(runtime);
    environment.close().await;
}

#[tokio::test]
async fn adapter_reconfiguration_disable_and_rollback_keep_inflight_generation_and_reap_workers() {
    let original = MockServer::start().await;
    let replacement = MockServer::start().await;
    for (server, expected) in [(&original, 2), (&replacement, 1)] {
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(expected)
            .mount(server)
            .await;
    }
    let (cache, store, runtime) = setup(configuration(&original.uri(), false)).await;
    let old = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
        .await
        .unwrap();
    let account = Account::new();
    let mut old_stream = execute(&runtime, &old, account.clone(), "key-one", None);
    old_stream.next().await.unwrap().unwrap();
    {
        let mut snapshot = store.snapshot.lock().unwrap();
        snapshot.config_revision = gateway_admin::model::Revision::new(2).unwrap();
        snapshot.instances[0].revision = snapshot.config_revision;
        snapshot.instances[0].configuration = configuration(&replacement.uri(), false);
    }
    let new = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(2).unwrap())
        .await
        .unwrap();
    assert_ne!(old.id(), new.id());
    let mut new_stream = execute(&runtime, &new, account.clone(), "key-one", None);
    new_stream.next().await.unwrap().unwrap();
    drop(old);
    let old_state = old_stream
        .next()
        .await
        .unwrap()
        .unwrap()
        .take_session_update()
        .unwrap();
    assert!(old_stream.next().await.is_none());
    drop(old_stream);
    {
        let mut snapshot = store.snapshot.lock().unwrap();
        snapshot.config_revision = gateway_admin::model::Revision::new(3).unwrap();
        snapshot.instances[0].revision = snapshot.config_revision;
        snapshot.instances[0].enabled = false;
    }
    let disabled = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(3).unwrap())
        .await
        .unwrap();
    assert!(
        context(&runtime, &disabled, "key-one")
            .upstream_adapter(
                &ProviderKind::new("openai").unwrap(),
                &UpstreamModelId::new("native-model").unwrap()
            )
            .unwrap()
            .is_none()
    );
    drop(new);
    assert!(
        new_stream
            .next()
            .await
            .unwrap()
            .unwrap()
            .canonical_facts()
            .iter()
            .any(|fact| matches!(fact, GatewayEvent::Completed(_)))
    );
    assert!(new_stream.next().await.is_none());
    drop(new_stream);
    drop(disabled);
    super::wait_until_empty(cache.path()).await;
    assert_eq!(Arc::strong_count(&account), 1);
    {
        let mut snapshot = store.snapshot.lock().unwrap();
        snapshot.config_revision = gateway_admin::model::Revision::new(4).unwrap();
        snapshot.instances[0].revision = snapshot.config_revision;
        snapshot.instances[0].enabled = true;
        snapshot.instances[0].configuration = configuration(&original.uri(), false);
    }
    let restored = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(4).unwrap())
        .await
        .unwrap();
    // 恢复同一配置仍是新代次，不能复活旧进程中的续接身份
    let error = execute(
        &runtime,
        &restored,
        account.clone(),
        "key-one",
        Some(old_state),
    )
    .next()
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    let mut fresh = execute(&runtime, &restored, account.clone(), "key-one", None);
    while let Some(event) = fresh.next().await {
        event.unwrap();
    }
    drop(fresh);
    drop(restored);
    runtime.shutdown().await;
    super::wait_until_empty(cache.path()).await;
    assert_eq!(Arc::strong_count(&account), 1);
}

#[tokio::test]
async fn upstream_adapter_can_dispatch_http_after_its_instance_entered_the_scope() {
    struct Http(Arc<AtomicUsize>);
    impl gateway_core::engine::middleware::http::Dispatcher for Http {
        fn dispatch(
            &self,
            context: gateway_core::engine::middleware::http::Context,
            request: gateway_core::engine::middleware::http::Request,
        ) -> BoxFuture<
            'static,
            Result<
                gateway_core::engine::middleware::http::Response,
                gateway_core::engine::middleware::MiddlewareError,
            >,
        > {
            assert!(
                context
                    .extensions
                    .contains(context.plugin_instance_id.as_deref().unwrap())
            );
            assert_eq!(context.extensions.len(), 1);
            assert_eq!(request.uri(), "/api/admin/settings");
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Ok(gateway_core::engine::middleware::http::Response::new(
                    gateway_core::engine::middleware::http::empty_body(),
                ))
            })
        }
    }
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{\"id\":\"response-one\"}"))
        .mount(&server)
        .await;
    let mut config = configuration(&server.uri(), false);
    config["upstream_callbacks"].as_array_mut().unwrap().insert(0, json!({
        "method":"host.http.dispatch", "params": {"settings":null,"method":"GET", "uri":"/api/admin/settings", "version":"HTTP/1.1", "headers":[], "timeout_ms":null, "body":{"kind":"empty"}}
    }));
    let (_cache, _, runtime) = setup(config).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let dispatcher: Arc<dyn gateway_core::engine::middleware::http::Dispatcher> =
        Arc::new(Http(calls.clone()));
    runtime.bind_http(&dispatcher).unwrap();
    let generation = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
        .await
        .unwrap();
    let mut stream = execute(&runtime, &generation, Account::new(), "key-one", None);
    while let Some(event) = stream.next().await {
        event.unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    drop(stream);
    drop(generation);
    drop(dispatcher);
    runtime.shutdown().await;
}

#[tokio::test]
async fn malformed_adapter_events_fail_only_the_current_stream() {
    use gateway_admin::ports::plugins::PluginRuntimeDiagnostics as _;
    use std::error::Error as _;
    for (fact, has_sequence_error) in [
        (
            json!({"type":"completed","id":"never-started","model":"native-model","reason":"stop"}),
            false,
        ),
        (
            json!({"type":"text_delta","index":0,"text":"delta before start"}),
            true,
        ),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        let mut config = configuration(&server.uri(), false);
        config["upstream_events"] = json!([{"event":{"facts":[fact]}}]);
        let (cache, store, runtime) = setup(config).await;
        let generation =
            ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
                .await
                .unwrap();
        let mut stream = execute(&runtime, &generation, Account::new(), "key-one", None);
        let error = stream.next().await.unwrap().unwrap_err();
        assert_eq!(error.kind(), ProviderErrorKind::Protocol);
        if has_sequence_error {
            assert!(
                error
                    .source()
                    .unwrap()
                    .is::<gateway_core::event::EventSequenceError>()
            );
        }
        assert_eq!(error.diagnostic().unwrap().code(), Some("invalid_event"));
        assert!(generation.is_ready());
        assert!(generation.can_serve());
        let snapshot = store.snapshot.lock().unwrap().clone();
        assert!(snapshot.instances[0].enabled);
        let diagnostics = runtime
            .runtime_diagnostics(&snapshot, Some(1), Some(&generation))
            .await
            .unwrap();
        assert!(diagnostics["instance-one"].failure.is_none());
        drop(stream);
        drop(generation);
        runtime.shutdown().await;
        super::wait_until_empty(cache.path()).await;
    }
}

#[tokio::test]
async fn large_adapter_requests_use_the_frozen_budget_for_both_http_capabilities() {
    for streaming in [false, true] {
        for upstream in [false, true] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(200).set_body_string(if streaming {
                    "data: {}\n\n"
                } else {
                    "{}"
                }))
                .expect(if upstream { 2 } else { 4 })
                .mount(&server)
                .await;
            let mut config = configuration(&server.uri(), streaming);
            if !upstream {
                let mut inference = config["upstream_callbacks"][0].clone();
                inference["body"] = json!("{}");
                config["upstream_callbacks"]
                    .as_array_mut()
                    .unwrap()
                    .push(inference);
                config["upstream_callbacks"][0]["method"] = json!(if streaming {
                    "host.http.do_stream"
                } else {
                    "host.http.do"
                });
                config["upstream_callbacks"][0]["params"] = json!({"method":"POST", "url":format!("{}/auxiliary", server.uri()), "headers":[]});
            }
            let (cache, _, runtime) = setup(config).await;
            let generation =
                ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
                    .await
                    .unwrap();
            let account = Account::new();
            let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
                ProtocolPayload::json_object(
                    "openai",
                    json!({"model":"native-model","input":"x".repeat(17 * 1024 * 1024)})
                        .as_object()
                        .unwrap()
                        .clone(),
                )
                .unwrap(),
            ));
            let expected = operation.middleware_body().unwrap();
            for accepted in [true, false, true] {
                let context = context_with_body_limit(
                    &runtime,
                    &generation,
                    "key-one",
                    Default::default(),
                    expected.len() - usize::from(!accepted),
                );
                let provider = ProviderKind::new("openai").unwrap();
                let model = UpstreamModelId::new("native-model").unwrap();
                let adapter = context
                    .upstream_adapter(&provider, &model)
                    .unwrap()
                    .unwrap();
                let metadata = ProviderCallMetadata::new(
                    provider,
                    model,
                    account.id.clone(),
                    UpstreamTransport::new(adapter.transport()).unwrap(),
                );
                let mut stream = adapter.execute(UpstreamAdapterInvocation {
                    operation: operation.clone(),
                    headers: vec![],
                    context,
                    metadata,
                    account: account.clone(),
                });
                let mut completed = false;
                let mut failure = None;
                while let Some(event) = stream.next().await {
                    match event {
                        Ok(event) => {
                            completed |= event
                                .canonical_facts()
                                .iter()
                                .any(|fact| matches!(fact, GatewayEvent::Completed(_)))
                        }
                        Err(error) => {
                            failure = Some(error);
                            break;
                        }
                    }
                }
                if accepted {
                    assert!(
                        failure.is_none(),
                        "streaming={streaming} upstream={upstream}: {failure:?}"
                    );
                    assert!(completed);
                } else {
                    assert_eq!(failure.unwrap().send_state(), UpstreamSendState::NotSent);
                }
            }
            let requests = server
                .received_requests()
                .await
                .unwrap()
                .into_iter()
                .filter(|request| request.body.len() > 1024 * 1024)
                .collect::<Vec<_>>();
            assert_eq!(requests.len(), 2);
            for request in requests {
                assert_eq!(request.body.as_slice(), expected.as_ref());
            }
            drop(generation);
            runtime.shutdown().await;
            super::wait_until_empty(cache.path()).await;
        }
    }
}
