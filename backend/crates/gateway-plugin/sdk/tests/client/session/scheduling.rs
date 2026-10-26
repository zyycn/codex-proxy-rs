//! 协作调度的部分完成、唤醒竞争、预算与回调回收合同测试

use std::{
    future::{Future, poll_fn},
    task::Poll,
};

use super::*;

struct ScheduledHandler {
    progressed: Arc<AtomicUsize>,
}

impl PluginHandler for ScheduledHandler {
    fn call(&self, call: PluginCall) -> CallFuture<'_> {
        Box::pin(async move {
            match call.method.as_str() {
                "parallel" | "mixed" => {
                    let first_method = if call.method == "mixed" {
                        NEXT_METHOD
                    } else {
                        "host.first"
                    };
                    let first = async {
                        let reply = call.host.call(first_method, json!({}), Vec::new()).await;
                        self.progressed.fetch_add(1, Ordering::SeqCst);
                        reply
                    };
                    let second = async {
                        let reply = call.host.call("host.second", json!({}), Vec::new()).await;
                        self.progressed.fetch_add(1, Ordering::SeqCst);
                        reply
                    };
                    let (first, second) = tokio::join!(first, second);
                    first.map_err(SessionError::into_plugin_fault)?;
                    second.map_err(SessionError::into_plugin_fault)?;
                }
                "yield" => {
                    tokio::task::yield_now().await;
                    self.progressed.fetch_add(1, Ordering::SeqCst);
                }
                "drop_callback" => {
                    let mut callback =
                        Box::pin(call.host.call("host.detached", json!({}), Vec::new()));
                    poll_fn(|cx| {
                        assert!(callback.as_mut().poll(cx).is_pending());
                        Poll::Ready(())
                    })
                    .await;
                    drop(callback);
                }
                "saturate" => {
                    let mut callbacks = [
                        Box::pin(call.host.call(NEXT_METHOD, json!({}), Vec::new())),
                        Box::pin(call.host.call(NEXT_METHOD, json!({}), Vec::new())),
                    ];
                    for callback in &mut callbacks {
                        poll_fn(|cx| {
                            assert!(callback.as_mut().poll(cx).is_pending());
                            Poll::Ready(())
                        })
                        .await;
                    }
                    let refused = call.host.call("host.third", json!({}), Vec::new()).await;
                    assert!(matches!(refused, Err(SessionError::Capacity)));
                }
                _ => {
                    return Err(PluginFault::new(
                        ErrorCode::Unsupported,
                        "unknown scheduling test call",
                    ));
                }
            }
            Ok(CallReply::unary(json!({}), Vec::new()))
        })
    }
}

async fn wire(host: &mut HostPeer) -> Message {
    let frame = tokio::time::timeout(Duration::from_secs(1), read_frame(&mut host.reader))
        .await
        .expect("expected protocol progress")
        .unwrap();
    assert!(frame.payload.is_empty());
    frame.message
}

async fn callback(host: &mut HostPeer, method: &str) -> u64 {
    let Message::Callback {
        id,
        parent_id: 1,
        method: actual,
        ..
    } = wire(host).await
    else {
        panic!("expected callback before suspension");
    };
    assert_eq!(actual, method);
    id
}

async fn callback_result(host: &mut HostPeer, id: u64) {
    send_control(
        host,
        Message::Result {
            id,
            result: json!({}),
        },
    )
    .await;
}

#[tokio::test]
async fn partial_callback_results_require_a_new_execution_grant_before_processing() {
    let progressed = Arc::new(AtomicUsize::new(0));
    let (mut host, task) = start_session(ScheduledHandler {
        progressed: Arc::clone(&progressed),
    })
    .await;
    send_call(&mut host, 1, "parallel", json!({}), Vec::new()).await;
    let first = callback(&mut host, "host.first").await;
    let second = callback(&mut host, "host.second").await;
    assert!(matches!(
        wire(&mut host).await,
        Message::Suspend { id: 1, epoch: 1 }
    ));

    callback_result(&mut host, first).await;
    assert!(matches!(
        wire(&mut host).await,
        Message::Resume { id: 1, epoch: 1 }
    ));
    assert_eq!(progressed.load(Ordering::SeqCst), 0);
    send_control(&mut host, Message::Resumed { id: 1, epoch: 1 }).await;
    assert!(
        matches!(wire(&mut host).await, Message::CallbackConsumed { id, parent_id: 1 } if id == first)
    );
    assert!(matches!(
        wire(&mut host).await,
        Message::Suspend { id: 1, epoch: 2 }
    ));
    assert_eq!(progressed.load(Ordering::SeqCst), 1);

    callback_result(&mut host, second).await;
    assert!(matches!(
        wire(&mut host).await,
        Message::Resume { id: 1, epoch: 2 }
    ));
    assert_eq!(progressed.load(Ordering::SeqCst), 1);
    send_control(&mut host, Message::Resumed { id: 1, epoch: 2 }).await;
    assert!(
        matches!(wire(&mut host).await, Message::CallbackConsumed { id, parent_id: 1 } if id == second)
    );
    assert!(matches!(
        wire(&mut host).await,
        Message::Result { id: 1, .. }
    ));
    assert_eq!(progressed.load(Ordering::SeqCst), 2);
    shutdown(&mut host, task).await;
}

