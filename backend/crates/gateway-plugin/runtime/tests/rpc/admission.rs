//! 验证执行排队、next 恢复、流式在途预算与取消回收

use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use super::session::{Callbacks, LifecycleCallbacks, prepared_session};
use futures::future::BoxFuture;
use gateway_plugin_runtime::{CallbackHandler, RpcError, RpcLimits, RpcReply, RpcSession};
use gateway_plugin_sdk::{CallContext, PluginFault, Stage, call::middleware::NEXT_METHOD};
use serde_json::json;
use tokio::sync::{Semaphore, mpsc};

async fn limited<C: CallbackHandler + 'static>(
    callbacks: Arc<C>,
    limits: RpcLimits,
) -> (tempfile::TempDir, Arc<RpcSession>) {
    let (cache, prepared, handshake, processes) = prepared_session();
    let session = RpcSession::start(prepared, handshake, limits, &processes, callbacks)
        .await
        .unwrap();
    (cache, Arc::new(session))
}

fn limits() -> RpcLimits {
    RpcLimits {
        maximum_calls: 4,
        maximum_executing_calls: 1,
        maximum_queued_calls: 1,
        maximum_queue_timeout: Duration::from_millis(200),
        maximum_callbacks: 4,
        ..Default::default()
    }
}

async fn wait_begun(callbacks: &LifecycleCallbacks, count: usize) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while callbacks.begun.load(Ordering::Acquire) < count {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

fn call(session: &RpcSession, method: &str) -> impl Future<Output = Result<RpcReply, RpcError>> {
    session.call(
        method,
        session.context(Stage::Request, Duration::from_secs(2)),
        json!({}),
        Vec::new(),
    )
}

#[tokio::test]
async fn bounded_initial_queue_waits_and_releases_cancelled_or_expired_waiters() {
    let callbacks = Arc::new(LifecycleCallbacks::default());
    let (_cache, session) = limited(callbacks.clone(), limits()).await;
    let active = tokio::spawn({
        let session = session.clone();
        async move { call(&session, "hang").await }
    });
    wait_begun(&callbacks, 1).await;

    let mut queued = Box::pin(call(&session, "echo"));
    assert!(futures::poll!(&mut queued).is_pending());
    assert!(matches!(
        call(&session, "echo").await,
        Err(RpcError::Overloaded)
    ));
    assert_eq!(callbacks.begun.load(Ordering::Acquire), 1);
    drop(queued);

    let expired = session
        .call(
            "echo",
            session.context(Stage::Request, Duration::from_millis(30)),
            json!({}),
            Vec::new(),
        )
        .await;
    assert!(matches!(expired, Err(RpcError::Overloaded)));
    assert_eq!(callbacks.begun.load(Ordering::Acquire), 1);

    let mut queued = Box::pin(call(&session, "echo"));
    assert!(futures::poll!(&mut queued).is_pending());
    active.abort();
    assert!(matches!(active.await, Err(error) if error.is_cancelled()));
    assert!(queued.await.is_ok());
    assert_eq!(callbacks.begun.load(Ordering::Acquire), 2);
    assert!(session.is_ready());
    session.shutdown(Duration::from_secs(1)).await;
}

struct NextCallbacks {
    entered: mpsc::UnboundedSender<()>,
    release: Arc<Semaphore>,
}

impl CallbackHandler for NextCallbacks {
    fn call(
        &self,
        _context: CallContext,
        method: String,
        params: serde_json::Value,
        payload: Vec<u8>,
    ) -> BoxFuture<'static, Result<RpcReply, PluginFault>> {
        let release = self.release.clone();
        self.entered.send(()).unwrap();
        Box::pin(async move {
            if method == NEXT_METHOD {
                release.acquire().await.unwrap().forget();
            }
            Ok(RpcReply {
                result: params,
                payload,
            })
        })
    }
}

