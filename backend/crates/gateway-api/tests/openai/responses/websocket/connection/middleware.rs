//! 验证 WebSocket 各轮次刷新身份与设置时保留握手中间件覆盖

use super::*;
use bytes::Bytes;
use gateway_core::engine::middleware::FrozenMiddlewarePlan;
use gateway_core::engine::middleware::MiddlewareContext;
use gateway_core::engine::middleware::MiddlewareError;
use gateway_core::engine::middleware::MiddlewareNext;
use gateway_core::engine::middleware::MiddlewarePlan;
use gateway_core::engine::middleware::MiddlewareRequest;
use gateway_core::engine::middleware::MiddlewareResponse;
use gateway_core::engine::middleware::http as http_contract;
use gateway_core::engine::middleware::websocket as core;
use gateway_core::routing::extensions::ExtensionSetId;
use gateway_core::routing::extensions::ExtensionSetLease;
use gateway_core::routing::extensions::ExtensionSetReference;

#[tokio::test(start_paused = true)]
async fn client_close_does_not_flush_a_retained_sender_with_an_inflight_write() {
    #[derive(Default)]
    struct RetainedSenderPlan(Mutex<Option<Arc<dyn core::Sender>>>);
    impl fmt::Debug for RetainedSenderPlan {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("RetainedSenderPlan")
        }
    }
    impl MiddlewarePlan for RetainedSenderPlan {
        fn has_websocket(&self) -> bool {
            true
        }
        fn handle_websocket(
            &self,
            context: core::Context,
            message: core::Message,
            next: core::Next,
        ) -> BoxFuture<'static, Result<Option<core::Message>, MiddlewareError>> {
            *self.0.lock().unwrap() = Some(context.sender);
            next.run(message)
        }
        fn handle(
            &self,
            _: MiddlewareContext,
            request: MiddlewareRequest,
            next: MiddlewareNext,
        ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
            next.run(request)
        }
    }

    let plan = Arc::new(RetainedSenderPlan::default());
    let frozen = FrozenMiddlewarePlan::new(
        plan.clone(),
        ExtensionSetReference::new(
            ExtensionSetId::new("retained-writer".into()).unwrap(),
            Arc::new(Lease),
        ),
    );
    let (incoming, received) = unbounded_channel();
    let (written, mut output) = unbounded_channel();
    let flush_started = Arc::new(AtomicBool::new(false));
    let socket = TestSocket {
        incoming: received,
        written,
        stall_writes: true,
        flush_started: flush_started.clone(),
        dropped: Arc::new(AtomicBool::new(false)),
    };
    let mut connection = spawn_connection(
        socket,
        Arc::from("retained-writer"),
        CancellationToken::new(),
        ConnectionConfig::PRODUCTION,
        Some(frozen),
        Arc::from([]),
    );
    incoming.send(Ok(Message::Text("capture".into()))).unwrap();
    assert!(matches!(
        connection.next_event().await,
        Some(ConnectionEvent::Text(_))
    ));
    let sender = plan.0.lock().unwrap().clone().unwrap();
    let mut write = sender.send(core::Message {
        kind: core::Kind::Text,
        payload: Bytes::from_static(b"pending"),
    });
    assert!(futures::poll!(write.as_mut()).is_pending());
    // 写 future 在 pump 的 select 之外持锁，而且暂不继续 poll 以响应取消
    incoming.send(Ok(Message::Close(None))).unwrap();
    let reason = tokio::time::timeout(Duration::from_millis(50), connection.wait_for_exit())
        .await
        .expect("peer close must not wait for a retained sender");
    assert_eq!(reason, PumpExitReason::ClientClose);
    assert!(!flush_started.load(Ordering::Acquire));
    assert!(write.await.is_err());
    assert!(matches!(output.try_recv(), Err(TryRecvError::Empty)));
    plan.0.lock().unwrap().take();
}

