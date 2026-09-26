use super::*;

const SELECTED_ACCOUNT: &str = "acct_scope_same";

fn isolation_history() -> Value {
    json!([
        {"role":"user", "content":"hello"},
        {"type":"function_call", "id":"fc_history", "call_id":"call_history", "name":"lookup", "arguments":"{}"},
        {
            "type":"function_call_output", "call_id":"call_history", "output":"tool result",
            "internal_chat_message_metadata_passthrough": {
                "executed_tool_calls": [{
                    "name":"lookup", "arguments":{},
                    "tool_result_metadata": {
                        "openai/resource_access":{"resource_coverage":"complete", "resources":[]},
                        "parent_response_id":"tool-business-extension"
                    }
                }]
            }
        },
        {"role":"user", "content":"continue"}
    ])
}

fn isolation_payload() -> ProtocolPayload {
    ProtocolPayload::json_object(
        "openai",
        json!({
            "model": "gpt-5.4",
            "input": isolation_history(),
            "client_metadata": {
                "parent_response_id": "resp_parent_old",
                "guardian_credits_requested": "true",
                "x-codex-turn-metadata": r#"{"parent_response_id":"client-correlation"}"#,
                "mcp_attribution": r#"{"status":"complete","sources":[]}"#
            }
        })
        .as_object()
        .unwrap()
        .clone(),
    )
    .unwrap()
    .with_context(Map::from_iter([(
        "opaque_request_headers".to_owned(),
        json!([
            ["X-OpenAI-Account-Routing-Override", STANDARD.encode("us")],
            [
                "x-openai-account-routing-override",
                STANDARD.encode("us_cr")
            ],
            ["X-OpenAI-Fedramp", STANDARD.encode("true")],
            ["x-openai-fedramp", STANDARD.encode("false")],
            ["x-codex-guardian", STANDARD.encode("true")],
            ["x-business-extension", STANDARD.encode("first")],
            ["x-business-extension", STANDARD.encode("second")]
        ]),
    )]))
}

fn isolation_context(owner: Option<&str>) -> AttemptContext {
    match owner {
        Some(owner) => context_with_state_owner("req_account_isolation", owner),
        None => context("req_account_isolation", CancellationToken::new()),
    }
}

fn assert_isolated_request(
    headers: &reqwest::header::HeaderMap,
    body: &Value,
    owner: Option<&str>,
) {
    for name in ["x-openai-account-routing-override", "x-openai-fedramp"] {
        assert!(
            !headers.contains_key(name),
            "leaked {name} with owner {owner:?}"
        );
    }
    assert_eq!(headers["x-codex-guardian"], "true");
    assert_eq!(
        headers
            .get_all("x-business-extension")
            .iter()
            .map(|value| value.as_bytes())
            .collect::<Vec<_>>(),
        vec![b"first".as_slice(), b"second".as_slice()],
    );
    assert_eq!(
        body.pointer("/client_metadata/parent_response_id")
            .and_then(Value::as_str),
        (owner == Some(SELECTED_ACCOUNT)).then_some("resp_parent_old"),
    );
    assert_eq!(
        body["client_metadata"]["guardian_credits_requested"],
        "true"
    );
    assert_eq!(
        serde_json::from_str::<Value>(
            body["client_metadata"]["x-codex-turn-metadata"]
                .as_str()
                .unwrap()
        )
        .unwrap(),
        json!({"parent_response_id": "client-correlation"}),
    );
    assert_eq!(
        body["client_metadata"]["mcp_attribution"],
        r#"{"status":"complete","sources":[]}"#
    );
    assert_eq!(body["input"], isolation_history());
}

#[tokio::test]
async fn http_isolates_account_headers_and_only_scopes_guardian_parent_reference() {
    for owner in [Some(SELECTED_ACCOUNT), Some("acct_scope_old"), None] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, SELECTED_ACCOUNT).await;
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
        let mut payload = isolation_payload();
        let mut context = payload.context().clone();
        context.insert("use_websocket".to_owned(), json!(false));
        payload = payload.with_context(context);
        let mut stream = provider_with_base_url(&store, server.uri())
            .execute(
                planned_request(
                    "openai",
                    Operation::Generate(GenerateRequest::from_protocol_payload(payload)),
                ),
                isolation_context(owner),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert_isolated_request(
            &requests[0].headers,
            &captured_request_body(&requests[0]),
            owner,
        );
    }
}

#[tokio::test]
async fn websocket_isolates_account_headers_and_only_scopes_guardian_parent_reference() {
    for owner in [Some(SELECTED_ACCOUNT), Some("acct_scope_old"), None] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, SELECTED_ACCOUNT).await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut headers = None;
            let mut socket =
                crate::transport::accept_codex_test_websocket_with(socket, |request, _| {
                    headers = Some(request.headers().clone());
                })
                .await;
            let message = socket.next().await.unwrap().unwrap();
            let body: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
            socket.send(Message::Text(json!({
                "type": "response.completed",
                "response": {"id":"resp_isolation", "model":"gpt-5.4", "status":"completed", "output":[]}
            }).to_string().into())).await.unwrap();
            (headers.unwrap(), body)
        });
        let mut stream = provider_with_base_url(&store, base_url)
            .execute(
                planned_request(
                    "openai",
                    Operation::Generate(
                        GenerateRequest::from_protocol_payload(isolation_payload()),
                    ),
                ),
                isolation_context(owner),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
        let (headers, body) = server.await.unwrap();
        assert_isolated_request(&headers, &body, owner);
    }
}

#[tokio::test]
async fn middleware_cannot_reintroduce_account_headers_before_http_send_or_websocket_open() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, SELECTED_ACCOUNT).await;
    let server = MockServer::start().await;
    for operation in [http_generate_operation(), generate_operation()] {
        for name in [
            "x-openai-account-routing-override",
            "x-openai-fedramp",
            "x-codex-installation-id",
        ] {
            let result = provider_with_base_url(&store, server.uri())
                .execute(
                    planned_request("openai", operation.clone()),
                    context_with_middleware(
                        "req_managed_account_header",
                        Arc::new(RecordingMiddleware {
                            observed: Arc::default(),
                            replacement: ("service_tier".to_owned(), json!("default")),
                            request_headers: vec![MiddlewareHeader::new(
                                name,
                                Bytes::from_static(b"downstream-value"),
                            )],
                        }),
                        false,
                    ),
                )
                .await;
            let error = match result {
                Err(error) => error,
                Ok(mut stream) => match stream.next().await {
                    Some(Err(error)) => error,
                    _ => panic!("managed account header must fail before send: {name}"),
                },
            };
            assert_eq!(error.kind(), ProviderErrorKind::Protocol);
            assert_eq!(error.send_state(), UpstreamSendState::NotSent);
        }
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}
