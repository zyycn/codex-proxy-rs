//! 验证真实插件 RPC 会话的启动重试、并发调用、关闭与资源回收

use std::{
    sync::{
        Arc, Condvar, Mutex, Weak,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures::future::BoxFuture;
use gateway_plugin_runtime::{
    CallbackHandler, PackageLimits, RpcError, RpcLimits, RpcReply, RpcSession, ValidatedPackage,
};
use gateway_plugin_sdk::{CallContext, ErrorCode, Handshake, PluginFault, Stage};
use serde_json::json;
use tokio::sync::{Barrier, Notify};

#[derive(Default)]
struct Callbacks {
    called: AtomicUsize,
    nested: Mutex<Option<Weak<RpcSession>>>,
}

#[derive(Default)]
struct BlockingCallbacks {
    started: Arc<Notify>,
    dropped: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Default)]
struct LifecycleCallbacks {
    begun: AtomicUsize,
    finished: AtomicUsize,
}

struct OversizedReplyCallbacks;

impl CallbackHandler for OversizedReplyCallbacks {
    fn call(
        &self,
        _context: CallContext,
        _method: String,
        params: serde_json::Value,
        _payload: Vec<u8>,
    ) -> BoxFuture<'static, Result<RpcReply, PluginFault>> {
        Box::pin(async move {
            if params["error"] == true {
                return Err(PluginFault::new(ErrorCode::Fault, "x".repeat(64 * 1024)));
            }
            Ok(RpcReply {
                result: json!({"text": "x".repeat(64 * 1024)}),
                payload: Vec::new(),
            })
        })
    }
}

struct CallbackDrop(Arc<std::sync::atomic::AtomicBool>);

#[derive(Default)]
struct GatedFirstBegin {
    entered: Notify,
    released: Mutex<bool>,
    release: Condvar,
}

impl GatedFirstBegin {
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.release.notify_one();
    }
}

impl Drop for CallbackDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

impl CallbackHandler for BlockingCallbacks {
    fn call(
        &self,
        _context: CallContext,
        _method: String,
        _params: serde_json::Value,
        _payload: Vec<u8>,
    ) -> BoxFuture<'static, Result<RpcReply, PluginFault>> {
        let started = Arc::clone(&self.started);
        let dropped = Arc::clone(&self.dropped);
        Box::pin(async move {
            let _drop = CallbackDrop(dropped);
            started.notify_one();
            futures::future::pending().await
        })
    }
}

impl CallbackHandler for LifecycleCallbacks {
    fn begin(&self, _context: &CallContext) {
        self.begun.fetch_add(1, Ordering::Relaxed);
    }

    fn finish(&self, _context: &CallContext) {
        self.finished.fetch_add(1, Ordering::Relaxed);
    }

    fn call(
        &self,
        _context: CallContext,
        _method: String,
        params: serde_json::Value,
        payload: Vec<u8>,
    ) -> BoxFuture<'static, Result<RpcReply, PluginFault>> {
        Box::pin(async move {
            Ok(RpcReply {
                result: params,
                payload,
            })
        })
    }
}

impl CallbackHandler for GatedFirstBegin {
    fn begin(&self, context: &CallContext) {
        if context.call_id == 1 {
            self.entered.notify_one();
            let released = self.released.lock().unwrap();
            drop(
                self.release
                    .wait_while(released, |released| !*released)
                    .unwrap(),
            );
        }
    }

    fn call(
        &self,
        _context: CallContext,
        _method: String,
        params: serde_json::Value,
        payload: Vec<u8>,
    ) -> BoxFuture<'static, Result<RpcReply, PluginFault>> {
        Box::pin(async move {
            Ok(RpcReply {
                result: params,
                payload,
            })
        })
    }
}

impl CallbackHandler for Callbacks {
    fn call(
        &self,
        context: CallContext,
        _method: String,
        params: serde_json::Value,
        payload: Vec<u8>,
    ) -> BoxFuture<'static, Result<RpcReply, PluginFault>> {
        self.called.fetch_add(1, Ordering::Relaxed);
        let nested = self.nested.lock().unwrap().as_ref().and_then(Weak::upgrade);
        Box::pin(async move {
            if let Some(nested) = nested {
                nested
                    .call("echo", context, params, payload)
                    .await
                    .map_err(|_| PluginFault::new(ErrorCode::Fault, "nested call failed"))
            } else {
                Ok(RpcReply {
                    result: params,
                    payload,
                })
            }
        })
    }
}

