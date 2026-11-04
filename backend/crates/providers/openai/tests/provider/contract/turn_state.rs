//! 验证客户端轮次 token 的全量回传、失败重试与账号状态隔离

use super::*;

const TOKEN: &str = "upstream-turn-state";

fn full_body() -> Value {
    json!({
        "model": "gpt-5.4", "store": false,
        "input": [{"role": "user", "content": "hello"}],
        "client_metadata": {
            "session_id": "turn-state-session",
            "thread_id": "turn-state-thread",
            "turn_id": "turn-state-turn"
        }
    })
}

fn full_request(body: Value, token: Option<&str>, websocket: bool) -> Operation {
    let mut protocol_context = Map::from_iter([
        ("use_websocket".to_owned(), json!(websocket)),
        (
            "session_id".to_owned(),
            body["client_metadata"]["session_id"].clone(),
        ),
        (
            "thread_id".to_owned(),
            body["client_metadata"]["thread_id"].clone(),
        ),
        (
            "turn_metadata".to_owned(),
            json!(body["client_metadata"].to_string()),
        ),
    ]);
    if let Some(token) = token {
        protocol_context.insert("turn_state".to_owned(), json!(token));
        protocol_context.insert(
            "opaque_request_headers".to_owned(),
            json!([["x-codex-turn-state", STANDARD.encode(token)]]),
        );
    }
    Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", body.as_object().unwrap().clone())
            .unwrap()
            .with_context(protocol_context),
    ))
}

async fn collect_response(
    provider: &Arc<CodexProvider>,
    operation: Operation,
    context: AttemptContext,
) -> Result<bool, gateway_core::error::ProviderError> {
    let mut stream = provider
        .clone()
        .execute(planned_request("openai", operation), context)
        .await?;
    let mut observed_token = false;
    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(observation) = event.response_observation() {
            observed_token |= observation.client_headers().iter().any(|header| {
                header.name() == "x-codex-turn-state" && header.value().as_ref() == TOKEN.as_bytes()
            });
        }
    }
    Ok(observed_token)
}

async fn http_fixture() -> (Arc<MemoryAccountStore>, MockServer, Arc<CodexProvider>) {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_session_affinity").await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .insert_header("x-codex-turn-state", TOKEN)
                .set_body_string(CAPTURE_COMPLETED_SSE),
        )
        .mount(&server)
        .await;
    let provider = provider_with_base_url(&store, server.uri());
    (store, server, provider)
}

#[tokio::test]
async fn same_turn_full_http_request_preserves_observed_token_without_restoring_other_account_state()
 {
    let (_store, server, provider) = http_fixture().await;
    assert!(
        collect_response(
            &provider,
            full_request(full_body(), None, false),
            context("req_turn_first", CancellationToken::new())
        )
        .await
        .unwrap()
    );
    let mut body = full_body();
    body["client_metadata"]["x-codex-turn-state"] = json!(TOKEN);
    body["client_metadata"]["response_id"] = json!("unowned-response");
    body["client_metadata"]["parent_response_id"] = json!("unowned-parent");
    body["client_metadata"]["account_id"] = json!("unowned-account");
    collect_response(
        &provider,
        full_request(body, Some(TOKEN), false),
        context("req_turn_echo", CancellationToken::new()),
    )
    .await
    .unwrap();
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].headers["x-codex-turn-state"], TOKEN);
    let body = captured_request_body(&requests[1]);
    assert_eq!(body["client_metadata"]["x-codex-turn-state"], TOKEN);
    assert!(body.get("previous_response_id").is_none());
    for field in ["response_id", "parent_response_id", "account_id"] {
        assert!(
            body["client_metadata"].get(field).is_none(),
            "{field} must remain scoped"
        );
    }
}

#[tokio::test]
async fn full_http_request_preserves_client_header_and_metadata_tokens() {
    let (_store, server, provider) = http_fixture().await;
    let mut body = full_body();
    body["client_metadata"]["x-codex-turn-state"] = json!("unobserved-token");
    collect_response(
        &provider,
        full_request(body, Some(TOKEN), false),
        context("req_turn_echo", CancellationToken::new()),
    )
    .await
    .unwrap();
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests[0].headers["x-codex-turn-state"], TOKEN);
    assert_eq!(
        captured_request_body(&requests[0])["client_metadata"]["x-codex-turn-state"],
        "unobserved-token"
    );
}

