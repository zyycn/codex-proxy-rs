//! 隐私规则在真实 HTTP、WebSocket 和发送前拒绝边界的合同

use super::*;
use crate::transport::privacy::{policy, rule};
use gateway_core::settings::privacy::{CodexPrivacyPolicy, PrivacyAction, PrivacyScope};

fn privacy_context(policy: &CodexPrivacyPolicy, fallback: bool) -> AttemptContext {
    let context = AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_privacy").unwrap(),
            ClientApiKeyId::new("key_openai_contract").unwrap(),
        )
        .with_privacy(Some(
            provider_openai::transport::privacy::compile(policy).unwrap(),
        )),
        NonZeroU32::MIN,
        SystemTime::now() + Duration::from_secs(30),
        account_policy(),
        AccountAttemptContext::new(BTreeSet::new(), None, None)
            .with_account_scope(contract_account_scope()),
        None,
        CancellationToken::new(),
    );
    if fallback {
        context.with_transport(AttemptTransport::Fallback)
    } else {
        context
    }
}

fn outbound_policy() -> CodexPrivacyPolicy {
    let mut auth = rule(
        PrivacyScope::RequestHeader,
        "authorization",
        PrivacyAction::SetValue,
    );
    auth.value = json!("Bearer configured-synthetic-token");
    let mut input = rule(
        PrivacyScope::RequestBody,
        "$.input[0].content[0].text",
        PrivacyAction::RegexReplace,
    );
    input.id = "input".into();
    input.pattern = Some("hello".into());
    input.replacement = "hellohello".into();
    let mut metadata = rule(
        PrivacyScope::TurnMetadata,
        "$.workspaces",
        PrivacyAction::RemoveField,
    );
    metadata.id = "metadata".into();
    policy(vec![auth, input, metadata])
}

fn metadata_operation() -> Operation {
    let header =
        json!({"workspaces":{"/home/alex/private":{}},"request_kind":"turn","header_only":true});
    let client = json!({"workspaces":{"/home/alex/private":{}},"tool_namespaces_info":{"tools":["example"]}});
    let body = json!({"model":"gpt-5.4","input":"hello","turnMetadata":header.to_string(),"client_metadata":{"x-codex-turn-metadata":client.to_string()}});
    Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", body.as_object().unwrap().clone()).unwrap(),
    ))
}

fn assert_metadata_filtered(raw: &str, client_copy: bool) {
    let metadata: Value = serde_json::from_str(raw).unwrap();
    assert!(metadata.get("workspaces").is_none());
    if client_copy {
        assert_eq!(
            metadata["tool_namespaces_info"],
            json!({"tools":["example"]})
        );
        assert!(metadata.get("header_only").is_none());
    } else {
        assert_eq!(metadata["header_only"], true);
        assert!(metadata.get("tool_namespaces_info").is_none());
    }
}

#[tokio::test]
async fn privacy_json_endpoints_preserve_untouched_bytes_and_rewrite_matched_body() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"output":"ok"})))
        .mount(&server)
        .await;
    let provider = provider_with_base_url(&store, server.uri());
    let raw = Bytes::from_static(
        br#"{ "id":"search", "workspaces":{"/private":{}}, "future":1, "future":2 }"#,
    );
    for change_body in [false, true] {
        let mut config = outbound_policy();
        config.rules.truncate(1);
        if change_body {
            let mut remove = rule(
                PrivacyScope::RequestBody,
                "$.workspaces",
                PrivacyAction::RemoveField,
            );
            remove.id = "remove-workspace".into();
            config.rules.push(remove);
        }
        let operation = Operation::Search(StandaloneSearchRequest::from_raw_json(
            RawJsonPayload::new("openai", raw.clone()).unwrap(),
        ));
        let mut stream = provider
            .clone()
            .execute(
                planned_provider_endpoint_request("openai", operation),
                privacy_context(&config, false),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
        let requests = server.received_requests().await.unwrap();
        let sent = requests.last().unwrap();
        assert_eq!(
            sent.headers["authorization"],
            "Bearer configured-synthetic-token"
        );
        if change_body {
            assert_eq!(
                serde_json::from_slice::<Value>(&sent.body).unwrap(),
                json!({"id":"search","future":2})
            );
        } else {
            assert_eq!(sent.body, raw);
        }
    }
}

