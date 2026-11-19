//! 验证事件即时交付、前导事件与取消对重试边界的影响

use gateway_core::diagnostics::TraceContext;
use gateway_core::engine::provider::ProviderStream;
use gateway_core::error::ProviderError;
use gateway_core::event::ProviderEvent;
use gateway_protocol::openai::sse::encode_sse_event;

use super::*;

fn traced_context(trace: &TraceContext, cancellation: CancellationToken) -> AttemptContext {
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_precommit").unwrap(),
            ClientApiKeyId::new("key_openai_contract").unwrap(),
        )
        .with_trace(trace.clone()),
        NonZeroU32::new(1).unwrap(),
        SystemTime::now() + Duration::from_secs(3_600),
        account_policy(),
        AccountAttemptContext::new(BTreeSet::new(), None, None)
            .with_account_scope(contract_account_scope()),
        None,
        cancellation,
    )
}

fn structural_event(kind: &str, bytes: usize) -> Value {
    let mut event = json!({
        "type": kind,
        "response": {
            "id": "resp_precommit", "model": "gpt-5.4", "status": "in_progress",
            "instructions": "", "tools": [], "output": []
        }
    });
    event["response"]["instructions"] = json!("x".repeat(bytes - event.to_string().len()));
    assert_eq!(event.to_string().len(), bytes);
    event
}

fn overload() -> Value {
    json!({
        "type": "error",
        "error": {"type": "service_unavailable_error", "code": "server_is_overloaded", "message": "busy"}
    })
}

fn sse(event: &Value) -> String {
    encode_sse_event(event["type"].as_str().unwrap(), &event.to_string())
}

pub(super) async fn next_client_event(stream: &mut ProviderStream) -> ProviderEvent {
    loop {
        let event = stream.next().await.unwrap().unwrap();
        if event.has_client_event() {
            return event;
        }
    }
}

pub(super) async fn stream_failure(
    stream: &mut ProviderStream,
) -> (Vec<ProviderEvent>, ProviderError) {
    let mut events = Vec::new();
    loop {
        match stream.next().await.expect("upstream failure") {
            Ok(event) => events.push(event),
            Err(error) => return (events, error),
        }
    }
}

#[tokio::test]
async fn bare_websocket_bad_request_offers_http_error_only_for_native_responses_sse() {
    use ClientTransport::{HttpJson, HttpSse, WebSocket};
    use gateway_protocol::openai::RESPONSES_PATH;

    let detail = json!({
        "type":"invalid_request_error", "code":"invalid_value", "message":"Invalid input.",
        "param":"input[2].id", "extension":{"reason":"duplicate"}
    });
    let bad_request = json!({"type":"error", "status":400, "error":detail, "sequence_number":7});
    let no_status = json!({"type":"error", "error":detail});
    let response_failed = json!({"type":"response.failed", "status":400, "response":{"id":"resp_failed","error":detail}});
    let capacity = json!({"status":400, "type":"error", "error":overload()["error"]});
    for (endpoint, transport, frame, expects_http) in [
        (RESPONSES_PATH, HttpSse, &bad_request, true),
        (RESPONSES_PATH, WebSocket, &bad_request, false),
        (RESPONSES_PATH, HttpJson, &bad_request, false),
        ("/v1/chat/completions", HttpSse, &bad_request, false),
        (RESPONSES_PATH, HttpSse, &no_status, false),
        (RESPONSES_PATH, HttpSse, &response_failed, false),
        (RESPONSES_PATH, HttpSse, &capacity, false),
    ] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_provider_contract").await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let raw = frame.to_string();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = accept_codex_test_websocket(socket).await;
            ws.next().await.unwrap().unwrap();
            ws.send(Message::Text(raw.into())).await.unwrap();
        });
        let request = RequestAttemptContext::new(
            ModelRequestId::new("req_ws_bad_input").unwrap(),
            ClientApiKeyId::new("key_openai_contract").unwrap(),
        )
        .with_middleware(None, Arc::from([]), endpoint.to_owned(), transport)
        .with_request_location(Some(global_request_location()));
        let mut stream = provider_with_base_url(&store, base_url)
            .execute(
                planned_request("openai", generate_operation()),
                context_for_request(request, CancellationToken::new()),
            )
            .await
            .unwrap();
        let (events, mut error) = stream_failure(&mut stream).await;
        assert!(events.iter().all(|event| !event.has_client_event()));
        let response = error.client_visible_upstream_response();
        assert_eq!(
            response.is_some(),
            expects_http,
            "{endpoint} {transport:?} {frame}"
        );
        if let Some(response) = response {
            assert_eq!(response.status(), 400);
            assert_eq!(
                response.content_type(),
                Some(b"application/json".as_slice())
            );
            assert_eq!(
                serde_json::from_slice::<Value>(response.body()).unwrap(),
                json!({"error":detail})
            );
        }
        let events = error.take_atomic_client_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].wire_event().unwrap().data(), frame);
        server.await.unwrap();
    }
}