#[tokio::test]
async fn next_resumption_does_not_need_a_vacant_initial_queue_slot() {
    let (entered, mut started) = mpsc::unbounded_channel();
    let release = Arc::new(Semaphore::new(0));
    let (_cache, session) = limited(
        Arc::new(NextCallbacks {
            entered,
            release: release.clone(),
        }),
        limits(),
    )
    .await;
    let waiting = tokio::spawn({
        let session = session.clone();
        async move {
            session
                .call(
                    "callback_method",
                    session.context(Stage::Request, Duration::from_secs(2)),
                    json!({"method": NEXT_METHOD}),
                    Vec::new(),
                )
                .await
        }
    });
    started.recv().await.unwrap();
    // 未消费的 next 与普通回调共享有界驻留额度
    let mut occupying = Box::pin(call(&session, "callback"));
    assert!(futures::poll!(&mut occupying).is_pending());
    started.recv().await.unwrap();
    occupying.await.unwrap();
    let mut active = Box::pin(call(&session, "hang"));
    assert!(futures::poll!(&mut active).is_pending());
    let mut queued = Box::pin(call(&session, "echo"));
    assert!(futures::poll!(&mut queued).is_pending());
    assert!(matches!(
        call(&session, "echo").await,
        Err(RpcError::Overloaded)
    ));
    release.add_permits(1);
    // 恢复等待不能立即因初次队列已满而拒绝，也不能绕过执行名额完成
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!waiting.is_finished());
    drop(active);
    assert!(queued.await.is_ok());
    assert!(waiting.await.unwrap().is_ok());
    assert!(session.is_ready());
    session.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn response_stream_releases_execution_but_retains_total_capacity_until_drop() {
    let callbacks = Arc::new(Callbacks::default());
    let (_cache, session) = limited(
        callbacks,
        RpcLimits {
            maximum_calls: 2,
            ..limits()
        },
    )
    .await;
    let mut streams = Vec::new();
    for _ in 0..2 {
        streams.push(
            session
                .call_stream(
                    "stream",
                    session.context(Stage::Request, Duration::from_secs(5)),
                    json!({}),
                    Vec::new(),
                )
                .await
                .unwrap(),
        );
    }
    assert!(matches!(
        call(&session, "echo").await,
        Err(RpcError::Overloaded)
    ));
    drop(streams.pop());
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match call(&session, "echo").await {
                Ok(_) => break,
                Err(RpcError::Overloaded) => tokio::task::yield_now().await,
                Err(error) => panic!("unexpected error: {error}"),
            }
        }
    })
    .await
    .unwrap();
    assert!(session.is_ready());
    drop(streams);
    session.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn queued_resumption_is_bounded_and_cancellable_without_failing_session() {
    for cancelled in [false, true] {
        let (entered, mut started) = mpsc::unbounded_channel();
        let release = Arc::new(Semaphore::new(0));
        let (_cache, session) = limited(
            Arc::new(NextCallbacks {
                entered,
                release: release.clone(),
            }),
            limits(),
        )
        .await;
        let waiting = tokio::spawn({
            let session = session.clone();
            async move {
                session
                    .call(
                        "callback_method",
                        session.context(Stage::Request, Duration::from_millis(200)),
                        json!({"method": NEXT_METHOD}),
                        Vec::new(),
                    )
                    .await
            }
        });
        started.recv().await.unwrap();
        let mut active = Box::pin(call(&session, "hang"));
        assert!(futures::poll!(&mut active).is_pending());
        release.add_permits(1);
        if cancelled {
            tokio::time::sleep(Duration::from_millis(30)).await;
            assert!(!waiting.is_finished());
            waiting.abort();
            assert!(matches!(waiting.await, Err(error) if error.is_cancelled()));
        } else {
            assert!(matches!(waiting.await.unwrap(), Err(RpcError::Overloaded)));
        }
        drop(active);
        assert!(call(&session, "echo").await.is_ok());
        assert!(session.is_ready());
        session.shutdown(Duration::from_secs(1)).await;
    }
}