#[tokio::test]
async fn wake_during_poll_is_preserved_but_cannot_skip_host_authorization() {
    let progressed = Arc::new(AtomicUsize::new(0));
    let (mut host, task) = start_session(ScheduledHandler {
        progressed: Arc::clone(&progressed),
    })
    .await;
    send_call(&mut host, 1, "yield", json!({}), Vec::new()).await;
    assert!(matches!(
        wire(&mut host).await,
        Message::Suspend { id: 1, epoch: 1 }
    ));
    assert!(matches!(
        wire(&mut host).await,
        Message::Resume { id: 1, epoch: 1 }
    ));
    assert_eq!(progressed.load(Ordering::SeqCst), 0);
    send_control(&mut host, Message::Resumed { id: 1, epoch: 1 }).await;
    assert!(matches!(
        wire(&mut host).await,
        Message::Result { id: 1, .. }
    ));
    assert_eq!(progressed.load(Ordering::SeqCst), 1);
    shutdown(&mut host, task).await;
}

#[tokio::test]
async fn cancellation_during_resume_does_not_allow_late_grants_to_revive_the_call() {
    let (mut host, task) = start_session(TestHandler::default()).await;
    send_call(&mut host, 1, "callback", json!({}), Vec::new()).await;
    let child = callback(&mut host, "host.log").await;
    assert!(matches!(
        wire(&mut host).await,
        Message::Suspend { id: 1, epoch: 1 }
    ));
    callback_result(&mut host, child).await;
    assert!(matches!(
        wire(&mut host).await,
        Message::Resume { id: 1, epoch: 1 }
    ));
    send_control(&mut host, Message::Cancel { id: 1 }).await;
    send_control(&mut host, Message::Resumed { id: 1, epoch: 1 }).await;
    assert!(matches!(
        wire(&mut host).await,
        Message::Cancelled { id: 1 }
    ));
    send_call(&mut host, 3, "echo", json!("alive"), Vec::new()).await;
    assert!(
        matches!(wire(&mut host).await, Message::Result { id: 3, result } if result == json!("alive"))
    );
    shutdown(&mut host, task).await;
}

#[tokio::test(start_paused = true)]
async fn next_pause_covers_resume_wait_but_cannot_hide_parallel_chargeable_work() {
    let (mut host, task) = start_session(TestHandler::default()).await;
    send_call_with_timeout(
        &mut host,
        1,
        "next_callback",
        json!({}),
        Vec::new(),
        Duration::from_millis(100),
    )
    .await;
    let child = callback(&mut host, NEXT_METHOD).await;
    assert!(matches!(
        wire(&mut host).await,
        Message::Suspend { id: 1, epoch: 1 }
    ));
    tokio::time::advance(Duration::from_secs(2)).await;
    callback_result(&mut host, child).await;
    assert!(matches!(
        wire(&mut host).await,
        Message::Resume { id: 1, epoch: 1 }
    ));
    tokio::time::advance(Duration::from_secs(2)).await;
    send_control(&mut host, Message::Resumed { id: 1, epoch: 1 }).await;
    assert!(
        matches!(wire(&mut host).await, Message::CallbackConsumed { id, parent_id: 1 } if id == child)
    );
    assert!(matches!(
        wire(&mut host).await,
        Message::Result { id: 1, .. }
    ));
    shutdown(&mut host, task).await;

    let (mut host, task) = start_session(ScheduledHandler {
        progressed: Arc::new(AtomicUsize::new(0)),
    })
    .await;
    send_call_with_timeout(
        &mut host,
        1,
        "mixed",
        json!({}),
        Vec::new(),
        Duration::from_millis(100),
    )
    .await;
    callback(&mut host, NEXT_METHOD).await;
    callback(&mut host, "host.second").await;
    assert!(matches!(
        wire(&mut host).await,
        Message::Suspend { id: 1, epoch: 1 }
    ));
    tokio::time::advance(Duration::from_millis(101)).await;
    loop {
        match wire(&mut host).await {
            Message::CallbackConsumed { parent_id: 1, .. } => {}
            Message::Error { id: 1, error } => {
                assert_eq!(error.code, ErrorCode::Timeout);
                break;
            }
            other => panic!("chargeable work must time out without a grant: {other:?}"),
        }
    }
    shutdown(&mut host, task).await;
}

#[tokio::test]
async fn dropping_a_callback_reports_consumption_before_the_parent_result() {
    let (mut host, task) = start_session(ScheduledHandler {
        progressed: Arc::new(AtomicUsize::new(0)),
    })
    .await;
    send_call(&mut host, 1, "drop_callback", json!({}), Vec::new()).await;
    let child = callback(&mut host, "host.detached").await;
    assert!(
        matches!(wire(&mut host).await, Message::CallbackConsumed { id, parent_id: 1 } if id == child)
    );
    assert!(matches!(
        wire(&mut host).await,
        Message::Result { id: 1, .. }
    ));
    callback_result(&mut host, child).await;
    shutdown(&mut host, task).await;
}

#[tokio::test]
async fn all_callback_methods_share_the_same_bounded_resident_capacity() {
    let (mut host, task) = start_configured_session(
        ScheduledHandler {
            progressed: Arc::new(AtomicUsize::new(0)),
        },
        MAXIMUM_STREAM_CHUNK_BYTES * 2,
        SessionConfig {
            maximum_calls: 1,
            maximum_callbacks: 2,
            ..Default::default()
        },
    )
    .await;
    send_call(&mut host, 1, "saturate", json!({}), Vec::new()).await;
    let first = callback(&mut host, NEXT_METHOD).await;
    let second = callback(&mut host, NEXT_METHOD).await;
    let mut consumed = Vec::new();
    for _ in 0..2 {
        let Message::CallbackConsumed { id, parent_id: 1 } = wire(&mut host).await else {
            panic!("resident callbacks must be reclaimed");
        };
        consumed.push(id);
    }
    consumed.sort_unstable();
    assert_eq!(consumed, [first, second]);
    assert!(matches!(
        wire(&mut host).await,
        Message::Result { id: 1, .. }
    ));
    shutdown(&mut host, task).await;
}
