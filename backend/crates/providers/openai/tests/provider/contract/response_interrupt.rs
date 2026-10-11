//! 验证未知控制帧和迟到中断使用原连接，并在复用时撤销旧执行控制权

use super::*;
use gateway_core::engine::response_control::{ResponseControl, ResponseControlUnavailable};

#[tokio::test]
async fn cancelled_queued_control_is_not_forwarded_to_upstream() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut ws = accept_codex_test_websocket(socket).await;
        assert_eq!(next_json(&mut ws).await["type"], "response.create");
        ws.send(Message::Text(
            json!({
                "type":"response.output_text.delta", "output_index":0,
                "content_index":0, "delta":"partial"
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
        let received = next_json(&mut ws).await;
        assert_eq!(
            received["marker"], "active",
            "cancelled command must never reach upstream"
        );
    });
    let control = ResponseControl::default();
    let mut stream = provider_with_base_url(&store, base_url)
        .execute(
            planned_request("openai", generate_operation()),
            interrupt_context(control.clone(), None),
        )
        .await
        .unwrap();
    loop {
        if stream.next().await.unwrap().unwrap().has_client_event() {
            break;
        }
    }
    {
        // 单线程 runtime 中只入队一次，未让 pump 执行便取消调用方
        let mut cancelled =
            Box::pin(control.send(r#"{"type":"future.control","marker":"cancelled"}"#));
        assert!(futures::poll!(cancelled.as_mut()).is_pending());
    }
    timeout(
        Duration::from_secs(2),
        control.send(r#"{"type":"future.control","marker":"active"}"#),
    )
    .await
    .unwrap()
    .unwrap();
    timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn controls_reject_closed_socket_without_losing_backpressured_close() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let (burst, ready) = oneshot::channel();
    let (closed, closing) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut ws = accept_codex_test_websocket(socket).await;
        next_json(&mut ws).await;
        let delta = Message::Text(
            json!({
                "type":"response.output_text.delta", "output_index":0,
                "content_index":0, "delta":"partial"
            })
            .to_string()
            .into(),
        );
        ws.send(delta.clone()).await.unwrap();
        ready.await.unwrap();
        // 填满 exchange 的 16 帧和 pump 的 64 帧，并占住正在转发的一帧
        for _ in 0..81 {
            ws.send(delta.clone()).await.unwrap();
        }
        // Pong 证明 pump 已读取前面的突发数据，随后 Close 必须等待下游排空
        ws.send(Message::Ping(vec![1].into())).await.unwrap();
        assert!(matches!(
            ws.next().await.unwrap().unwrap(),
            Message::Pong(_)
        ));
        ws.close(Some(CloseFrame {
            code: CloseCode::Normal,
            reason: "".into(),
        }))
        .await
        .unwrap();
        closed.send(()).unwrap();
        // 保留 TCP 直到客户端确认关闭，避免把 Close 场景变成连接重置
        let _ = ws.next().await;
    });
    let control = ResponseControl::default();
    let mut stream = provider_with_base_url(&store, base_url)
        .execute(
            planned_request("openai", generate_operation()),
            interrupt_context(control.clone(), None),
        )
        .await
        .unwrap();
    loop {
        if stream.next().await.unwrap().unwrap().has_client_event() {
            break;
        }
    }
    burst.send(()).unwrap();
    timeout(Duration::from_secs(2), closing)
        .await
        .unwrap()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        timeout(
            Duration::from_secs(2),
            control.send(r#"{"type":"response.interrupt"}"#)
        )
        .await
        .unwrap(),
        Err(ResponseControlUnavailable)
    );
    let (delivered, error) = timeout(Duration::from_secs(2), async {
        let mut delivered = 0;
        loop {
            match stream.next().await.unwrap() {
                Ok(event) => delivered += usize::from(event.has_client_event()),
                Err(error) => break (delivered, error),
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(delivered, 81);
    assert_eq!(error.diagnostic().unwrap().code(), Some("upstream_close"));
    assert!(error.diagnostic().unwrap().as_str().contains("1000"));
    assert!(
        error.pre_delivery_retry().is_none(),
        "delivered output cannot be replayed"
    );
    timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

async fn next_json(socket: &mut tokio_tungstenite::WebSocketStream<TcpStream>) -> Value {
    loop {
        match socket.next().await.unwrap().unwrap() {
            Message::Text(text) => return serde_json::from_str(&text).unwrap(),
            Message::Ping(payload) => socket.send(Message::Pong(payload)).await.unwrap(),
            Message::Pong(_) => {}
            other => panic!("unexpected synthetic upstream frame: {other:?}"),
        }
    }
}

fn interrupt_context(control: ResponseControl, previous: Option<&str>) -> AttemptContext {
    let account = ProviderAccountId::new("acct_provider_contract").unwrap();
    let provider = ProviderKind::new("openai").unwrap();
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_interrupt").unwrap(),
            ClientApiKeyId::new("key_openai_contract").unwrap(),
        )
        .with_response_control(Some(control)),
        NonZeroU32::new(1).unwrap(),
        SystemTime::now() + Duration::from_secs(30),
        account_policy(),
        AccountAttemptContext::new(
            BTreeSet::new(),
            None,
            previous.map(|_| ProviderAccountStateOwner::new(provider.clone(), account.clone())),
        )
        .with_account_scope(contract_account_scope()),
        previous.map(|id| {
            ContinuationBinding::Pinned(NativeContinuationPin::new(
                PreviousResponseId::new(id),
                PreviousResponseId::new(id),
                ClientApiKeyId::new("key_openai_contract").unwrap(),
                provider,
                account,
            ))
        }),
        CancellationToken::new(),
    )
}

#[tokio::test]
async fn response_controls_use_the_original_socket_through_terminal_and_revoke_on_reuse() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let (finish, finished) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = accept_codex_test_websocket(socket).await;
        for id in ["resp_interrupt_a", "resp_interrupt_b"] {
            let create = next_json(&mut socket).await;
            assert_eq!(create["type"], "response.create");
            if id == "resp_interrupt_b" {
                assert_eq!(create["previous_response_id"], "resp_interrupt_a");
            }
            socket
                .send(Message::Text(
                    json!({
                        "type": "response.created",
                        "response": {"id":id, "model":"gpt-5.4", "status":"in_progress"}
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            socket.send(Message::Text(json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"partial"}).to_string().into())).await.unwrap();
            let unknown = next_json(&mut socket).await;
            assert_eq!(
                unknown,
                json!({"type":"future.control", "extension":{"mode":"future"}})
            );
            socket
                .send(Message::Text(
                    json!({"type":"future.ack", "extension":true})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            let interrupt = next_json(&mut socket).await;
            assert_eq!(
                interrupt,
                json!({"type":"response.interrupt", "response_id":id,"mode":"discard_partial_items"})
            );
            socket
                .send(Message::Text(
                    json!({
                        "type":"response.incomplete",
                        "response":{"id":id,"model":"gpt-5.4","status":"incomplete","incomplete_details":{"reason":"interrupted"},"output":[]}
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            let late = next_json(&mut socket).await;
            assert_eq!(
                late,
                json!({"type":"response.interrupt", "response_id":id,"mode":"future_mode", "extra":1})
            );
            socket
                .send(Message::Text(
                    json!({"type":"future.idle_ack", "original":late})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
        }
        let _ = finished.await;
    });
    let provider = provider_with_base_url(&store, base_url);
    let mut session_state = None;
    let mut old_control: Option<ResponseControl> = None;
    for id in ["resp_interrupt_a", "resp_interrupt_b"] {
        let control = ResponseControl::default();
        let previous = (id == "resp_interrupt_b").then_some("resp_interrupt_a");
        let mut request = generate_with_persisted_session_context(
            "acct_provider_contract",
            "interrupt-conversation",
            "interrupt-session",
            "interrupt-thread",
        );
        if let Some(state) = session_state.take() {
            request = request.with_provider_session_state(state);
        }
        let mut stream = provider
            .clone()
            .execute(
                planned_request("openai", Operation::Generate(request)),
                interrupt_context(control.clone(), previous),
            )
            .await
            .unwrap();
        let mut unknown_reply = false;
        let mut sent = false;
        timeout(Duration::from_secs(5), async {
            while let Some(event) = stream.next().await {
                let event = event.unwrap();
                if let Some(state) = event.session_update() {
                    session_state = Some(state.clone());
                }
                if !sent {
                    if let Some(old) = old_control.take() {
                        assert_eq!(old.send("old control").await, Err(ResponseControlUnavailable));
                    }
                    control.send(&json!({"type":"future.control", "extension":{"mode":"future"}}).to_string()).await.unwrap();
                    control.send(&json!({"type":"response.interrupt", "response_id":id,"mode":"discard_partial_items"}).to_string()).await.unwrap();
                    sent = true;
                }
                unknown_reply |= event.wire_event().and_then(|wire| wire.event_type()) == Some("future.ack");
            }
        })
        .await
        .expect("controls complete the active response");
        assert!(unknown_reply, "unknown upstream events remain visible");
        assert!(session_state.is_some());
        let late =
            json!({"type":"response.interrupt", "response_id":id,"mode":"future_mode", "extra":1});
        control.send(&late.to_string()).await.unwrap();
        let reply = timeout(Duration::from_secs(5), control.receive())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&reply).unwrap(),
            json!({"type":"future.idle_ack", "original":late})
        );
        // 取消空闲读取后，下一轮正文必须能继续消费同一个有界接收通道
        assert!(
            timeout(Duration::from_millis(10), control.receive())
                .await
                .is_err()
        );
        old_control = Some(control);
    }
    finish.send(()).unwrap();
    timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    let control = old_control.unwrap();
    assert_eq!(
        timeout(Duration::from_secs(5), control.receive())
            .await
            .unwrap(),
        Err(ResponseControlUnavailable)
    );
    assert_eq!(
        control.send("closed connection").await,
        Err(ResponseControlUnavailable)
    );
}

fn lite_operation() -> Operation {
    Operation::Generate(
        GenerateRequest::from_protocol_payload(
            ProtocolPayload::json_object(
                "openai",
                Map::from_iter([
                    ("model".to_owned(), json!("gpt-5.4")),
                    ("input".to_owned(), json!("hello")),
                    ("store".to_owned(), json!(false)),
                    ("session_id".to_owned(), json!("lite-session")),
                ]),
            )
            .unwrap()
            .with_context(Map::from_iter([
                (
                    "downstream_websocket_connection_id".to_owned(),
                    json!("ws_lite"),
                ),
                ("responses_lite".to_owned(), json!("true")),
            ])),
        )
        .with_provider_session_state(
            ProviderSessionState::new(
                "openai",
                Map::from_iter([
                    ("account_id".to_owned(), json!("acct_provider_contract")),
                    ("conversation_id".to_owned(), json!("lite-conversation")),
                    ("continuation_scope".to_owned(), json!("persisted")),
                ]),
            )
            .unwrap(),
        ),
    )
}

fn lite_context(control: ResponseControl) -> AttemptContext {
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_lite").unwrap(),
            ClientApiKeyId::new("key_openai_contract").unwrap(),
        )
        .with_response_control(Some(control))
        .with_middleware(
            None,
            Arc::from([]),
            "/v1/responses".to_owned(),
            ClientTransport::WebSocket,
        ),
        NonZeroU32::new(1).unwrap(),
        SystemTime::now() + Duration::from_secs(30),
        account_policy(),
        AccountAttemptContext::new(BTreeSet::new(), None, None)
            .with_account_scope(contract_account_scope()),
        None,
        CancellationToken::new(),
    )
}

#[tokio::test]
async fn lite_created_is_delivered_before_output_and_interrupt_completes_it() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut ws = accept_codex_test_websocket(socket).await;
        next_json(&mut ws).await;
        ws.send(Message::Text(
            json!({"type":"codex.response.metadata","model":"gpt-5.4"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        ws.send(Message::Text(json!({"type":"response.created","response":{"id":"resp_lite","model":"gpt-5.4","status":"in_progress","output":[]}}).to_string().into())).await.unwrap();
        let interrupt = next_json(&mut ws).await;
        assert_eq!(interrupt["response_id"], "resp_lite");
        assert_eq!(interrupt["type"], "response.interrupt");
        ws.send(Message::Text(json!({"type":"response.incomplete","response":{"id":"resp_lite","model":"gpt-5.4","status":"incomplete","output":[],"incomplete_details":{"reason":"interrupted"}}}).to_string().into())).await.unwrap();
    });
    let control = ResponseControl::default();
    let mut stream = provider_with_base_url(&store, base_url)
        .execute(
            planned_request("openai", lite_operation()),
            lite_context(control.clone()),
        )
        .await
        .unwrap();
    let metadata = timeout(
        Duration::from_secs(3),
        super::precommit::next_client_event(&mut stream),
    )
    .await
    .unwrap();
    assert_eq!(
        metadata.wire_event().unwrap().event_type(),
        Some("codex.response.metadata")
    );
    assert!(metadata.is_replayable_prelude());
    let created = timeout(
        Duration::from_secs(3),
        super::precommit::next_client_event(&mut stream),
    )
    .await
    .expect("created cannot wait for output");
    assert_eq!(
        created.wire_event().unwrap().event_type(),
        Some("response.created")
    );
    assert!(created.is_replayable_prelude());
    control.send(&json!({"type":"response.interrupt","response_id":"resp_lite","mode":"discard_partial_items"}).to_string()).await.unwrap();
    let terminal = timeout(
        Duration::from_secs(3),
        super::precommit::next_client_event(&mut stream),
    )
    .await
    .unwrap();
    assert_eq!(
        terminal.wire_event().unwrap().event_type(),
        Some("response.incomplete")
    );
    assert!(!terminal.is_replayable_prelude());
    server.await.unwrap();
}

#[tokio::test]
async fn lite_prelude_preserves_overload_and_connection_limit_recovery() {
    for connection_limit in [false, true] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_provider_contract").await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let (release, released) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = accept_codex_test_websocket(socket).await;
            next_json(&mut ws).await;
            ws.send(Message::Text(json!({"type":"response.created","response":{"id":"resp_lite","model":"gpt-5.4","status":"in_progress","output":[]}}).to_string().into())).await.unwrap();
            released.await.unwrap();
            let error = if connection_limit {
                json!({"type":"error","status":400,"error":{"type":"invalid_request_error","code":"websocket_connection_limit_reached","message":"expired"}})
            } else {
                json!({"type":"error","error":{"type":"service_unavailable_error","code":"server_is_overloaded","message":"busy"}})
            };
            ws.send(Message::Text(error.to_string().into()))
                .await
                .unwrap();
        });
        let mut stream = provider_with_base_url_and_retry_budget(&store, base_url, 1)
            .execute(
                planned_request("openai", lite_operation()),
                lite_context(ResponseControl::default()),
            )
            .await
            .unwrap();
        let created = timeout(
            Duration::from_secs(3),
            super::precommit::next_client_event(&mut stream),
        )
        .await
        .unwrap();
        assert!(created.is_replayable_prelude());
        release.send(()).unwrap();
        let (_, error) = timeout(
            Duration::from_secs(3),
            super::precommit::stream_failure(&mut stream),
        )
        .await
        .unwrap();
        assert!(error.replay_is_safe(), "{error:?}");
        assert_eq!(error.send_state(), UpstreamSendState::Sent);
        if connection_limit {
            assert!(matches!(
                error.pre_delivery_retry(),
                Some(PreDeliveryRetry::SameAccountTransportRetry { .. })
            ));
        }
        server.await.unwrap();
    }
}

#[tokio::test]
async fn lite_unknown_event_does_not_delay_created_or_interrupt() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let unknown = json!({"type":"response.future_progress","marker":"preserved"});
    let upstream_unknown = unknown.clone();
    let (release, released) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut ws = accept_codex_test_websocket(socket).await;
        next_json(&mut ws).await;
        ws.send(Message::Text(upstream_unknown.to_string().into()))
            .await
            .unwrap();
        // 客户端已收到未知事件后才创建响应，避免终态或后续事件替测试释放缓冲
        released.await.unwrap();
        ws.send(Message::Text(json!({"type":"response.created","response":{"id":"resp_unknown","model":"gpt-5.4","status":"in_progress","output":[]}}).to_string().into())).await.unwrap();
        let interrupt = next_json(&mut ws).await;
        assert_eq!(interrupt["response_id"], "resp_unknown");
        assert_eq!(interrupt["type"], "response.interrupt");
        ws.send(Message::Text(json!({"type":"response.incomplete","response":{"id":"resp_unknown","model":"gpt-5.4","status":"incomplete","output":[],"incomplete_details":{"reason":"interrupted"}}}).to_string().into())).await.unwrap();
    });
    let control = ResponseControl::default();
    let mut stream = provider_with_base_url(&store, base_url)
        .execute(
            planned_request("openai", lite_operation()),
            lite_context(control.clone()),
        )
        .await
        .unwrap();
    let event = timeout(
        Duration::from_secs(3),
        super::precommit::next_client_event(&mut stream),
    )
    .await
    .expect("unknown event cannot wait for created");
    assert_eq!(event.wire_event().unwrap().data(), &unknown);
    assert!(!event.is_replayable_prelude());
    release.send(()).unwrap();
    let created = timeout(
        Duration::from_secs(3),
        super::precommit::next_client_event(&mut stream),
    )
    .await
    .expect("created cannot wait for output");
    assert_eq!(
        created.wire_event().unwrap().data()["response"]["id"],
        "resp_unknown"
    );
    assert!(!created.is_replayable_prelude());
    control.send(&json!({"type":"response.interrupt","response_id":"resp_unknown","mode":"discard_partial_items"}).to_string()).await.unwrap();
    let terminal = timeout(
        Duration::from_secs(3),
        super::precommit::next_client_event(&mut stream),
    )
    .await
    .unwrap();
    assert_eq!(
        terminal.wire_event().unwrap().event_type(),
        Some("response.incomplete")
    );
    server.await.unwrap();
}

#[tokio::test]
async fn lite_unknown_delivery_closes_recovery_before_later_preludes() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let (release, released) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut ws = accept_codex_test_websocket(socket).await;
        next_json(&mut ws).await;
        for event in [
            json!({"type":"response.created","response":{"id":"resp_unknown","model":"gpt-5.4","status":"in_progress","output":[]}}),
            json!({"type":"response.future_progress","marker":"preserved"}),
            json!({"type":"response.in_progress","response":{"id":"resp_unknown","output":[]}}),
        ] {
            ws.send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        released.await.unwrap();
        ws.send(Message::Text(json!({"type":"error","error":{"type":"service_unavailable_error","code":"server_is_overloaded","message":"busy"}}).to_string().into())).await.unwrap();
    });
    let mut stream = provider_with_base_url_and_retry_budget(&store, base_url, 1)
        .execute(
            planned_request("openai", lite_operation()),
            lite_context(ResponseControl::default()),
        )
        .await
        .unwrap();
    for (event_type, replayable) in [
        ("response.created", true),
        ("response.future_progress", false),
        ("response.in_progress", false),
    ] {
        let event = timeout(
            Duration::from_secs(3),
            super::precommit::next_client_event(&mut stream),
        )
        .await
        .expect("normal events cannot wait for an error or output");
        assert_eq!(event.wire_event().unwrap().event_type(), Some(event_type));
        assert_eq!(event.is_replayable_prelude(), replayable);
    }
    release.send(()).unwrap();
    let (_, error) = timeout(
        Duration::from_secs(3),
        super::precommit::stream_failure(&mut stream),
    )
    .await
    .unwrap();
    assert!(!error.replay_is_safe(), "{error:?}");
    assert!(error.pre_delivery_retry().is_none());
    assert_eq!(error.send_state(), UpstreamSendState::Sent);
    server.await.unwrap();
}