#[tokio::test]
async fn turn_state_from_failed_http_response_survives_client_retry() {
    let (_store, server, provider) = http_fixture().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ResponseTemplate::new(400)
            .insert_header("x-codex-turn-state", TOKEN)
            .set_body_json(json!({"error": {"type": "invalid_request_error", "message": "Invalid request."}})))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server).await;
    assert!(
        collect_response(
            &provider,
            full_request(full_body(), None, false),
            context("req_turn_first", CancellationToken::new())
        )
        .await
        .is_err()
    );
    collect_response(
        &provider,
        full_request(full_body(), Some(TOKEN), false),
        context("req_turn_echo", CancellationToken::new()),
    )
    .await
    .unwrap();
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].headers["x-codex-turn-state"], TOKEN);
}

#[tokio::test]
async fn client_turn_state_does_not_authorize_an_unowned_previous_response_id() {
    let (_store, server, provider) = http_fixture().await;
    collect_response(
        &provider,
        full_request(full_body(), None, false),
        context("req_turn_first", CancellationToken::new()),
    )
    .await
    .unwrap();
    let mut body = full_body();
    body["previous_response_id"] = json!("unowned-response");
    let error = collect_response(
        &provider,
        full_request(body, Some(TOKEN), false),
        context("req_turn_echo", CancellationToken::new()),
    )
    .await
    .unwrap_err();
    assert_eq!(
        error
            .client_visible_upstream_error()
            .and_then(|detail| detail.code()),
        Some("previous_response_not_found")
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn full_http_replay_preserves_turn_state_without_local_proof() {
    for case in ["new_provider", "expired_affinity", "missing_turn"] {
        let (store, server, provider) = http_fixture().await;
        let first_context = if case == "expired_affinity" {
            AttemptContext::new(
                RequestAttemptContext::new(
                    ModelRequestId::new("req_turn_first").unwrap(),
                    ClientApiKeyId::new("key_openai_contract").unwrap(),
                ),
                NonZeroU32::new(1).unwrap(),
                SystemTime::now() + Duration::from_secs(30),
                account_policy().with_openai_session_affinity_ttl(Duration::ZERO),
                AccountAttemptContext::new(BTreeSet::new(), None, None)
                    .with_account_scope(contract_account_scope()),
                None,
                CancellationToken::new(),
            )
        } else {
            context("req_turn_first", CancellationToken::new())
        };
        collect_response(
            &provider,
            full_request(full_body(), None, false),
            first_context,
        )
        .await
        .unwrap();
        let provider = if case == "new_provider" {
            provider_with_base_url(&store, server.uri())
        } else {
            provider
        };
        let mut body = full_body();
        if case == "missing_turn" {
            body["client_metadata"]
                .as_object_mut()
                .unwrap()
                .remove("turn_id");
        }
        for key in ["turnState", "turn_state", "x-codex-turn-state"] {
            body[key] = json!(TOKEN);
            body["client_metadata"][key] = json!(TOKEN);
        }
        collect_response(
            &provider,
            full_request(body, Some(TOKEN), false),
            context("req_turn_echo", CancellationToken::new()),
        )
        .await
        .unwrap_or_else(|error| panic!("{case}: {error:?}"));
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests[1].headers["x-codex-turn-state"], TOKEN, "{case}");
        let body = captured_request_body(&requests[1]);
        let metadata: Value = serde_json::from_str(
            requests[1].headers["x-codex-turn-metadata"]
                .to_str()
                .unwrap(),
        )
        .unwrap();
        for key in ["turnState", "turn_state", "x-codex-turn-state"] {
            assert_eq!(body[key], TOKEN, "{case}: {key}");
            assert_eq!(body["client_metadata"][key], TOKEN, "{case}: {key}");
            assert_eq!(metadata[key], TOKEN, "{case}: {key}");
        }
    }
}

#[tokio::test]
async fn conflicting_account_owner_clears_client_turn_state() {
    let (_store, server, provider) = http_fixture().await;
    let mut body = full_body();
    for key in ["turnState", "turn_state", "x-codex-turn-state"] {
        body[key] = json!(TOKEN);
        body["client_metadata"][key] = json!(TOKEN);
    }
    collect_response(
        &provider,
        full_request(body, Some(TOKEN), false),
        context_with_state_owner("req_turn_switch", "acct_scope_old"),
    )
    .await
    .unwrap();
    let requests = server.received_requests().await.unwrap();
    assert!(!requests[0].headers.contains_key("x-codex-turn-state"));
    let body = captured_request_body(&requests[0]);
    let metadata: Value = serde_json::from_str(
        requests[0].headers["x-codex-turn-metadata"]
            .to_str()
            .unwrap(),
    )
    .unwrap();
    for key in ["turnState", "turn_state", "x-codex-turn-state"] {
        assert!(body.get(key).is_none(), "{key}");
        assert!(body["client_metadata"].get(key).is_none(), "{key}");
        assert!(metadata.get(key).is_none(), "{key}");
    }
}

#[tokio::test]
async fn full_http_request_does_not_restore_a_token_omitted_by_the_client() {
    let (_store, server, provider) = http_fixture().await;
    for request_id in ["req_turn_first", "req_turn_omitted"] {
        collect_response(
            &provider,
            full_request(full_body(), None, false),
            context(request_id, CancellationToken::new()),
        )
        .await
        .unwrap();
    }
    let requests = server.received_requests().await.unwrap();
    assert!(!requests[1].headers.contains_key("x-codex-turn-state"));
    assert!(
        captured_request_body(&requests[1])["client_metadata"]
            .get("x-codex-turn-state")
            .is_none()
    );
}

#[tokio::test]
async fn websocket_metadata_turn_state_survives_full_replay_after_completion_or_failure() {
    for (fail_first, new_provider) in [(false, false), (true, false), (false, true), (true, true)] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_session_affinity").await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut websocket = accept_codex_test_websocket(stream).await;
            websocket.next().await.unwrap().unwrap();
            websocket
                .send(Message::Text(
                    json!({
                        "type": "response.metadata", "headers": {"x-codex-turn-state": [TOKEN]}
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            if !fail_first {
                websocket
                    .send(Message::Text(
                        CAPTURE_COMPLETED_SSE
                            .lines()
                            .find_map(|line| line.strip_prefix("data: "))
                            .unwrap()
                            .to_owned()
                            .into(),
                    ))
                    .await
                    .unwrap();
            }
            websocket.close(None).await.unwrap();
            let (stream, _) = listener.accept().await.unwrap();
            websocket = accept_codex_test_websocket(stream).await;
            let echo: Value = loop {
                match websocket.next().await.unwrap().unwrap() {
                    Message::Text(text) => break serde_json::from_str(&text).unwrap(),
                    Message::Ping(payload) => websocket.send(Message::Pong(payload)).await.unwrap(),
                    Message::Close(_) => {
                        let (stream, _) = listener.accept().await.unwrap();
                        websocket = accept_codex_test_websocket(stream).await;
                    }
                    other => panic!("expected response.create, got {other:?}"),
                }
            };
            websocket
                .send(Message::Text(
                    CAPTURE_COMPLETED_SSE
                        .lines()
                        .find_map(|line| line.strip_prefix("data: "))
                        .unwrap()
                        .to_owned()
                        .into(),
                ))
                .await
                .unwrap();
            echo
        });
        let provider = provider_with_base_url(&store, base_url.clone());
        let first = collect_response(
            &provider,
            full_request(full_body(), None, true),
            context("req_turn_first", CancellationToken::new()),
        )
        .await;
        assert_eq!(first.is_err(), fail_first);
        let provider = if new_provider {
            provider_with_base_url(&store, base_url)
        } else {
            provider
        };
        let mut body = full_body();
        body["client_metadata"]["x-codex-turn-state"] = json!(TOKEN);
        collect_response(
            &provider,
            full_request(body, None, true),
            context("req_turn_echo", CancellationToken::new()),
        )
        .await
        .unwrap();
        let echo = timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            echo["client_metadata"]["x-codex-turn-state"], TOKEN,
            "failed first response: {fail_first}, new provider: {new_provider}"
        );
        assert!(echo.get("previous_response_id").is_none());
    }
}