#[tokio::test]
async fn initial_queue_wait_does_not_consume_the_plugin_processing_budget() {
    let (_cache, session) = limited(
        Arc::new(Callbacks::default()),
        RpcLimits {
            maximum_queue_timeout: Duration::from_secs(2),
            ..limits()
        },
    )
    .await;
    let mut active = Box::pin(call(&session, "hang"));
    assert!(futures::poll!(&mut active).is_pending());
    let mut queued = Box::pin(session.call(
        "echo",
        session.context(Stage::Request, Duration::from_millis(200)),
        json!({}),
        Vec::new(),
    ));
    assert!(futures::poll!(&mut queued).is_pending());
    tokio::time::sleep(Duration::from_millis(350)).await;
    drop(active);
    assert!(queued.await.is_ok());
    assert!(session.is_ready());
    session.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn resumption_queue_wait_preserves_the_remaining_plugin_processing_budget() {
    let (entered, mut started) = mpsc::unbounded_channel();
    let release = Arc::new(Semaphore::new(0));
    let (_cache, session) = limited(
        Arc::new(NextCallbacks {
            entered,
            release: release.clone(),
        }),
        RpcLimits {
            maximum_queue_timeout: Duration::from_secs(2),
            ..limits()
        },
    )
    .await;
    let waiting = tokio::spawn({
        let session = session.clone();
        async move {
            session
                .call(
                    "callback_method",
                    session.context(Stage::Request, Duration::from_millis(200)),
                    json!({"method": NEXT_METHOD}),
                    Vec::new(),
                )
                .await
        }
    });
    started.recv().await.unwrap();
    let mut active = Box::pin(call(&session, "hang"));
    assert!(futures::poll!(&mut active).is_pending());
    release.add_permits(1);
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert!(!waiting.is_finished());
    drop(active);
    assert!(waiting.await.unwrap().is_ok());
    assert!(session.is_ready());
    session.shutdown(Duration::from_secs(1)).await;
}

async fn scheduled<C: CallbackHandler + 'static>(
    callbacks: Arc<C>,
    mut limits: RpcLimits,
) -> (tempfile::TempDir, Arc<RpcSession>) {
    let (cache, prepared, mut handshake, processes) = prepared_session();
    handshake.configuration = json!({"sdk_session": true});
    limits.maximum_queue_timeout = Duration::from_secs(2);
    let session = RpcSession::start(prepared, handshake, limits, &processes, callbacks)
        .await
        .unwrap();
    (cache, Arc::new(session))
}

struct CrossCallbacks {
    other: std::sync::Mutex<std::sync::Weak<RpcSession>>,
    ready: Arc<tokio::sync::Barrier>,
}

impl CallbackHandler for CrossCallbacks {
    fn call(
        &self,
        _context: CallContext,
        _method: String,
        _params: serde_json::Value,
        _payload: Vec<u8>,
    ) -> BoxFuture<'static, Result<RpcReply, PluginFault>> {
        let other = self.other.lock().unwrap().upgrade().unwrap();
        let ready = self.ready.clone();
        Box::pin(async move {
            ready.wait().await;
            other
                .call(
                    "echo",
                    other.context(Stage::Request, Duration::from_secs(2)),
                    json!("child"),
                    Vec::new(),
                )
                .await
                .map_err(|_| PluginFault::new(gateway_plugin_sdk::ErrorCode::Fault, "child failed"))
        })
    }
}

#[tokio::test]
async fn independent_sessions_release_execution_for_every_callback_method() {
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let left = Arc::new(CrossCallbacks {
        other: Default::default(),
        ready: barrier.clone(),
    });
    let right = Arc::new(CrossCallbacks {
        other: Default::default(),
        ready: barrier,
    });
    let (_left_cache, a) = scheduled(left.clone(), limits()).await;
    let (_right_cache, b) = scheduled(right.clone(), limits()).await;
    *left.other.lock().unwrap() = Arc::downgrade(&b);
    *right.other.lock().unwrap() = Arc::downgrade(&a);
    for method in [
        "host.service.call",
        "host.http.dispatch",
        "host.model.execute",
        "host.client.verify",
        NEXT_METHOD,
        "host.unclassified",
    ] {
        let invoke = |session: Arc<RpcSession>| async move {
            session
                .call(
                    "callback",
                    session.context(Stage::Request, Duration::from_secs(2)),
                    json!({"method":method}),
                    Vec::new(),
                )
                .await
        };
        let (a_result, b_result) = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(invoke(a.clone()), invoke(b.clone()))
        })
        .await
        .unwrap();
        assert_eq!(a_result.unwrap().result, json!("child"), "{method}");
        assert_eq!(b_result.unwrap().result, json!("child"), "{method}");
    }
    a.shutdown(Duration::from_secs(1)).await;
    b.shutdown(Duration::from_secs(1)).await;
}

