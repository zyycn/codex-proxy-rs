//! 验证真实响应头只写回一次，流内失败不重复处理 Cookie

use super::*;

fn cookie_provider(store: &Arc<MemoryAccountStore>, base_url: String) -> Arc<CodexProvider> {
    let profile = wire_profile();
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let leases = Arc::new(TestLeaseCoordinator::default());
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
        Arc::clone(&leases) as Arc<dyn ProviderLeasePort>,
        crate::support::runtime_policy(),
    ));
    let feedback = Arc::new(AccountFeedbackStats::default());
    let selector = Arc::new(CodexCredentialSelector::new(
        ProviderKind::new("openai").unwrap(),
        store.repository(),
        leases,
        Arc::new(MemorySessionAffinity::default()),
        Arc::new(MemorySessionExclusions::default()),
        Arc::clone(&quota),
        Arc::clone(&feedback),
        // 测试仅允许本机模拟上游，生产 Cookie 白名单不放宽
        CodexCookiePolicy::new(["cf_clearance"], ["127.0.0.1"]).unwrap(),
    ));
    Arc::new(
        CodexProvider::new(
            selector,
            catalog,
            quota,
            feedback,
            http,
            profile,
            base_url,
            Arc::new(CodexWebSocketPool::default()),
            0,
        )
        .unwrap(),
    )
}