#[tokio::test]
async fn large_preludes_are_forwarded_before_failure_on_http_and_websocket() {
    for websocket in [false, true] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_provider_contract").await;
        // 大段配置回显不应提前结束重试窗口；使用合成正文覆盖单帧与累计的大体积前导事件
        let created = structural_event("response.created", 300 * 1024);
        let progress = structural_event("response.in_progress", 300 * 1024);
        let failure = overload();
        let expected = vec![failure.clone()];
        let (base_url, release, server) = if websocket {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base_url = format!("http://{}", listener.local_addr().unwrap());
            let (release, released) = oneshot::channel();
            let server = tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                let mut ws = accept_codex_test_websocket(socket).await;
                ws.next().await.unwrap().unwrap();
                for event in [created, progress] {
                    ws.send(Message::Text(event.to_string().into()))
                        .await
                        .unwrap();
                }
                released.await.unwrap();
                ws.send(Message::Text(failure.to_string().into()))
                    .await
                    .unwrap();
            });
            (base_url, release, server)
        } else {
            let (base_url, release, _, server) =
                paused_chunked_sse_server(sse(&created) + &sse(&progress), sse(&failure)).await;
            (base_url, release, server)
        };
        let trace = TraceContext::new("req_precommit");
        let operation = if websocket {
            generate_operation()
        } else {
            http_generate_operation()
        };
        let mut stream = provider_with_base_url(&store, base_url)
            .execute(
                planned_request("openai", operation),
                traced_context(&trace, CancellationToken::new()),
            )
            .await
            .unwrap();

        for kind in ["response.created", "response.in_progress"] {
            let event = timeout(Duration::from_secs(1), next_client_event(&mut stream))
                .await
                .unwrap();
            assert_eq!(event.wire_event().unwrap().event_type(), Some(kind));
            assert!(event.is_replayable_prelude());
        }
        release.send(()).unwrap();
        let (events, mut error) = timeout(Duration::from_secs(1), stream_failure(&mut stream))
            .await
            .unwrap();
        assert!(events.iter().all(|event| !event.has_client_event()));
        assert!(error.replay_is_safe());
        assert!(matches!(
            error.pre_delivery_retry(),
            Some(PreDeliveryRetry::SameAccountTransientRetry { .. })
        ));
        let atomic = error.take_atomic_client_events();
        let actual: Vec<_> = atomic
            .iter()
            .filter_map(|event| event.wire_event().map(|wire| wire.data().clone()))
            .collect();
        assert_eq!(actual, expected);
        server.await.unwrap();
    }
}

