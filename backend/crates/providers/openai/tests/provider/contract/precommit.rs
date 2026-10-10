//! 验证结构事件缓冲、长时间等待与取消对重试边界的影响

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

fn releases(trace: &TraceContext) -> Vec<Value> {
    trace.snapshot().unwrap()["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["stage"] == "provider.precommit.released")
        .cloned()
        .collect()
}

async fn next_client_event(stream: &mut ProviderStream) -> ProviderEvent {
    loop {
        let event = stream.next().await.unwrap().unwrap();
        if event.has_client_event() {
            return event;
        }
    }
}

async fn stream_failure(stream: &mut ProviderStream) -> (Vec<ProviderEvent>, ProviderError) {
    let mut events = Vec::new();
    loop {
        match stream.next().await.expect("upstream failure") {
            Ok(event) => events.push(event),
            Err(error) => return (events, error),
        }
    }
}

#[tokio::test]
async fn large_structural_events_preserve_overload_replay_on_http_and_websocket() {
    for websocket in [false, true] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_provider_contract").await;
        // 大段配置回显不应提前结束重试窗口；使用合成正文覆盖单帧与累计的大体积前导事件
        let created = structural_event("response.created", 300 * 1024);
        let progress = structural_event("response.in_progress", 300 * 1024);
        let failure = overload();
        let expected = vec![created.clone(), progress.clone(), failure.clone()];
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

        assert!(
            timeout(Duration::from_millis(3_500), next_client_event(&mut stream))
                .await
                .is_err()
        );
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
        assert!(
            releases(&trace).is_empty(),
            "discardable failure must not be reported as a release"
        );
        server.await.unwrap();
    }
}

#[tokio::test]
async fn late_overload_after_multiple_structural_events_remains_replayable() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let created = sse(&structural_event("response.created", 96 * 1024));
    let progress = sse(&structural_event("response.in_progress", 96 * 1024));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let (release, released) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_http_request(&mut socket).await;
        socket.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n").await.unwrap();
        write_http_chunk(&mut socket, &created).await;
        tokio::time::sleep(Duration::from_millis(1_500)).await;
        write_http_chunk(&mut socket, &progress).await;
        released.await.unwrap();
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
    assert!(
        timeout(Duration::from_millis(3_500), next_client_event(&mut stream))
            .await
            .is_err()
    );
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
    assert_eq!(
        error
            .take_atomic_client_events()
            .iter()
            .filter_map(|event| event.wire_event()?.event_type())
            .collect::<Vec<_>>(),
        ["response.created", "response.in_progress", "error"]
    );
    assert!(releases(&trace).is_empty());
    server.await.unwrap();
}

#[tokio::test]
async fn immediate_release_preserves_wire_and_records_the_boundary_once() {
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
        let releases = releases(&trace);
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0]["data"]["reason"], reason);
        assert_eq!(releases[0]["data"]["prefetchedBytes"], body.len());
        server.await.unwrap();
    }
}

#[tokio::test]
async fn cancelling_buffered_structural_events_does_not_release_or_offer_replay() {
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
    assert!(
        timeout(Duration::from_millis(100), next_client_event(&mut stream))
            .await
            .is_err()
    );
    cancellation.cancel();
    let (events, error) = timeout(Duration::from_secs(1), stream_failure(&mut stream))
        .await
        .unwrap();
    assert_eq!(error.kind(), ProviderErrorKind::Cancelled);
    assert!(!error.replay_is_safe());
    assert!(events.iter().all(|event| !event.has_client_event()));
    assert!(releases(&trace).is_empty());
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
    assert!(
        timeout(Duration::from_millis(100), next_client_event(&mut stream))
            .await
            .is_err()
    );
    // 结构进度保持上游传输活跃，长思考不能额外触发首内容等待期限
    for _ in 0..20 {
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(60)).await;
        tokio::time::resume();
        progress.send(()).await.unwrap();
        assert!(
            timeout(Duration::from_millis(50), next_client_event(&mut stream))
                .await
                .is_err()
        );
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
    assert!(releases(&trace).is_empty());
    server.await.unwrap();
}

#[tokio::test]
async fn structural_preamble_over_16_mib_preserves_late_overload_recovery() {
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
    assert!(
        timeout(Duration::from_secs(1), next_client_event(&mut stream))
            .await
            .is_err()
    );
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
    assert!(buffered_bytes > 16 * 1024 * 1024);
    assert!(releases(&trace).is_empty());
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
            assert!(events.iter().all(|event| !event.has_client_event()));
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
            assert!(releases(&trace).is_empty());
        }
        server.await.unwrap();
    }
}
