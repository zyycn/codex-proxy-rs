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
        maximum_callbacks: 1,
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
    // 等待 next 不挤占唯一的普通回调额度
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