#[tokio::test]
async fn immediate_delivery_preserves_wire_through_output_terminal_and_eof() {
    let semantic = json!({"type": "response.output_text.delta", "output_index": 0, "content_index": 0, "delta": "hello"});
    let terminal = json!({"type": "response.completed", "response": {"id": "resp_precommit", "model": "gpt-5.4", "status": "completed", "output": []}});
    let tool = json!({"type": "response.web_search_call.in_progress", "item_id": "search_precommit", "output_index": 0});
    for (reason, body) in [
        (
            "semantic_output",
            sse(&structural_event("response.created", 300 * 1024)) + &sse(&semantic),
        ),
        (
            "semantic_output",
            sse(&structural_event("response.created", 300 * 1024)) + &sse(&tool),
        ),
        (
            "terminal",
            sse(&structural_event("response.created", 300 * 1024)) + &sse(&terminal),
        ),
        (
            "eof",
            sse(&structural_event("response.created", 300 * 1024)),
        ),
    ] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_provider_contract").await;
        let (base_url, release, _, server) =
            paused_chunked_sse_server(body.clone(), String::new()).await;
        // EOF 是另一种边界；其余场景必须在上游结束前立即释放
        let mut release = Some(release);
        if reason == "eof" {
            release.take().unwrap().send(()).unwrap();
        }
        let trace = TraceContext::new("req_precommit");
        let mut stream = provider_with_base_url(&store, base_url)
            .execute(
                planned_request("openai", http_generate_operation()),
                traced_context(&trace, CancellationToken::new()),
            )
            .await
            .unwrap();
        let first = timeout(Duration::from_secs(1), next_client_event(&mut stream))
            .await
            .unwrap_or_else(|error| panic!("{reason} did not release immediately: {error}"));
        if let Some(release) = release {
            release.send(()).unwrap();
        }
        let mut wire = Vec::new();
        wire.extend_from_slice(first.wire_event().unwrap().raw_sse_frame().unwrap());
        while let Some(event) = stream.next().await {
            if let Some(frame) = event
                .unwrap()
                .wire_event()
                .and_then(|wire| wire.raw_sse_frame())
            {
                wire.extend_from_slice(frame);
            }
        }
        assert_eq!(wire, body.as_bytes());
        server.await.unwrap();
    }
}

#[tokio::test]
async fn cancelling_after_prelude_delivery_does_not_offer_replay() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let (base_url, release, _, server) = paused_chunked_sse_server(
        sse(&structural_event("response.created", 300 * 1024)),
        String::new(),
    )
    .await;
    let trace = TraceContext::new("req_precommit");
    let cancellation = CancellationToken::new();
    let mut stream = provider_with_base_url(&store, base_url)
        .execute(
            planned_request("openai", http_generate_operation()),
            traced_context(&trace, cancellation.clone()),
        )
        .await
        .unwrap();
    let created = timeout(Duration::from_secs(1), next_client_event(&mut stream))
        .await
        .unwrap();
    assert_eq!(
        created.wire_event().unwrap().event_type(),
        Some("response.created")
    );
    cancellation.cancel();
    let (events, error) = timeout(Duration::from_secs(1), stream_failure(&mut stream))
        .await
        .unwrap();
    assert_eq!(error.kind(), ProviderErrorKind::Cancelled);
    assert!(!error.replay_is_safe());
    assert!(events.iter().all(|event| !event.has_client_event()));
    release.send(()).unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn twenty_minute_structural_progress_preserves_late_overload_recovery() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let (progress, mut progress_requests) = tokio::sync::mpsc::channel::<()>(1);
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_http_request(&mut socket).await;
        socket.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n").await.unwrap();
        write_http_chunk(
            &mut socket,
            &sse(&structural_event("response.created", 1024)),
        )
        .await;
        while progress_requests.recv().await.is_some() {
            write_http_chunk(
                &mut socket,
                &sse(&structural_event("response.in_progress", 1024)),
            )
            .await;
        }
        write_http_chunk(&mut socket, &sse(&overload())).await;
        socket.write_all(b"0\r\n\r\n").await.unwrap();
    });
    let trace = TraceContext::new("req_precommit");
    let mut stream = provider_with_base_url(&store, base_url)
        .execute(
            planned_request("openai", http_generate_operation()),
            traced_context(&trace, CancellationToken::new()),
        )
        .await
        .unwrap();
    let created = timeout(Duration::from_secs(1), next_client_event(&mut stream))
        .await
        .unwrap();
    assert_eq!(
        created.wire_event().unwrap().event_type(),
        Some("response.created")
    );
    // 结构进度保持上游传输活跃，长思考不能额外触发首内容等待期限
    for _ in 0..20 {
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(60)).await;
        tokio::time::resume();
        progress.send(()).await.unwrap();
        let event = timeout(Duration::from_secs(1), next_client_event(&mut stream))
            .await
            .unwrap();
        assert_eq!(
            event.wire_event().unwrap().event_type(),
            Some("response.in_progress")
        );
        assert!(event.is_replayable_prelude());
    }
    drop(progress);
    let (events, error) = timeout(Duration::from_secs(1), stream_failure(&mut stream))
        .await
        .unwrap();
    assert!(events.iter().all(|event| !event.has_client_event()));
    assert!(error.replay_is_safe());
    assert!(matches!(
        error.pre_delivery_retry(),
        Some(PreDeliveryRetry::SameAccountTransientRetry { .. })
    ));
    server.await.unwrap();
}

