//! 会话期限、取消钩子、排空与关闭测试

use super::*;

#[tokio::test]
async fn call_deadline_cancels_handler_and_keeps_the_session_available() {
    let handler = TestHandler::default();
    let state = Arc::clone(&handler.lifecycle);
    let (mut host, task) = start_session(handler).await;
    send_call_with_timeout(
        &mut host,
        1,
        "pending",
        json!({}),
        Vec::new(),
        Duration::from_millis(20),
    )
    .await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Error {
            id: 1,
            error: PluginFault {
                code: ErrorCode::Timeout,
                ..
            },
        }
    ));
    tokio::time::timeout(Duration::from_millis(100), async {
        while !state.cancel_started.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("deadline did not notify the lifecycle hook");

    send_call(&mut host, 3, "echo", json!("still-ready"), Vec::new()).await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Result { id: 3, result } if result == json!("still-ready")
    ));
    shutdown(&mut host, task).await;
}

#[tokio::test]
async fn blocking_cancel_hook_cannot_delay_cancelled_or_eof_settlement() {
    let handler = TestHandler {
        cancel_delay: Some(Duration::from_millis(200)),
        ..TestHandler::default()
    };
    let state = Arc::clone(&handler.lifecycle);
    let (mut host, task) = start_session(handler).await;
    send_call(&mut host, 1, "pending", json!({}), Vec::new()).await;
    send_control(&mut host, Message::Cancel { id: 1 }).await;

    let cancelled = tokio::time::timeout(Duration::from_millis(50), receive(&mut host))
        .await
        .expect("blocking lifecycle hook delayed Cancelled");
    assert!(matches!(cancelled.message, Message::Cancelled { id: 1 }));
    tokio::time::timeout(Duration::from_millis(50), async {
        while !state.cancel_started.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancel hook never started");

    host.writer.shutdown().await.unwrap();
    let result = tokio::time::timeout(Duration::from_millis(50), task)
        .await
        .expect("blocking lifecycle hook delayed EOF settlement")
        .expect("plugin session task panicked");
    assert!(matches!(result, Err(SessionError::Closed)));
}

#[tokio::test]
async fn quiesce_rejects_new_calls_and_shutdown_closes_the_session() {
    let handler = TestHandler::default();
    let state = Arc::clone(&handler.lifecycle);
    let (mut host, task) = start_session(handler).await;
    send_control(&mut host, Message::Quiesce).await;
    send_call(&mut host, 1, "echo", json!({}), Vec::new()).await;

    assert!(matches!(
        receive(&mut host).await.message,
        Message::Error {
            id: 1,
            error: PluginFault {
                code: ErrorCode::Capacity,
                ..
            },
        }
    ));
    send_control(
        &mut host,
        Message::Credit {
            id: 1,
            bytes: 1,
            frames: 1,
        },
    )
    .await;
    tokio::time::timeout(Duration::from_millis(50), async {
        while !state.quiesced.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("quiesce hook was not signalled");

    shutdown(&mut host, task).await;
}

async fn send_middleware_call(host: &mut HostPeer, id: u64, resource_stream: bool) {
    let mut context = middleware_context(id);
    context.resource_stream = resource_stream;
    write_frame(
        &mut host.writer,
        &Frame {
            message: Message::Call {
                id,
                method: HANDLE_METHOD.into(),
                context,
                params: serde_json::to_value(middleware_request()).unwrap(),
            },
            payload: Vec::new(),
        },
    )
    .await
    .unwrap();
    send_control(
        host,
        Message::Credit {
            id,
            bytes: 1024,
            frames: 2,
        },
    )
    .await;
}

async fn receive_next(host: &mut HostPeer, parent: u64) -> u64 {
    let Message::Callback {
        id,
        parent_id,
        method,
        ..
    } = receive(host).await.message
    else {
        panic!("expected middleware next")
    };
    assert_eq!(parent_id, parent);
    assert_eq!(method, NEXT_METHOD);
    id
}

async fn reply_next(host: &mut HostPeer, id: u64) {
    send_control(
        host,
        Message::Result {
            id,
            result: serde_json::to_value(MiddlewareNextResponse {
                metadata: None,
                response: format!("response-{id}"),
                protocol: "openai".into(),
                status: 200,
                headers: Vec::new(),
                body: None,
            })
            .unwrap(),
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn managed_next_can_outlive_the_initial_budget_with_or_without_resource_stream() {
    for resource_stream in [false, true] {
        let (mut host, task) = start_session(middleware_plugin(false)).await;
        send_middleware_call(&mut host, 1, resource_stream).await;
        let next = receive_next(&mut host, 1).await;
        tokio::time::advance(Duration::from_secs(10)).await;
        reply_next(&mut host, next).await;
        assert!(matches!(
            receive(&mut host).await.message,
            Message::Result { id: 1, .. }
        ));
        assert!(matches!(
            receive(&mut host).await.message,
            Message::End { id: 1, error: None }
        ));
        shutdown(&mut host, task).await;
    }
}

#[tokio::test(start_paused = true)]
async fn response_processing_uses_only_the_budget_left_before_next() {
    let plugin = MiddlewarePlugin::new(
        &middleware_contributions(),
        |call: RequestCall| async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let response = call.next.run(call.request).await?;
            tokio::time::sleep(Duration::from_millis(850)).await;
            Ok(response)
        },
    )
    .unwrap();
    let (mut host, task) = start_session(plugin).await;
    send_middleware_call(&mut host, 1, false).await;
    let next = receive_next(&mut host, 1).await;
    tokio::time::advance(Duration::from_secs(10)).await;
    reply_next(&mut host, next).await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Error {
            id: 1,
            error: PluginFault {
                code: ErrorCode::Timeout,
                ..
            }
        }
    ));
    shutdown(&mut host, task).await;
}

#[tokio::test(start_paused = true)]
async fn waiting_next_does_not_suspend_an_independent_handlers_budget() {
    let plugin = MiddlewarePlugin::new(
        &middleware_contributions(),
        |call: RequestCall| async move {
            if call.context.call_id == 3 {
                return pending().await;
            }
            call.next.run(call.request).await
        },
    )
    .unwrap();
    let (mut host, task) = start_session(plugin).await;
    send_middleware_call(&mut host, 1, false).await;
    let next = receive_next(&mut host, 1).await;
    send_middleware_call(&mut host, 3, false).await;
    // 先让第二个 Call 进入 SDK，再推进到其预算之外
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Error {
            id: 3,
            error: PluginFault {
                code: ErrorCode::Timeout,
                ..
            }
        }
    ));
    reply_next(&mut host, next).await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Result { id: 1, .. }
    ));
    shutdown(&mut host, task).await;
}

