//! 验证压缩请求的上游传输选择与显式策略、WS 连接约束

use super::*;

fn compaction_body() -> Value {
    json!({
        "model": "gpt-5.4", "stream": true, "store": false,
        "input": [{"role": "user", "content": "Summarize this conversation."}]
    })
}

fn request(body: Value, protocol_context: Map<String, Value>) -> GenerateRequest {
    GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", body.as_object().unwrap().clone())
            .unwrap()
            .with_context(protocol_context),
    )
}

#[tokio::test]
async fn http_compaction_preserves_http_and_account_policy() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("x-codex-turn-state", "compaction-routing-token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(CAPTURE_COMPLETED_SSE, "text/event-stream"),
        )
        .expect(5)
        .mount(&upstream)
        .await;

    for case in [
        "header",
        "metadata",
        "trigger",
        "explicit_http",
        "account_http",
    ] {
        let mut body = compaction_body();
        let mut protocol_context =
            Map::from_iter([("turn_state".to_owned(), json!("compaction-routing-token"))]);
        let turn_metadata = json!({"request_kind": "compaction"}).to_string();
        match case {
            "header" => {
                protocol_context.insert("turn_metadata".to_owned(), json!(turn_metadata));
            }
            "metadata" => {
                body["client_metadata"] = json!({"x-codex-turn-metadata": turn_metadata});
            }
            _ => {
                body["input"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"type": "compaction_trigger"}));
                if case == "explicit_http" {
                    protocol_context.insert("use_websocket".to_owned(), json!(false));
                } else if case == "account_http" {
                    store.set_oauth_transport(
                        "acct_provider_contract",
                        provider_openai::credential::ResponsesTransport::Http,
                    );
                    protocol_context.insert("use_websocket".to_owned(), json!(true));
                }
            }
        }
        let provider = provider_with_base_url(&store, upstream.uri());
        timeout(Duration::from_secs(10), async {
            let mut stream = provider
                .execute(
                    planned_request(
                        "openai",
                        Operation::Generate(request(body, protocol_context)),
                    ),
                    context("req_compaction_http", CancellationToken::new()),
                )
                .await
                .unwrap();
            assert_eq!(stream.metadata().transport().as_str(), "http_sse", "{case}");
            let mut completed = false;
            while let Some(event) = stream.next().await {
                completed |= event
                    .unwrap()
                    .wire_event()
                    .is_some_and(|wire| wire.data()["type"] == "response.completed");
            }
            assert!(
                completed,
                "{case}: HTTP compaction must deliver a terminal event"
            );
        })
        .await
        .expect("HTTP compaction completes");
    }
    assert_eq!(
        upstream.received_requests().await.unwrap().len(),
        5,
        "no WS opening or replay"
    );
    upstream.verify().await;
}

#[tokio::test]
async fn compaction_preserves_websocket_requirements_and_explicit_preference() {
    for case in [
        "explicit_ws",
        "ws_frame",
        "ws_connection",
        "ws_chain",
        "warmup",
        "ordinary_turn",
    ] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_provider_contract").await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = accept_codex_test_websocket(socket).await;
            let message = socket.next().await.unwrap().unwrap();
            let request: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
            socket.send(Message::text(json!({
                "type": "response.completed",
                "response": {"id": "resp_compaction", "model": "gpt-5.4", "status": "completed", "output": []}
            }).to_string())).await.unwrap();
            request
        });
        let mut body = compaction_body();
        body["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type": "compaction_trigger"}));
        let mut protocol_context = Map::new();
        match case {
            "explicit_ws" => {
                protocol_context.insert("use_websocket".to_owned(), json!(true));
            }
            "ws_frame" => {
                body["store"] = json!(true);
                protocol_context.insert("websocket_frame".to_owned(), json!(true));
            }
            "ws_connection" | "ws_chain" => {
                body["store"] = json!(case == "ws_connection");
                protocol_context.insert(
                    "downstream_websocket_connection_id".to_owned(),
                    json!("client-compaction-ws"),
                );
            }
            "warmup" => {
                body["generate"] = json!(false);
            }
            "ordinary_turn" => {
                // 历史中的压缩结果不是本次压缩触发，普通请求继续使用账号的 WS 优先策略
                body["input"] = json!([{"type": "compaction", "encrypted_content": "prior-compaction"},
                    {"role": "user", "content": "Continue."}]);
            }
            _ => unreachable!(),
        }
        // 测试 Provider 未安装会话身份服务，显式提供本地会话以满足 WS 连接池约束
        let request = request(body.clone(), protocol_context).with_provider_session_state(
            ProviderSessionState::new(
                "openai",
                Map::from_iter([
                    ("account_id".to_owned(), json!("acct_provider_contract")),
                    (
                        "conversation_id".to_owned(),
                        json!("compaction-transport-session"),
                    ),
                    ("continuation_scope".to_owned(), json!("persisted")),
                ]),
            )
            .unwrap(),
        );
        let provider = provider_with_base_url(&store, base_url);
        timeout(Duration::from_secs(10), async {
            let mut stream = Arc::clone(&provider)
                .execute(
                    planned_request("openai", Operation::Generate(request)),
                    context("req_compaction_ws", CancellationToken::new()),
                )
                .await
                .unwrap();
            assert_eq!(
                stream.metadata().transport().as_str(),
                "websocket",
                "{case}"
            );
            while let Some(event) = stream.next().await {
                event.unwrap_or_else(|error| panic!("{case}: {error:?}"));
            }
            let request = server.await.unwrap();
            assert_eq!(request["type"], "response.create", "{case}");
            assert_eq!(request["input"], body["input"], "{case}");
        })
        .await
        .expect("WS request completes");
    }
}