#[derive(Debug, Default)]
struct Plan {
    block_terminal: bool,
    control_received: Arc<tokio::sync::Notify>,
    incoming: AtomicUsize,
    outgoing: AtomicUsize,
    http_cancellation: Mutex<Option<CancellationToken>>,
    session_cancellation: Mutex<Option<CancellationToken>>,
}
struct Lease;
impl ExtensionSetLease for Lease {
    fn is_ready(&self) -> bool {
        true
    }
}
impl MiddlewarePlan for Plan {
    fn has_http(&self) -> bool {
        true
    }
    fn has_websocket(&self) -> bool {
        true
    }
    fn handle_http(
        &self,
        context: http_contract::Context,
        mut request: http_contract::Request,
        next: http_contract::Next,
    ) -> BoxFuture<'static, Result<http_contract::Response, MiddlewareError>> {
        *self.http_cancellation.lock().unwrap() = Some(context.cancellation);
        assert_eq!(request.uri(), "/custom/websocket");
        *request.uri_mut() = "/v1/responses".parse().unwrap();
        next.run(request)
    }
    fn handle_websocket(
        &self,
        context: core::Context,
        message: core::Message,
        next: core::Next,
    ) -> BoxFuture<'static, Result<Option<core::Message>, MiddlewareError>> {
        assert!(
            context.plan.is_some(),
            "消息回调必须继承连接冻结的服务组合计划"
        );
        assert!(
            context
                .headers
                .iter()
                .any(|header| header.name() == "authorization"
                    && header.value().as_ref() == b"Bearer sk_ws_crossing_limit")
        );
        *self.session_cancellation.lock().unwrap() = Some(context.cancellation.clone());
        match context.direction {
            core::Direction::Incoming => self.incoming.fetch_add(1, Ordering::SeqCst),
            core::Direction::Outgoing => self.outgoing.fetch_add(1, Ordering::SeqCst),
        };
        let control_received = self.control_received.clone();
        let block_terminal = self.block_terminal;
        Box::pin(async move {
            if message.payload == "signal" {
                control_received.notify_one();
                return Ok(None);
            }
            if context.direction == core::Direction::Incoming && message.payload == "custom.control"
            {
                context
                    .sender
                    .send(core::Message {
                        kind: core::Kind::Binary,
                        payload: Bytes::from_static(&[0, 255, 1]),
                    })
                    .await?;
                return Ok(None);
            }
            let mut output = next.run(message).await?;
            if context.direction == core::Direction::Outgoing
                && let Some(message) = output.as_mut()
                && message.kind == core::Kind::Text
            {
                let mut value: Value = serde_json::from_slice(&message.payload).unwrap();
                if block_terminal && value["type"] == "response.completed" {
                    message.kind = core::Kind::Binary;
                    message.payload = Bytes::from(vec![1; 64 * 1024 * 1024]);
                    return Ok(output);
                }
                value["plugin"] = Value::Bool(true);
                message.payload = Bytes::from(serde_json::to_vec(&value).unwrap());
            }
            Ok(output)
        })
    }
    fn handle(
        &self,
        _: MiddlewareContext,
        request: MiddlewareRequest,
        next: MiddlewareNext,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        // 统一计划延续到连接内的模型调用，没有模型绑定时直接委托
        next.run(request)
    }
}

