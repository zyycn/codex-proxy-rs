//! 验证结构事件缓冲、交付宽限与取消对重试边界的影响

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
        SystemTime::now() + Duration::from_secs(30),
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
async fn large_structural_events_preserve_overload_replay_past_the_old_grace_on_http_and_websocket()
{
    for websocket in [false, true] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_provider_contract").await;
        // #259 仅保留了事件长度；用合成配置回显复现尺寸，不依赖现场私有正文
        let created = structural_event("response.created", 38_781);
        let progress = structural_event("response.in_progress", 38_785);
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
            timeout(Duration::from_millis(1_500), next_client_event(&mut stream))
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
async fn later_structural_events_do_not_extend_grace_and_late_overload_is_not_replayable() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let created = sse(&structural_event("response.created", 38_781));
    let progress = sse(&structural_event("response.in_progress", 38_785));
    let expected_bytes = created.len() + progress.len();
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
    let first = timeout(Duration::from_millis(3_500), next_client_event(&mut stream))
        .await
        .unwrap();
    assert_eq!(
        first.wire_event().unwrap().event_type(),
        Some("response.created")
    );
    release.send(()).unwrap();
    let (events, error) = timeout(Duration::from_secs(1), stream_failure(&mut stream))
        .await
        .unwrap();
    assert!(!error.replay_is_safe());
    assert!(error.pre_delivery_retry().is_none());
    assert!(events.iter().any(|event| {
        event
            .wire_event()
            .is_some_and(|wire| wire.data() == &overload())
    }));
    let releases = releases(&trace);
    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0]["attemptIndex"], 1);
    assert_eq!(releases[0]["data"]["reason"], "grace_timeout");
    assert_eq!(releases[0]["data"]["prefetchedBytes"], expected_bytes);
    assert!((2_500..3_500).contains(&releases[0]["data"]["waitMs"].as_u64().unwrap()));
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
            sse(&structural_event("response.created", 512)) + &sse(&semantic),
        ),
        (
            "semantic_output",
            sse(&structural_event("response.created", 512)) + &sse(&tool),
        ),
        (
            "terminal",
            sse(&structural_event("response.created", 512)) + &sse(&terminal),
        ),
        (
            "byte_limit",
            sse(&structural_event("response.created", 129 * 1024)),
        ),
        ("eof", sse(&structural_event("response.created", 512))),
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
        if reason == "byte_limit" {
            assert!(releases[0]["data"]["prefetchedBytes"].as_u64().unwrap() > 128 * 1024);
        } else {
            assert_eq!(releases[0]["data"]["prefetchedBytes"], body.len());
        }
        server.await.unwrap();
    }
}

#[tokio::test]
async fn cancelling_buffered_structural_events_does_not_release_or_offer_replay() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let (base_url, release, _, server) = paused_chunked_sse_server(
        sse(&structural_event("response.created", 38_781)),
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