#[tokio::test]
async fn outbound_redirected_response_cookies_keep_the_actual_origin_on_success_and_failure() {
    for status in [200, 400] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_provider_contract").await;
        let original = store.account("acct_provider_contract").unwrap();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(status)
                    .insert_header("content-type", "text/event-stream")
                    .insert_header("set-cookie", "cf_clearance=foreign; Path=/; Max-Age=3600")
                    .set_body_string(if status == 200 {
                        CAPTURE_COMPLETED_SSE
                    } else {
                        r#"{"error":{"message":"invalid","type":"invalid_request_error"}}"#
                    }),
            )
            .expect(1)
            .mount(&server)
            .await;
        let target = format!(
            "{}/plugin-target",
            server.uri().replace("127.0.0.1", "localhost")
        );
        let mut stream = cookie_provider(&store, server.uri())
            .execute(
                planned_request("openai", http_generate_operation()),
                outbound_context("req_outbound_cookie_origin", move |request| {
                    let gateway_core::engine::middleware::outbound::Target::Http { uri, .. } =
                        &mut request.target
                    else {
                        panic!("HTTP target")
                    };
                    *uri = target.parse().unwrap();
                    Ok(())
                }),
            )
            .await
            .unwrap();
        let mut failed = false;
        while let Some(event) = stream.next().await {
            if event.is_err() {
                failed = true;
                break;
            }
        }
        assert_eq!(failed, status != 200);
        let current = store.account("acct_provider_contract").unwrap();
        assert_eq!(
            current.revision(),
            original.revision(),
            "localhost response must not be attributed to the allowed 127.0.0.1 origin"
        );
        assert!(
            store
                .repository()
                .load_complete_data(&current)
                .await
                .unwrap()
                .cookies()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn outbound_websocket_cookies_keep_the_actual_opening_origin_on_success_and_failure() {
    for rejected in [false, true] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_provider_contract").await;
        let original = store.account("acct_provider_contract").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (release_server, finish) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            // 错误分支需要真实 HTTP 拒绝头；回调类型由 tungstenite 固定
            #[allow(clippy::result_large_err)]
            let result = tokio_tungstenite::accept_hdr_async_with_config(
                stream,
                |_request: &tungstenite::handshake::server::Request,
                 mut response: tungstenite::handshake::server::Response| {
                    if rejected {
                        return Err(tungstenite::http::Response::builder()
                            .status(400)
                            .header("set-cookie", "cf_clearance=foreign; Path=/; Max-Age=3600")
                            .body(Some(
                                r#"{"error":{"message":"invalid","type":"invalid_request_error"}}"#
                                    .to_owned(),
                            ))
                            .unwrap());
                    }
                    response.headers_mut().insert(
                        "set-cookie",
                        "cf_clearance=foreign; Path=/; Max-Age=3600"
                            .parse()
                            .unwrap(),
                    );
                    Ok(response)
                },
                Some(crate::transport::websocket_accept_config()),
            )
            .await;
            if rejected {
                assert!(result.is_err());
                return;
            }
            let mut socket = result.unwrap();
            assert!(matches!(
                socket.next().await.unwrap().unwrap(),
                Message::Text(_)
            ));
            socket.send(Message::Text(json!({"type":"response.completed", "response":{"id":"resp_foreign_origin","model":"gpt-5.4","status":"completed","output":[]}}).to_string().into())).await.unwrap();
            let _ = finish.await;
        });
        let target = format!("ws://localhost:{}/plugin-target", address.port());
        let operation = Operation::Generate(
            GenerateRequest::from_protocol_payload(
                ProtocolPayload::json_object(
                    "openai",
                    json!({"model":"gpt-5.4", "input":"hello"})
                        .as_object()
                        .unwrap()
                        .clone(),
                )
                .unwrap()
                .with_context(Map::from_iter([
                    ("use_websocket".into(), json!(true)),
                    (
                        "downstream_websocket_connection_id".into(),
                        json!("outbound-cookie-origin"),
                    ),
                ])),
            )
            .with_provider_session_state(
                ProviderSessionState::new(
                    "openai",
                    json!({
                        "account_id":"acct_provider_contract",
                        "conversation_id":"outbound-cookie-origin",
                        "continuation_scope":"connection_local"
                    })
                    .as_object()
                    .unwrap()
                    .clone(),
                )
                .unwrap(),
            ),
        );
        let mut stream = cookie_provider(&store, format!("http://{address}"))
            .execute(
                planned_request("openai", operation),
                outbound_context("req_outbound_ws_cookie_origin", move |request| {
                    let gateway_core::engine::middleware::outbound::Target::WebSocket { uri } =
                        &mut request.target
                    else {
                        panic!("WebSocket target")
                    };
                    *uri = target.parse().unwrap();
                    Ok(())
                }),
            )
            .await
            .unwrap();
        let mut failure = None;
        timeout(Duration::from_secs(5), async {
            while let Some(event) = stream.next().await {
                if let Err(error) = event {
                    failure = Some(error);
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(failure.is_some(), rejected, "{failure:?}");
        let current = store.account("acct_provider_contract").unwrap();
        assert_eq!(current.revision(), original.revision());
        assert!(
            store
                .repository()
                .load_complete_data(&current)
                .await
                .unwrap()
                .cookies()
                .is_empty()
        );
        let _ = release_server.send(());
        server.await.unwrap();
    }
}

#[tokio::test]
async fn response_cookies_are_saved_once_for_stream_and_opening_failures() {
    for streaming in [true, false] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_provider_contract").await;
        let original = store.account("acct_provider_contract").unwrap();
        let server = MockServer::start().await;
        let failure = json!({"type":"error", "error":{"type":"server_error", "code":"server_error", "message":"backend unavailable"}});
        let response = if streaming {
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(format!("event: error\ndata: {failure}\n\n"))
        } else {
            ResponseTemplate::new(503).set_body_json(&failure)
        }
        .insert_header("set-cookie", "cf_clearance=current; Path=/; Max-Age=3600");
        Mock::given(method("POST"))
            .and(path("/codex/responses"))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
        let mut stream = cookie_provider(&store, server.uri())
            .execute(
                planned_request("openai", http_generate_operation()),
                context("req_cookie_failure", CancellationToken::new()),
            )
            .await
            .unwrap();
        let mut failure = None;
        while let Some(event) = stream.next().await {
            if let Err(error) = event {
                failure = Some(error);
                break;
            }
        }
        assert!(failure.is_some(), "simulated upstream must fail");
        let current = store.account("acct_provider_contract").unwrap();
        assert_eq!(
            current.revision().get(),
            original.revision().get() + 1,
            "streaming={streaming}"
        );
        let data = store
            .repository()
            .load_complete_data(&current)
            .await
            .unwrap();
        assert_eq!(data.cookies()[0].value, "current");
        server.verify().await;
    }
}