#[tokio::test]
async fn turns_refresh_host_settings_and_key_identity_while_preserving_handshake_overrides() {
    use gateway_core::{
        engine::execution::DefaultExecutionService, engine::provider::ProviderRegistry,
        runtime::RuntimeSnapshotHandle,
    };
    #[derive(Debug, Default)]
    struct SettingsPlan(Mutex<Vec<Value>>);
    impl MiddlewarePlan for SettingsPlan {
        fn has_http(&self) -> bool {
            true
        }
        fn handle_http(
            &self,
            _: http_contract::Context,
            mut request: http_contract::Request,
            next: http_contract::Next,
        ) -> BoxFuture<'static, Result<http_contract::Response, MiddlewareError>> {
            let settings = request
                .extensions_mut()
                .get_mut::<http_contract::Settings>()
                .unwrap();
            let baseline = settings.runtime.as_ref().unwrap();
            let mut values = serde_json::to_value(baseline.values()).unwrap();
            values["max_account_rotations"] = json!(0);
            settings.runtime = Some(
                baseline
                    .replace(serde_json::from_value(values).unwrap(), "handshake")
                    .unwrap(),
            );
            next.run(request)
        }
        fn handle(
            &self,
            _: MiddlewareContext,
            request: MiddlewareRequest,
            _: MiddlewareNext,
        ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
            self.0.lock().unwrap().push(
                serde_json::to_value(request.settings().unwrap().execution_values().unwrap())
                    .unwrap(),
            );
            Box::pin(async { Err(MiddlewareError::Rejected) })
        }
    }
    let key_snapshot = |enabled: Option<bool>| {
        let profiles = enabled.map_or_else(Default::default, |enabled| {
            std::collections::BTreeMap::from([(
                gateway_core::identity::ProviderKind::new("openai").unwrap(),
                gateway_core::account::OpaqueProviderData::new(
                    json!({
                        "mode": "custom",
                        "userAgent": "codex_cli_rs/0.160.1 (Linux 6.8.0; x86_64) unknown",
                        "perKeyInstallationIdEnabled": enabled,
                    })
                    .as_object()
                    .unwrap()
                    .clone(),
                ),
            )])
        });
        crate::openai::snapshot_with_key_profiles(
            "sk_ws_settings",
            "openai",
            "key_api_test",
            &["acct_api_test"],
            true,
            profiles,
        )
    };
    let initial = key_snapshot(Some(false));
    let snapshots = RuntimeSnapshotHandle::new(initial.clone());
    let execution = Arc::new(DefaultExecutionService::new(
        snapshots.clone(),
        Arc::new(crate::openai::UnusedExecutionStore),
        ProviderRegistry::default(),
        Arc::new(crate::openai::UnusedAdmissions),
        Arc::new(crate::openai::UnusedContinuation),
        Arc::new(crate::openai::IgnoredClientApiKeyUsage),
        Arc::new(crate::support::RecordingDiagnostics::default()),
    ));
    let admin = crate::admin::AdminTestFixture::new().await;
    let plan = Arc::new(SettingsPlan::default());
    let frozen = FrozenMiddlewarePlan::new(
        plan.clone(),
        ExtensionSetReference::new(
            ExtensionSetId::new("ws-settings".into()).unwrap(),
            Arc::new(Lease),
        ),
    );
    let router = gateway_api::initialize(
        gateway_api::ApiConfig {
            asset_directory: std::env::temp_dir(),
            cors_allowed_origins: vec![],
            request_timeout_seconds: None,
            request_id_header: "x-request-id".into(),
        },
        execution,
        admin.services,
        vec![],
        Arc::new(crate::openai::EmptyWorkerHealth),
        Arc::new(crate::openai::TestLifecycle::default()),
        Arc::new(crate::support::RecordingDiagnostics::default()),
    )
    .unwrap()
    .with_middleware(move |_| Some(frozen.clone()))
    .router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let mut request = format!("ws://{address}/v1/responses")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert(AUTHORIZATION, "Bearer sk_ws_settings".parse().unwrap());
    let (mut socket, _) = connect_async(request).await.unwrap();
    for round in 0..5 {
        if round == 1 {
            let mut settings = serde_json::to_value(initial.settings()).unwrap();
            settings["max_account_rotations"] = json!(5);
            settings["responses_max_decompressed_body_bytes"] = json!(2048);
            snapshots.publish(
                key_snapshot(Some(true))
                    .with_settings(&serde_json::from_value(settings).unwrap())
                    .unwrap(),
            );
        } else if round == 2 {
            snapshots.publish(key_snapshot(None));
        } else if round == 3 {
            snapshots.publish(key_snapshot(Some(false)));
        } else if round == 4 {
            snapshots.publish(crate::openai::snapshot("sk_replaced", "openai"));
        }
        socket
            .send(ClientMessage::Text(
                json!({"type":"response.create", "model":"model-a", "input":"hello"})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let ClientMessage::Text(text) = message else {
            panic!("expected error event")
        };
        let event: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(event["type"], "error");
        assert_eq!(
            plan.0.lock().unwrap().len(),
            (round + 1).min(4),
            "revoked Key must not reach model middleware"
        );
    }
    {
        let observed = plan.0.lock().unwrap();
        assert_eq!(
            observed[0]["runtime"]["responses_max_decompressed_body_bytes"],
            64 * 1024 * 1024
        );
        assert_eq!(
            observed[1]["runtime"]["responses_max_decompressed_body_bytes"],
            2048
        );
        assert_eq!(observed[0]["runtime"]["max_account_rotations"], 0);
        assert_eq!(observed[1]["runtime"]["max_account_rotations"], 0);
        for (round, enabled) in [(0, false), (1, true), (3, false)] {
            assert_eq!(
                observed[round]["runtime"]["request_profiles"]["openai"]["perKeyInstallationIdEnabled"],
                enabled,
                "each WebSocket execution must use the current key configuration"
            );
        }
        assert!(
            observed[2]["runtime"]["request_profiles"]
                .get("openai")
                .is_none()
        );
    }
    socket.close(None).await.unwrap();
    server.abort();
}

#[tokio::test]
async fn rewrite_upgrade_and_control_active_response_keep_http_context_until_connection_ends() {
    let plan = Arc::new(Plan::default());
    let frozen = FrozenMiddlewarePlan::new(
        plan.clone(),
        ExtensionSetReference::new(
            ExtensionSetId::new("ws-middleware-test".into()).unwrap(),
            Arc::new(Lease),
        ),
    );
    let (trace, mut socket, server) =
        start_active_response_with_middleware(Some(frozen), "/custom/websocket").await;
    // 升级后的默认会话仍持有入口上下文；101 正文结束不再提前取消它
    assert!(
        !plan
            .http_cancellation
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .is_cancelled()
    );
    assert!(
        !plan
            .session_cancellation
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .is_cancelled()
    );
    socket
        .send(ClientMessage::Text("custom.control".into()))
        .await
        .unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(reply, ClientMessage::Binary(bytes) if bytes.as_ref() == [0,255,1]));
    assert_eq!(trace.starts.load(Ordering::SeqCst), 1);
    assert!(!trace.cancelled.load(Ordering::SeqCst));
    trace.release_terminal.notify_one();
    let reply = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let ClientMessage::Text(text) = reply else {
        panic!("terminal text")
    };
    let event: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(event["type"], "response.completed");
    assert_eq!(event["plugin"], true);
    assert!(plan.incoming.load(Ordering::SeqCst) >= 2);
    assert!(plan.outgoing.load(Ordering::SeqCst) >= 3);
    socket.close(None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if plan
                .session_cancellation
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .is_cancelled()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let http_cancellation = plan.http_cancellation.lock().unwrap().clone().unwrap();
    tokio::time::timeout(Duration::from_secs(2), http_cancellation.cancelled())
        .await
        .unwrap();
    server.abort();
}

#[tokio::test]
async fn input_plugin_runs_while_a_real_socket_write_is_blocked() {
    let plan = Arc::new(Plan {
        block_terminal: true,
        ..Plan::default()
    });
    let frozen = FrozenMiddlewarePlan::new(
        plan.clone(),
        ExtensionSetReference::new(
            ExtensionSetId::new("ws-backpressure-test".into()).unwrap(),
            Arc::new(Lease),
        ),
    );
    let (trace, mut socket, server) =
        start_active_response_with_middleware(Some(frozen), "/custom/websocket").await;
    trace.release_terminal.notify_one();
    // 客户端不读取 64 MiB 输出，真实 TCP 写入将受接收窗口背压
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!trace.finalized.load(Ordering::SeqCst));
    socket
        .send(ClientMessage::Text("signal".into()))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), plan.control_received.notified())
        .await
        .expect("control middleware must run without waiting for the blocked output");
    assert!(!trace.finalized.load(Ordering::SeqCst));
    socket.close(None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if trace.finalized.load(Ordering::SeqCst) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("blocked response must finalize on disconnect");
    server.abort();
}