async fn session<C>(callbacks: Arc<C>) -> (tempfile::TempDir, Arc<RpcSession>)
where
    C: CallbackHandler + 'static,
{
    let (cache, prepared, handshake, processes) = prepared_session();
    let session = RpcSession::start(
        prepared,
        handshake,
        RpcLimits::default(),
        &processes,
        callbacks,
    )
    .await
    .unwrap();
    (cache, Arc::new(session))
}

fn prepared_session() -> (
    tempfile::TempDir,
    Arc<gateway_plugin_runtime::PreparedPackage>,
    Handshake,
    gateway_host::process::ProcessSupervisor,
) {
    let cache = tempfile::tempdir().unwrap();
    let package = Arc::new(
        ValidatedPackage::read(
            crate::support::package(crate::support::worker()),
            None,
            PackageLimits::default(),
        )
        .unwrap(),
    );
    let prepared = Arc::new(
        package
            .prepare(cache.path(), &"1.0.0".parse().unwrap())
            .unwrap(),
    );
    let handshake = Handshake {
        protocol_version: gateway_plugin_sdk::PROTOCOL_VERSION,
        artifact_sha256: package.digest().into(),
        plugin_id: package.manifest().plugin_id().unwrap(),
        instance_id: "test-instance".into(),
        generation: 1,
        incarnation: uuid::Uuid::new_v4().to_string(),
        configuration: json!({}),
        contributes: gateway_plugin_sdk::Contributions::new(),
    };
    let processes =
        gateway_host::process::ProcessSupervisor::new(std::num::NonZeroUsize::new(128).unwrap());
    (cache, prepared, handshake, processes)
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn newly_unpacked_busy_executable_recovers_within_startup_deadline() {
    let (_cache, prepared, handshake, processes) = prepared_session();
    let writer = std::fs::OpenOptions::new()
        .write(true)
        .open(prepared.executable())
        .unwrap();
    let closing = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(writer);
    });
    let session = RpcSession::start(
        prepared,
        handshake,
        RpcLimits::default(),
        &processes,
        Arc::new(Callbacks::default()),
    )
    .await
    .unwrap();
    closing.await.unwrap();
    session.shutdown(Duration::from_secs(1)).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn persistently_busy_executable_stops_retrying_and_releases_capacity() {
    let (_cache, prepared, handshake, _) = prepared_session();
    let processes =
        gateway_host::process::ProcessSupervisor::new(std::num::NonZeroUsize::new(1).unwrap());
    let writer = std::fs::OpenOptions::new()
        .write(true)
        .open(prepared.executable())
        .unwrap();
    let limits = RpcLimits {
        handshake_timeout: Duration::from_millis(100),
        ..RpcLimits::default()
    };
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        RpcSession::start(
            Arc::clone(&prepared),
            handshake.clone(),
            limits,
            &processes,
            Arc::new(Callbacks::default()),
        ),
    )
    .await
    .expect("bounded startup");
    assert!(matches!(
        result,
        Err(RpcError::Start(
            gateway_host::process::ProcessStartError::Spawn {
                kind: std::io::ErrorKind::ExecutableFileBusy,
                ..
            }
        ))
    ));
    drop(writer);
    let session = RpcSession::start(
        prepared,
        handshake,
        RpcLimits::default(),
        &processes,
        Arc::new(Callbacks::default()),
    )
    .await
    .unwrap();
    session.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn shutdown_returns_after_in_flight_callback_future_is_dropped() {
    let callbacks = Arc::new(BlockingCallbacks::default());
    let (_cache, session) = session(Arc::clone(&callbacks)).await;
    let callback_started = callbacks.started.notified();
    let calling = {
        let session = Arc::clone(&session);
        tokio::spawn(async move {
            session
                .call(
                    "callback",
                    session.context(Stage::Management, Duration::from_secs(30)),
                    json!({}),
                    vec![],
                )
                .await
        })
    };
    callback_started.await;

    session.quiesce();
    tokio::time::timeout(
        Duration::from_secs(2),
        session.shutdown(Duration::from_secs(1)),
    )
    .await
    .expect("session shutdown");
    assert!(callbacks.dropped.load(Ordering::Acquire));
    assert!(matches!(calling.await.unwrap(), Err(RpcError::Closed)));
}

