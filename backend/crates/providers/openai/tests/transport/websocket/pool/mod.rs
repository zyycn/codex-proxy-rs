//! 搜索能力变化时的连接复用与原生续接恢复

use super::*;

async fn search_test_server() -> (
    CodexBackendClient,
    Arc<CodexWebSocketPool>,
    tokio::sync::mpsc::UnboundedReceiver<(usize, Value)>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let server = tokio::spawn(async move {
        let mut connection = 0;
        while let Ok((socket, _)) = listener.accept().await {
            connection += 1;
            let mut websocket = accept_codex_test_websocket(socket).await;
            while let Some(Ok(message)) = websocket.next().await {
                match message {
                    Message::Text(text) => {
                        let request: Value = serde_json::from_str(&text).unwrap();
                        tx.send((connection, request)).unwrap();
                        websocket
                            .send(Message::Text(
                                completed_websocket_response("resp_search", 1, 1).into(),
                            ))
                            .await
                            .unwrap();
                    }
                    Message::Ping(_) => websocket.flush().await.unwrap(),
                    Message::Close(_) => {
                        websocket.flush().await.unwrap();
                        break;
                    }
                    other => panic!("unexpected frame: {other:?}"),
                }
            }
        }
    });
    let pool = Arc::new(CodexWebSocketPool::default());
    let client = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    (client, pool, rx, server)
}

fn search_request(downstream_ws: bool, overrides: Value) -> CodexResponsesRequest {
    let mut body = codex_request_body("gpt-test", "be brief", vec![]);
    body.insert("store".into(), json!(false));
    body.extend(overrides.as_object().unwrap().clone());
    let mut request = CodexResponsesRequest::from_body(body);
    request.local_conversation_id = Some("search-session".into());
    request.downstream_websocket_connection_id = downstream_ws.then(|| "search-lane".into());
    request
}

async fn send(
    client: &CodexBackendClient,
    request: &CodexResponsesRequest,
) -> CodexClientResult<CollectedBackendResponse> {
    timeout(
        Duration::from_secs(3),
        client.create_response(
            request,
            request_context("req_search", Some("search-account")),
        ),
    )
    .await
    .expect("request must finish without waiting for pool expiry")
}

#[tokio::test]
async fn search_activation_reconnects_for_http_and_websocket_ingress() {
    for downstream_ws in [false, true] {
        for (warmup, mut declaration) in [
            (
                false,
                json!({"tools":[{"type":"web_search", "search_content_types":["text","image"]}]}),
            ),
            (
                true,
                json!({"tools":[{"type":"web_search", "external_web_access":false}]}),
            ),
            (
                false,
                json!({"input":[{"type":"additional_tools", "role":"developer", "tools":[{"type":"web_search"}]}]}),
            ),
            (
                false,
                json!({"input":[{"type":"tool_search_output", "tools":[{"type":"web_search_preview"}]}]}),
            ),
        ] {
            let (client, pool, mut seen, server) = search_test_server().await;
            let first = search_request(
                downstream_ws,
                json!({
                    "generate": !warmup,
                    "tools": [{"type":"function", "name":"web_search", "parameters":{}}]
                }),
            );
            send(&client, &first).await.unwrap();
            assert_eq!(seen.recv().await.unwrap().0, 1);

            declaration
                .as_object_mut()
                .unwrap()
                .entry("input")
                .or_insert_with(|| json!([]));
            declaration["input"].as_array_mut().unwrap().extend([
                json!({"type":"function_call", "name":"lookup", "call_id":"call_1", "arguments":"{}"}),
                json!({"type":"function_call_output", "call_id":"call_1", "output":"synthetic history"}),
            ]);
            let second = search_request(downstream_ws, declaration);
            let response = send(&client, &second).await.unwrap();
            let (connection, payload) = seen.recv().await.unwrap();
            pool.shutdown().await;
            server.abort();

            assert_eq!(connection, 2, "search activation must use a fresh socket");
            assert_eq!(response.websocket_pool_decision.unwrap().kind(), "new");
            assert_eq!(payload["input"], second.body()["input"]);
            assert_eq!(payload.get("tools"), second.body().get("tools"));
            assert!(payload.get("previous_response_id").is_none());
        }
    }
}

#[tokio::test]
async fn search_initialized_socket_survives_tool_removal_and_image_search_activation() {
    let (client, pool, mut seen, server) = search_test_server().await;
    for tools in [
        json!([{"type":"web_search", "external_web_access":false}]),
        json!([]),
        json!([{"type":"web_search", "search_content_types":["text","image"]}]),
    ] {
        let request = search_request(true, json!({"tools": tools}));
        send(&client, &request).await.unwrap();
        let (connection, payload) = seen.recv().await.unwrap();
        assert_eq!(connection, 1);
        assert_eq!(payload["tools"], tools);
    }
    pool.shutdown().await;
    server.abort();
}

#[tokio::test]
async fn search_activation_rejects_native_delta_then_accepts_full_replay() {
    let (client, pool, mut seen, server) = search_test_server().await;
    let request = search_request(true, json!({}));
    send(&client, &request).await.unwrap();
    seen.recv().await.unwrap();
    let mut request = search_request(true, json!({"tools":[{"type":"web_search"}]}));
    request.set_previous_response_id(Some("resp_search".into()));
    request.previous_response_scope = Some(PreviousResponseScope::ConnectionLocal);
    let result = send(&client, &request).await;
    if result.is_ok() {
        pool.shutdown().await;
        server.abort();
        panic!("connection-local delta must require client replay before sending");
    }
    let CodexClientError::WebSocket(error) = result.unwrap_err() else {
        panic!("expected a typed continuation error");
    };
    assert!(error.continuation_unavailable_reason().is_some());
    assert_eq!(
        error.connection_observation().unwrap().exit_reason(),
        "search_activation_required"
    );
    assert!(seen.try_recv().is_err(), "delta must never reach upstream");

    let request = search_request(
        true,
        json!({
            "tools":[{"type":"web_search"}],
            "input":[{"role":"user", "content":"complete synthetic history"}]
        }),
    );
    send(&client, &request).await.unwrap();
    let (connection, payload) = seen.recv().await.unwrap();
    assert_eq!(connection, 2);
    assert_eq!(payload["input"], request.body()["input"]);
    assert!(payload.get("previous_response_id").is_none());
    pool.shutdown().await;
    server.abort();
}