struct PartialCallbacks {
    entered: mpsc::UnboundedSender<String>,
    fast: Arc<Semaphore>,
    slow: Arc<Semaphore>,
}

impl CallbackHandler for PartialCallbacks {
    fn call(
        &self,
        _context: CallContext,
        method: String,
        _params: serde_json::Value,
        _payload: Vec<u8>,
    ) -> BoxFuture<'static, Result<RpcReply, PluginFault>> {
        self.entered.send(method.clone()).unwrap();
        let gate = match method.as_str() {
            "fast" => Some(self.fast.clone()),
            "slow" => Some(self.slow.clone()),
            _ => None,
        };
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.acquire().await.unwrap().forget();
            }
            Ok(RpcReply {
                result: json!(method),
                payload: Vec::new(),
            })
        })
    }
}

#[tokio::test]
async fn a_partial_callback_result_requires_a_permit_and_can_wait_again() {
    let (entered, mut events) = mpsc::unbounded_channel();
    let fast = Arc::new(Semaphore::new(0));
    let slow = Arc::new(Semaphore::new(0));
    let (cache, session) = scheduled(
        Arc::new(PartialCallbacks {
            entered,
            fast: fast.clone(),
            slow: slow.clone(),
        }),
        limits(),
    )
    .await;
    let parent = tokio::spawn({
        let session = session.clone();
        async move { call(&session, "partial").await }
    });
    for _ in 0..2 {
        tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap();
    }
    let marker = cache.path().join("processing");
    let busy = tokio::spawn({
        let session = session.clone();
        let marker = marker.clone();
        async move {
            session
                .call(
                    "busy",
                    session.context(Stage::Request, Duration::from_secs(2)),
                    json!({"marker":marker, "delay_ms":300}),
                    Vec::new(),
                )
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !marker.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    fast.add_permits(1);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), events.recv())
            .await
            .is_err()
    );
    busy.await.unwrap().unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .as_deref(),
        Some("processed")
    );
    assert!(!parent.is_finished());
    // 再次等待慢回调后，独立调用可取得同一个执行名额
    assert!(call(&session, "echo").await.is_ok());
    slow.add_permits(1);
    assert_eq!(parent.await.unwrap().unwrap().result, json!("complete"));
    session.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn polling_wakeups_and_cancelled_suspensions_leave_the_session_usable() {
    let (entered, mut events) = mpsc::unbounded_channel();
    let slow = Arc::new(Semaphore::new(0));
    let (_cache, session) = scheduled(
        Arc::new(PartialCallbacks {
            entered,
            fast: Arc::new(Semaphore::new(0)),
            slow,
        }),
        RpcLimits {
            maximum_calls: 1,
            ..limits()
        },
    )
    .await;
    assert_eq!(
        call(&session, "yield").await.unwrap().result,
        json!("yielded")
    );
    let parent = tokio::spawn({
        let session = session.clone();
        async move {
            session
                .call(
                    "callback",
                    session.context(Stage::Request, Duration::from_secs(2)),
                    json!({"method":"slow"}),
                    Vec::new(),
                )
                .await
        }
    });
    assert_eq!(events.recv().await.as_deref(), Some("slow"));
    assert!(matches!(
        call(&session, "echo").await,
        Err(RpcError::Overloaded)
    ));
    parent.abort();
    assert!(matches!(parent.await, Err(error) if error.is_cancelled()));
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match call(&session, "echo").await {
                Ok(_) => break,
                Err(RpcError::Overloaded) => tokio::task::yield_now().await,
                Err(error) => panic!("{error}"),
            }
        }
    })
    .await
    .unwrap();
    assert!(session.is_ready());
    session.shutdown(Duration::from_secs(1)).await;
}