#[tokio::test]
async fn real_process_round_trips_raw_bytes_and_reentrant_callbacks() {
    let callbacks = Arc::new(Callbacks::default());
    let (_cache, session) = session(Arc::clone(&callbacks)).await;
    assert_eq!(
        session
            .context(Stage::Registration, Duration::MAX)
            .timeout_ms,
        u64::try_from(RpcLimits::default().maximum_call_timeout.as_millis()).unwrap()
    );
    *callbacks.nested.lock().unwrap() = Some(Arc::downgrade(&session));
    let context = session.context(Stage::Management, Duration::from_secs(2));
    let reply = session
        .call(
            "callback",
            context,
            json!({"hello": "世界"}),
            vec![0, 255, 128],
        )
        .await
        .unwrap();
    assert_eq!(reply.result, json!({"hello": "世界"}));
    assert_eq!(reply.payload, [0, 255, 128]);
    assert_eq!(callbacks.called.load(Ordering::Relaxed), 1);
    session.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn slow_management_call_does_not_block_an_independent_call() {
    let (_cache, session) = session(Arc::new(Callbacks::default())).await;
    let context = session.context(Stage::Management, Duration::from_secs(2));
    let slow_session = Arc::clone(&session);
    let slow =
        tokio::spawn(async move { slow_session.call("slow", context, json!({}), vec![]).await });
    tokio::time::sleep(Duration::from_millis(40)).await;
    let fast = session.call(
        "echo",
        session.context(Stage::Request, Duration::from_millis(250)),
        json!({}),
        vec![],
    );
    assert!(fast.await.is_ok());
    assert!(!slow.is_finished());
    assert!(slow.await.unwrap().is_ok());
    session.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn large_binary_payload_round_trips_through_calls_and_callbacks() {
    let callbacks = Arc::new(Callbacks::default());
    let (_cache, session) = session(Arc::clone(&callbacks)).await;
    let payload = (0..4 * 1024 * 1024 + 17)
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    for method in ["echo", "callback"] {
        let reply = session
            .call(
                method,
                session.context(Stage::Request, Duration::from_secs(30)),
                json!({"large": true}),
                payload.clone(),
            )
            .await
            .unwrap();
        assert_eq!(reply.result, json!({"large": true}));
        assert_eq!(reply.payload, payload);
    }
    assert_eq!(callbacks.called.load(Ordering::Relaxed), 1);
    session.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn oversized_call_frames_do_not_close_the_session_or_begin_callbacks() {
    let callbacks = Arc::new(LifecycleCallbacks::default());
    let (_cache, session) = session(Arc::clone(&callbacks)).await;
    let cases = [(json!({"text": "x".repeat(64 * 1024)}), vec![])];
    for (params, payload) in cases {
        let begun = callbacks.begun.load(Ordering::Relaxed);
        let error = session
            .call(
                "echo",
                session.context(Stage::Request, Duration::from_secs(2)),
                params.clone(),
                payload.clone(),
            )
            .await
            .err()
            .expect("oversized call must fail");
        assert_eq!(error, RpcError::Context);
        assert!(matches!(
            session
                .call_stream(
                    "stream",
                    session.context(Stage::Request, Duration::from_secs(2)),
                    params,
                    payload,
                )
                .await,
            Err(RpcError::Context)
        ));
        assert_eq!(callbacks.begun.load(Ordering::Relaxed), begun);
        assert!(session.is_ready());
        let reply = session
            .call(
                "echo",
                session.context(Stage::Request, Duration::from_secs(2)),
                json!({"still": "ready"}),
                vec![1, 2, 3],
            )
            .await
            .unwrap();
        assert_eq!(reply.result, json!({"still": "ready"}));
        assert_eq!(reply.payload, [1, 2, 3]);
    }
    session.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn oversized_callback_metadata_only_fails_its_parent_call() {
    let (_cache, session) = session(Arc::new(OversizedReplyCallbacks)).await;
    for error in [false, true] {
        assert!(matches!(
            session
                .call(
                    "callback",
                    session.context(Stage::Request, Duration::from_secs(2)),
                    json!({"error": error}),
                    vec![],
                )
                .await,
            Err(RpcError::Remote(_))
        ));
        assert!(session.is_ready());
        let reply = session
            .call(
                "echo",
                session.context(Stage::Request, Duration::from_secs(2)),
                json!({"still": "ready"}),
                vec![1, 2, 3],
            )
            .await
            .unwrap();
        assert_eq!(reply.payload, [1, 2, 3]);
    }
    session.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_calls_keep_their_wire_ids_monotonic() {
    let callbacks = Arc::new(GatedFirstBegin::default());
    let entered = callbacks.entered.notified();
    let (_cache, session) = session(Arc::clone(&callbacks)).await;
    let ready = Arc::new(Barrier::new(17));
    let mut calls = Vec::with_capacity(16);
    for index in 0..16 {
        let session = Arc::clone(&session);
        let ready = Arc::clone(&ready);
        calls.push(tokio::spawn(async move {
            ready.wait().await;
            if index % 2 == 0 {
                session
                    .call(
                        "echo",
                        session.context(Stage::Request, Duration::from_secs(5)),
                        json!({"index": index}),
                        vec![],
                    )
                    .await?;
            } else {
                let mut stream = session
                    .call_stream(
                        "stream",
                        session.context(Stage::Request, Duration::from_secs(5)),
                        json!({"index": index}),
                        vec![],
                    )
                    .await?;
                while stream.next().await?.is_some() {}
            }
            Ok::<(), RpcError>(())
        }));
    }
    ready.wait().await;
    entered.await;
    // 旧实现会在 begin 前分配后续 ID；等其他线程进入该窗口后再释放首调用
    tokio::time::sleep(Duration::from_millis(20)).await;
    callbacks.release();
    for call in calls {
        call.await.unwrap().unwrap();
    }
    session.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_a_call_waiting_for_send_order_does_not_block_following_calls() {
    let callbacks = Arc::new(GatedFirstBegin::default());
    let entered = callbacks.entered.notified();
    let (_cache, session) = session(Arc::clone(&callbacks)).await;
    let first = {
        let session = Arc::clone(&session);
        tokio::spawn(async move {
            session
                .call(
                    "echo",
                    session.context(Stage::Request, Duration::from_secs(2)),
                    json!("first"),
                    vec![],
                )
                .await
        })
    };
    entered.await;

    let queued = {
        let session = Arc::clone(&session);
        tokio::spawn(async move {
            session
                .call(
                    "echo",
                    session.context(Stage::Request, Duration::from_secs(2)),
                    json!("cancelled"),
                    vec![],
                )
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    queued.abort();
    assert!(matches!(queued.await, Err(error) if error.is_cancelled()));

    let following = {
        let session = Arc::clone(&session);
        tokio::spawn(async move {
            session
                .call(
                    "echo",
                    session.context(Stage::Request, Duration::from_secs(2)),
                    json!("following"),
                    vec![],
                )
                .await
        })
    };
    callbacks.release();
    assert_eq!(first.await.unwrap().unwrap().result, json!("first"));
    assert_eq!(following.await.unwrap().unwrap().result, json!("following"));
    assert!(session.is_ready());
    session.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn waiting_for_send_order_consumes_the_original_call_deadline() {
    let callbacks = Arc::new(GatedFirstBegin::default());
    let entered = callbacks.entered.notified();
    let (_cache, session) = session(Arc::clone(&callbacks)).await;
    let first = {
        let session = Arc::clone(&session);
        tokio::spawn(async move {
            session
                .call(
                    "echo",
                    session.context(Stage::Request, Duration::from_secs(2)),
                    json!("first"),
                    vec![],
                )
                .await
        })
    };
    entered.await;

    let waiting = session
        .call(
            "echo",
            session.context(Stage::Request, Duration::from_millis(30)),
            json!("expired"),
            vec![],
        )
        .await;
    assert!(matches!(waiting, Err(RpcError::Timeout)));

    callbacks.release();
    assert!(first.await.unwrap().is_ok());
    assert!(
        session
            .call(
                "echo",
                session.context(Stage::Request, Duration::from_secs(1)),
                json!("following"),
                vec![],
            )
            .await
            .is_ok()
    );
    session.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn callback_reaches_host_without_permission_declarations() {
    let callbacks = Arc::new(Callbacks::default());
    let (_cache, session) = session(Arc::clone(&callbacks)).await;
    let reply = session
        .call(
            "callback_method",
            session.context(Stage::Management, Duration::from_secs(2)),
            json!({"method":"host.http.do"}),
            vec![],
        )
        .await;
    assert!(reply.is_ok());
    assert_eq!(callbacks.called.load(Ordering::Relaxed), 1);
    session.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn unknown_parent_cannot_borrow_another_calls_resources() {
    let callbacks = Arc::new(Callbacks::default());
    let (_cache, session) = session(Arc::clone(&callbacks)).await;
    let reply = session
        .call(
            "forged_callback",
            session.context(Stage::Management, Duration::from_secs(2)),
            json!({}),
            vec![],
        )
        .await;
    assert!(matches!(reply, Err(RpcError::Protocol)));
    assert_eq!(callbacks.called.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn explicit_policy_denial_keeps_its_error_kind_and_status() {
    let (_cache, session) = session(Arc::new(Callbacks::default())).await;
    let reply = session
        .call(
            "deny",
            session.context(Stage::Request, Duration::from_secs(2)),
            json!({}),
            vec![],
        )
        .await;
    assert!(matches!(
        reply,
        Err(RpcError::Remote(PluginFault {
            code: ErrorCode::Rejected,
            http_status: Some(403),
            ..
        }))
    ));
    session.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn ignored_cancellation_closes_the_incarnation_and_settles_other_calls() {
    let (_cache, session) = session(Arc::new(Callbacks::default())).await;
    let first = session.call(
        "hang_uncancellable",
        session.context(Stage::Request, Duration::from_millis(100)),
        json!({}),
        vec![],
    );
    let second = session.call(
        "hang",
        session.context(Stage::Request, Duration::from_secs(2)),
        json!({}),
        vec![],
    );
    let (first, second) = tokio::join!(first, second);
    assert!(matches!(first, Err(RpcError::Timeout)));
    assert!(matches!(second, Err(RpcError::Timeout)));
    session.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn child_crash_fails_the_call_without_waiting_for_its_deadline() {
    let (_cache, session) = session(Arc::new(Callbacks::default())).await;
    let call = session.call(
        "crash",
        session.context(Stage::Request, Duration::from_secs(30)),
        json!({}),
        vec![],
    );
    let result = tokio::time::timeout(Duration::from_secs(2), call)
        .await
        .unwrap();
    assert!(matches!(result, Err(RpcError::Closed)));
}

#[tokio::test]
async fn malformed_worker_frames_fail_boundedly_and_release_call_resources() {
    for method in ["malformed_truncated_frame", "malformed_frame_length"] {
        let callbacks = Arc::new(LifecycleCallbacks::default());
        let (_cache, session) = session(Arc::clone(&callbacks)).await;
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            session.call(
                method,
                session.context(Stage::Request, Duration::from_secs(30)),
                json!({}),
                vec![],
            ),
        )
        .await
        .expect("malformed worker output must fail within the process boundary");
        assert!(matches!(result, Err(RpcError::Closed)));
        assert!(!session.is_ready());
        assert_eq!(callbacks.begun.load(Ordering::Relaxed), 1);
        assert_eq!(callbacks.finished.load(Ordering::Relaxed), 1);
        session.shutdown(Duration::from_secs(1)).await;
    }
}

#[tokio::test]
async fn slow_stream_consumer_backpressures_the_child_without_blocking_control_calls() {
    let (_cache, session) = session(Arc::new(Callbacks::default())).await;
    let mut stream = session
        .call_stream(
            "stream",
            session.context(Stage::Request, Duration::from_secs(5)),
            json!({}),
            vec![],
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    // 消费者停止读取时对端会耗尽窗口；另一个请求仍能通过独立读写循环完成
    let echo = session
        .call(
            "echo",
            session.context(Stage::Management, Duration::from_millis(500)),
            json!("ready"),
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(echo.result, json!("ready"));
    let mut count = 0;
    while let Some(chunk) = stream.next().await.unwrap() {
        assert_eq!(chunk, vec![count as u8; 8192]);
        count += 1;
    }
    assert_eq!(count, 128);
    assert!(stream.next().await.unwrap().is_none());
    session.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn stream_window_and_sequence_violations_fail_closed() {
    for method in ["stream_overflow", "stream_sequence"] {
        let (_cache, session) = session(Arc::new(Callbacks::default())).await;
        let mut stream = session
            .call_stream(
                method,
                session.context(Stage::Request, Duration::from_secs(2)),
                json!({}),
                vec![],
            )
            .await
            .unwrap();
        assert!(matches!(stream.next().await, Err(RpcError::Protocol)));
        session.shutdown(Duration::from_secs(1)).await;
    }
}

#[tokio::test]
async fn dropping_a_backpressured_stream_cancels_only_that_call() {
    let (_cache, session) = session(Arc::new(Callbacks::default())).await;
    let stream = session
        .call_stream(
            "stream",
            session.context(Stage::Request, Duration::from_secs(5)),
            json!({}),
            vec![],
        )
        .await
        .unwrap();
    drop(stream);
    let reply = session
        .call(
            "echo",
            session.context(Stage::Request, Duration::from_secs(1)),
            json!({}),
            vec![],
        )
        .await;
    assert!(reply.is_ok());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(session.is_ready());
    session.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn acknowledged_timeout_does_not_interrupt_another_call() {
    let (_cache, session) = session(Arc::new(Callbacks::default())).await;
    let first = session.call(
        "hang",
        session.context(Stage::Management, Duration::from_millis(100)),
        json!({}),
        vec![],
    );
    let second = session.call(
        "slow",
        session.context(Stage::Management, Duration::from_secs(2)),
        json!("survived"),
        vec![],
    );
    let (first, second) = tokio::join!(first, second);
    assert!(matches!(first, Err(RpcError::Timeout)));
    assert_eq!(second.unwrap().result, json!("survived"));
    assert!(session.is_ready());
    session.shutdown(Duration::from_secs(1)).await;
}