#[tokio::test]
async fn large_prelude_stream_is_forwarded_without_retaining_it_for_failure() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    // 大段配置回显仍属于前导事件，累计体积不应改变上游失败的恢复方式
    let preamble = sse(&structural_event("response.in_progress", 1024 * 1024)).repeat(17);
    let (base_url, release, _, server) =
        paused_chunked_sse_server(preamble, sse(&overload())).await;
    let trace = TraceContext::new("req_precommit");
    let mut stream = provider_with_base_url(&store, base_url)
        .execute(
            planned_request("openai", http_generate_operation()),
            traced_context(&trace, CancellationToken::new()),
        )
        .await
        .unwrap();
    for _ in 0..17 {
        let event = timeout(Duration::from_secs(2), next_client_event(&mut stream))
            .await
            .unwrap();
        assert_eq!(
            event.wire_event().unwrap().event_type(),
            Some("response.in_progress")
        );
        assert!(event.is_replayable_prelude());
    }
    release.send(()).unwrap();
    let (events, mut error) = timeout(Duration::from_secs(10), stream_failure(&mut stream))
        .await
        .unwrap();
    assert!(events.iter().all(|event| !event.has_client_event()));
    assert!(error.replay_is_safe());
    assert!(matches!(
        error.pre_delivery_retry(),
        Some(PreDeliveryRetry::SameAccountTransientRetry { .. })
    ));
    let buffered_bytes: usize = error
        .take_atomic_client_events()
        .iter()
        .filter_map(|event| event.wire_event()?.raw_sse_frame())
        .map(bytes::Bytes::len)
        .sum();
    assert_eq!(buffered_bytes, sse(&overload()).len());
    server.await.unwrap();
}

#[tokio::test]
async fn websocket_disconnect_recovery_honors_request_retry_budget_without_session_affinity() {
    for max_retries in [0, 5] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_provider_contract").await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            for _ in 0..=max_retries {
                let (socket, _) = listener.accept().await.unwrap();
                let mut ws = accept_codex_test_websocket(socket).await;
                ws.next().await.unwrap().unwrap();
                ws.send(Message::Text(
                    structural_event("response.created", 1024)
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
                ws.close(None).await.unwrap();
            }
        });
        let provider = provider_with_base_url_and_retry_budget(&store, base_url, max_retries);
        for retry_count in 0..=max_retries {
            let transport = NonZeroU32::new(retry_count)
                .map_or(AttemptTransport::Default, AttemptTransport::Retry);
            let trace = TraceContext::new("req_precommit");
            let mut stream = Arc::clone(&provider)
                .execute(
                    planned_request("openai", generate_operation()),
                    traced_context(&trace, CancellationToken::new()).with_transport(transport),
                )
                .await
                .unwrap();
            let (events, error) = stream_failure(&mut stream).await;
            let delivered: Vec<_> = events
                .iter()
                .filter(|event| event.has_client_event())
                .collect();
            assert_eq!(delivered.len(), 1);
            assert!(delivered[0].is_replayable_prelude());
            assert_eq!(error.send_state(), UpstreamSendState::Ambiguous);
            assert!(!error.replay_is_safe());
            if retry_count < max_retries {
                assert!(
                    matches!(error.pre_delivery_retry(), Some(PreDeliveryRetry::SameAccountTransportRetry { retry_index, .. }) if retry_index.get() == retry_count + 1)
                );
            } else {
                assert_eq!(
                    error.pre_delivery_retry(),
                    Some(PreDeliveryRetry::SameAccountTransportFallback)
                );
            }
        }
        server.await.unwrap();
    }
}