#[tokio::test]
async fn privacy_rewrites_final_http_headers_and_body_once_per_attempt() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
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
    let provider = provider_with_base_url(&store, server.uri());
    let operation = metadata_operation();
    for _ in 0..2 {
        let mut stream = provider
            .clone()
            .execute(
                planned_request("openai", operation.clone()),
                privacy_context(&outbound_policy(), true),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    }
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    for request in requests {
        assert_metadata_filtered(
            request.headers["x-codex-turn-metadata"].to_str().unwrap(),
            false,
        );
        assert_metadata_filtered(
            captured_request_body(&request)["client_metadata"]["x-codex-turn-metadata"]
                .as_str()
                .unwrap(),
            true,
        );
        assert_eq!(
            request.headers["authorization"],
            "Bearer configured-synthetic-token"
        );
        assert_eq!(
            captured_request_body(&request)["input"][0]["content"][0]["text"],
            "hellohello"
        );
    }
}

#[tokio::test]
async fn privacy_rejection_is_not_sent_and_cannot_retry_or_rotate() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let server = MockServer::start().await;
    let mut invalid_target = rule(
        PrivacyScope::RequestBody,
        "$.input[0].content[0].text",
        PrivacyAction::SetValue,
    );
    invalid_target.value = json!(42);
    let provider = provider_with_base_url(&store, server.uri());
    let result = provider
        .execute(
            planned_request("openai", generate_operation()),
            privacy_context(&policy(vec![invalid_target]), true),
        )
        .await;
    let error = match result {
        Err(error) => error,
        Ok(mut stream) => stream.next().await.unwrap().err().unwrap(),
    };
    assert_eq!(error.kind(), ProviderErrorKind::InvalidRequest);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    assert!(error.retry_is_prohibited());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn privacy_rewrites_websocket_handshake_and_frame_once() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let captured = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket =
            crate::transport::accept_codex_test_websocket_with(stream, |request, response| {
                assert_eq!(
                    request.headers()["authorization"],
                    "Bearer configured-synthetic-token"
                );
                assert_metadata_filtered(
                    request.headers()["x-codex-turn-metadata"].to_str().unwrap(),
                    false,
                );
                response.headers_mut().insert(
                    "sec-websocket-extensions",
                    "permessage-deflate".parse().unwrap(),
                );
            })
            .await;
        let frame = socket.next().await.unwrap().unwrap().into_text().unwrap();
        let body: Value = serde_json::from_str(&frame).unwrap();
        assert_metadata_filtered(
            body["client_metadata"]["x-codex-turn-metadata"]
                .as_str()
                .unwrap(),
            true,
        );
        assert_eq!(body["input"][0]["content"][0]["text"], "hellohello");
        socket.send(Message::Text(json!({"type":"response.completed","response":{"id":"resp_privacy","model":"gpt-5.4","status":"completed","output":[],"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}).to_string().into())).await.unwrap();
    });
    let provider = provider_with_base_url(&store, format!("http://{address}"));
    let mut stream = provider
        .execute(
            planned_request("openai", metadata_operation()),
            privacy_context(&outbound_policy(), false),
        )
        .await
        .unwrap();
    while let Some(event) = stream.next().await {
        event.unwrap();
    }
    captured.await.unwrap();
}

#[tokio::test]
async fn privacy_header_change_uses_a_new_websocket_pool_connection() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let captured = tokio::spawn(async move {
        let mut sockets = Vec::new();
        for expected in [
            "Bearer first-synthetic-token",
            "Bearer second-synthetic-token",
        ] {
            let (stream, _) = timeout(Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut socket =
                crate::transport::accept_codex_test_websocket_with(stream, |request, response| {
                    assert_eq!(request.headers()["authorization"], expected);
                    response.headers_mut().insert(
                        "sec-websocket-extensions",
                        "permessage-deflate".parse().unwrap(),
                    );
                })
                .await;
            let frame = socket.next().await.unwrap().unwrap().into_text().unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&frame).unwrap()["input"][0]["content"][0]["text"],
                "hellohello"
            );
            socket.send(Message::Text(json!({"type":"response.completed","response":{"id":"resp_privacy_pool","model":"gpt-5.4","status":"completed","output":[]}}).to_string().into())).await.unwrap();
            // 保留第一条连接，确保下一次握手是池隔离的结果而非断线重连
            sockets.push(socket);
        }
        sockets
    });
    let provider = provider_with_base_url(&store, format!("http://{address}"));
    for token in [
        "Bearer first-synthetic-token",
        "Bearer second-synthetic-token",
    ] {
        let mut config = outbound_policy();
        config.rules[0].value = json!(token);
        let operation = Operation::Generate(generate_with_session_context(
            "privacy-pool",
            Some("privacy-thread"),
            None,
        ));
        let mut stream = provider
            .clone()
            .execute(
                planned_request("openai", operation),
                privacy_context(&config, false),
            )
            .await
            .unwrap();
        timeout(Duration::from_secs(5), async {
            while let Some(event) = stream.next().await {
                event.unwrap();
            }
        })
        .await
        .unwrap();
    }
    captured.await.unwrap();
}
