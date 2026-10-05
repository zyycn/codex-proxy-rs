//! 验证自定义 WebSocket 升级、双向收发与取消后的资源回收

use super::*;
use futures::{SinkExt as _, StreamExt as _};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest as _},
};

#[tokio::test]
async fn custom_upgrade_keeps_the_call_alive_and_allows_send_during_receive() {
    let (router, plan) = router(
        Mode::Upgrade {
            reject: false,
            idle: false,
            block_messages: false,
        },
        None,
    )
    .await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut request = format!("ws://{address}/plugin-owned/session")
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            "sec-websocket-protocol",
            "other, test-session".parse().unwrap(),
        );
        let (mut socket, response) = connect_async(request).await.unwrap();
        assert_eq!(response.headers()["sec-websocket-protocol"], "test-session");
        assert_eq!(
            socket.next().await.unwrap().unwrap(),
            Message::Text("ready".into())
        );
        let cancellation = plan.cancellation.lock().unwrap().clone().unwrap();
        assert!(!cancellation.is_cancelled(), "101 正文结束不能取消会话");
        socket
            .send(Message::Binary(vec![0, 255, 128].into()))
            .await
            .unwrap();
        assert_eq!(
            socket.next().await.unwrap().unwrap(),
            Message::Binary(vec![0, 255, 128].into())
        );
        let Message::Close(Some(close)) = socket.next().await.unwrap().unwrap() else {
            panic!("close frame expected");
        };
        assert_eq!(close.reason, "done");
        cancellation.cancelled().await;
        assert_eq!(
            plan.calls.load(Ordering::SeqCst),
            5,
            "HTTP 握手及四次消息收发各组合一次"
        );
    })
    .await
    .unwrap();
    server.abort();
}

#[tokio::test]
async fn replacing_upgrade_status_drops_the_session_before_handshake() {
    let (router, plan) = router(
        Mode::Upgrade {
            reject: true,
            idle: false,
            block_messages: false,
        },
        None,
    )
    .await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        connect_async(format!("ws://{address}/plugin-owned/session")),
    )
    .await
    .unwrap();
    assert!(
        matches!(result, Err(tokio_tungstenite::tungstenite::Error::Http(response)) if response.status() == StatusCode::BAD_GATEWAY)
    );
    assert!(
        plan.cancellation
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .is_cancelled()
    );
    server.abort();
}

#[tokio::test]
async fn disconnect_cancels_an_idle_session_without_a_plugin_receive_call() {
    let (router, plan) = router(
        Mode::Upgrade {
            reject: false,
            idle: true,
            block_messages: false,
        },
        None,
    )
    .await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let (socket, _) = connect_async(format!("ws://{address}/idle")).await.unwrap();
    let cancellation = plan.cancellation.lock().unwrap().clone().unwrap();
    assert!(!cancellation.is_cancelled());
    drop(socket);
    tokio::time::timeout(Duration::from_secs(5), cancellation.cancelled())
        .await
        .unwrap();
    server.abort();
}

#[tokio::test]
async fn cancelling_custom_session_drops_a_blocked_message_middleware() {
    let (router, plan) = router(
        Mode::Upgrade {
            reject: false,
            idle: false,
            block_messages: true,
        },
        None,
    )
    .await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let (socket, _) = connect_async(format!("ws://{address}/blocked"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), plan.message_lifetime.0.notified())
        .await
        .unwrap();
    let cancellation = plan.cancellation.lock().unwrap().clone().unwrap();
    cancellation.cancel();
    tokio::time::timeout(Duration::from_secs(5), plan.message_lifetime.1.notified())
        .await
        .unwrap();
    assert_eq!(
        plan.calls.load(Ordering::SeqCst),
        2,
        "取消后不会重新进入消息链"
    );
    drop(socket);
    server.abort();
}