#[tokio::test(start_paused = true)]
async fn cancelling_managed_next_retires_its_late_reply_and_keeps_session_available() {
    let (mut host, task) = start_session(middleware_plugin(false)).await;
    send_middleware_call(&mut host, 1, false).await;
    let next = receive_next(&mut host, 1).await;
    tokio::time::advance(Duration::from_secs(10)).await;
    send_control(&mut host, Message::Cancel { id: 1 }).await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Cancelled { id: 1 }
    ));
    reply_next(&mut host, next).await;
    send_middleware_call(&mut host, 3, false).await;
    let next = receive_next(&mut host, 3).await;
    reply_next(&mut host, next).await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Result { id: 3, .. }
    ));
    shutdown(&mut host, task).await;
}

#[tokio::test(start_paused = true)]
async fn ordinary_callbacks_still_consume_the_parent_budget() {
    let (mut host, task) = start_session(TestHandler::default()).await;
    send_call_with_timeout(
        &mut host,
        1,
        "callback",
        json!({}),
        Vec::new(),
        Duration::from_millis(100),
    )
    .await;
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Callback { parent_id: 1, .. }
    ));
    assert!(matches!(
        receive(&mut host).await.message,
        Message::Error {
            id: 1,
            error: PluginFault {
                code: ErrorCode::Timeout,
                ..
            }
        }
    ));
    shutdown(&mut host, task).await;
}