#[tokio::test]
async fn sse_comments_are_forwarded_without_closing_the_retry_window() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let heartbeat = ": keep-alive\r\n\r\n";
    let (base_url, release, _, server) =
        paused_chunked_sse_server(heartbeat.to_owned(), sse(&overload())).await;
    let mut stream = provider_with_base_url(&store, base_url)
        .execute(
            planned_request("openai", http_generate_operation()),
            context("req_heartbeat_retry", CancellationToken::new()),
        )
        .await
        .unwrap();
    let event = timeout(Duration::from_secs(1), next_client_event(&mut stream))
        .await
        .expect("heartbeat cannot wait for a later event");
    assert_eq!(
        event.wire_event().unwrap().raw_sse_frame().unwrap(),
        heartbeat.as_bytes()
    );
    assert!(event.is_replayable_prelude());
    release.send(()).unwrap();
    let (_, error) = stream_failure(&mut stream).await;
    assert!(error.replay_is_safe());
    assert!(matches!(
        error.pre_delivery_retry(),
        Some(PreDeliveryRetry::SameAccountTransientRetry { .. })
    ));
    server.await.unwrap();
}

#[tokio::test]
async fn ordinary_streams_forward_compaction_output_and_unknown_events_without_reopening_retry() {
    for websocket in [false, true] {
        for first in [
            json!({"type":"response.compaction.compacting","item_id":"compact_test","output_index":0}),
            json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"partial"}),
            json!({"type":"response.future","value":"opaque"}),
        ] {
            let store = Arc::new(MemoryAccountStore::default());
            create_account(&store, "acct_provider_contract").await;
            let prelude = structural_event("response.created", 1024);
            let (base_url, release, server) = if websocket {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let base_url = format!("http://{}", listener.local_addr().unwrap());
                let (release, released) = oneshot::channel();
                let first = first.clone();
                let server = tokio::spawn(async move {
                    let (socket, _) = listener.accept().await.unwrap();
                    let mut socket = accept_codex_test_websocket(socket).await;
                    socket.next().await.unwrap().unwrap();
                    for event in [first, prelude] {
                        socket
                            .send(Message::Text(event.to_string().into()))
                            .await
                            .unwrap();
                    }
                    released.await.unwrap();
                    socket
                        .send(Message::Text(overload().to_string().into()))
                        .await
                        .unwrap();
                });
                (base_url, release, server)
            } else {
                let (base_url, release, _, server) =
                    paused_chunked_sse_server(sse(&first) + &sse(&prelude), sse(&overload())).await;
                (base_url, release, server)
            };
            let operation = if websocket {
                generate_operation()
            } else {
                http_generate_operation()
            };
            let mut stream = provider_with_base_url(&store, base_url)
                .execute(
                    planned_request("openai", operation),
                    context("req_closed_retry", CancellationToken::new()),
                )
                .await
                .unwrap();
            let delivered = timeout(Duration::from_secs(1), next_client_event(&mut stream))
                .await
                .unwrap();
            assert_eq!(delivered.wire_event().unwrap().data(), &first);
            assert!(!delivered.is_replayable_prelude());
            let created = timeout(Duration::from_secs(1), next_client_event(&mut stream))
                .await
                .unwrap();
            assert!(!created.is_replayable_prelude());
            release.send(()).unwrap();
            let (_, error) = stream_failure(&mut stream).await;
            assert!(!error.replay_is_safe());
            assert!(error.pre_delivery_retry().is_none());
            server.await.unwrap();
        }
    }
}
