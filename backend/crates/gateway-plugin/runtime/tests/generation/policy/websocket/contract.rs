//! 跨模块行为合同测试

use super::*;

#[tokio::test]
async fn real_plugin_passes_large_binary_messages_by_handle_in_both_directions() {
    let (_cache, runtime, generation, plan) = websocket_plugin("passthrough").await;
    for direction in [core::Direction::Incoming, core::Direction::Outgoing] {
        let payload = Bytes::from(vec![255; 20 * 1024 * 1024]);
        let pointer = payload.as_ptr();
        let message = core::Message {
            kind: core::Kind::Binary,
            payload,
        };
        let result = plan
            .handle_websocket(
                context(direction, Arc::default()),
                message,
                compose(Vec::new(), |message| Box::pin(async { Ok(Some(message)) })),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.kind, core::Kind::Binary);
        assert_eq!(result.payload.len(), 20 * 1024 * 1024);
        assert_eq!(
            result.payload.as_ptr(),
            pointer,
            "unread payload stays in the host"
        );
    }
    drop(plan);
    drop(generation);
    runtime.shutdown().await;
}

#[tokio::test]
async fn real_plugin_consumes_unknown_control_and_sends_a_binary_reply() {
    let (_cache, runtime, generation, plan) = websocket_plugin("control").await;
    let sender = Arc::new(Sender::default());
    let result = plan
        .handle_websocket(
            context(core::Direction::Incoming, sender.clone()),
            core::Message {
                kind: core::Kind::Text,
                payload: Bytes::from_static(b"custom.control"),
            },
            compose(Vec::new(), |_| {
                Box::pin(async { panic!("consumed control must not reach protocol parser") })
            }),
        )
        .await
        .unwrap();
    assert!(result.is_none());
    {
        let sent = sender.0.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].payload.as_ref(), &[0, 255, 1]);
    }
    drop(plan);
    drop(generation);
    runtime.shutdown().await;
}

#[tokio::test]
async fn real_plugin_rewrites_payload_before_next() {
    let (_cache, runtime, generation, plan) = websocket_plugin("map").await;
    let result = plan
        .handle_websocket(
            context(core::Direction::Outgoing, Arc::default()),
            core::Message {
                kind: core::Kind::Text,
                payload: Bytes::from_static(b"opaque output"),
            },
            compose(Vec::new(), |message: core::Message| {
                Box::pin(async {
                    assert_eq!(message.payload, "OPAQUE OUTPUT");
                    Ok(Some(message))
                })
            }),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.payload, "OPAQUE OUTPUT");
    drop(plan);
    drop(generation);
    runtime.shutdown().await;
}
