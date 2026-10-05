//! 验证受管 WebSocket 的消息收发、关闭、超时与代理隔离

use std::time::Duration;

use futures::{SinkExt as _, StreamExt as _};
use gateway_core::{account::OutboundProxy, upstream::UpstreamSendState};
use gateway_host::outbound::{
    HttpClient, HttpErrorKind, HttpRequest, NetworkPolicy, WebSocketMessage,
};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

fn request(url: String) -> HttpRequest {
    HttpRequest {
        method: "GET".into(),
        url,
        headers: vec![],
        body: vec![],
    }
}
fn network() -> NetworkPolicy {
    NetworkPolicy::new(&["127.0.0.0/8".into()]).unwrap()
}

#[tokio::test]
async fn websocket_preserves_frames_handles_ping_and_releases_connection_on_drop() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/responses", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut websocket = tokio_tungstenite::accept_async(socket).await.unwrap();
        assert_eq!(
            websocket.next().await.unwrap().unwrap(),
            Message::Text("first".into())
        );
        websocket
            .send(Message::Ping(vec![1, 2].into()))
            .await
            .unwrap();
        websocket
            .send(Message::Binary(vec![0, 255, 7].into()))
            .await
            .unwrap();
        assert_eq!(
            websocket.next().await.unwrap().unwrap(),
            Message::Pong(vec![1, 2].into())
        );
        assert!(
            websocket
                .next()
                .await
                .is_none_or(|message| message.is_err())
        );
    });
    let mut response = HttpClient::new()
        .unwrap()
        .open_websocket(request(url), None, &network(), Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(response.status, 101);
    let websocket = response.connection.take().unwrap();
    websocket
        .send(
            WebSocketMessage::Text("first".into()),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
    let Some(WebSocketMessage::Binary(bytes)) =
        websocket.read(Duration::from_secs(2)).await.unwrap()
    else {
        panic!("binary frame");
    };
    assert_eq!(&bytes[..], &[0, 255, 7]);
    drop(websocket);
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn websocket_can_send_control_while_waiting_for_a_response() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/responses", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
        assert_eq!(
            socket.next().await.unwrap().unwrap(),
            Message::Text("control".into())
        );
        socket
            .send(Message::Text("acknowledged".into()))
            .await
            .unwrap();
    });
    let websocket = HttpClient::new()
        .unwrap()
        .open_websocket(request(url), None, &network(), Duration::from_secs(2))
        .await
        .unwrap()
        .connection
        .unwrap();
    let read = websocket.read(Duration::from_secs(2));
    tokio::pin!(read);
    assert!(futures::poll!(&mut read).is_pending());
    websocket
        .send(
            WebSocketMessage::Text("control".into()),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
    assert!(
        matches!(read.await.unwrap(), Some(WebSocketMessage::Text(text)) if text == "acknowledged")
    );
    server.await.unwrap();
}

#[tokio::test]
async fn websocket_close_wakes_pending_reads_and_releases_the_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/responses", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
        assert!(socket.next().await.is_none_or(|result| result.is_err()));
    });
    let websocket = HttpClient::new()
        .unwrap()
        .open_websocket(request(url), None, &network(), Duration::from_secs(2))
        .await
        .unwrap()
        .connection
        .unwrap();
    let read = websocket.read(Duration::from_secs(60));
    tokio::pin!(read);
    assert!(futures::poll!(&mut read).is_pending());
    let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(read, websocket.close())
    })
    .await
    .unwrap();
    assert!(result.unwrap().is_none());
    assert!(websocket.is_closed());
    assert!(
        websocket
            .read(Duration::from_secs(1))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        websocket
            .send(
                WebSocketMessage::Text("late".into()),
                Duration::from_secs(1)
            )
            .await
            .is_err()
    );
    websocket.close().await;
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn websocket_read_timeout_closes_the_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/responses", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
        assert!(socket.next().await.is_none_or(|result| result.is_err()));
    });
    let websocket = HttpClient::new()
        .unwrap()
        .open_websocket(request(url), None, &network(), Duration::from_secs(2))
        .await
        .unwrap()
        .connection
        .unwrap();
    let error = websocket
        .read(Duration::from_millis(20))
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind(), HttpErrorKind::Timeout);
    assert!(websocket.is_closed());
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn websocket_rejections_return_http_body_and_never_fall_back_from_proxy() {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::any};
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(401).set_body_string("denied"))
        .expect(1)
        .mount(&server)
        .await;
    let client = HttpClient::new().unwrap();
    let response = client
        .open_websocket(
            request(server.uri()),
            None,
            &network(),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
    assert_eq!(response.status, 401);
    assert!(response.connection.is_none());
    assert_eq!(
        &response.body.unwrap().read(64).await.unwrap().unwrap()[..],
        b"denied"
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy =
        OutboundProxy::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    drop(listener);
    let error = client
        .open_websocket(
            request(server.uri()),
            Some(&proxy),
            &network(),
            Duration::from_secs(2),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(error.send_state, UpstreamSendState::NotSent);
    assert_eq!(error.kind(), HttpErrorKind::Transport);
}

#[tokio::test]
async fn websocket_refuses_invalid_upgrade_and_header_injection() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        while !bytes.ends_with(b"\r\n\r\n") {
            assert!(bytes.len() < 4096);
            bytes.push(socket.read_u8().await.unwrap());
        }
        socket.write_all(b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: invalid\r\n\r\n").await.unwrap();
    });
    let client = HttpClient::new().unwrap();
    let mut invalid = request(url.clone());
    invalid
        .headers
        .push(("sec-websocket-key".into(), b"injected".to_vec()));
    let error = client
        .open_websocket(invalid, None, &network(), Duration::from_secs(2))
        .await
        .err()
        .unwrap();
    assert_eq!(error.send_state, UpstreamSendState::NotSent);
    let error = client
        .open_websocket(request(url), None, &network(), Duration::from_secs(2))
        .await
        .err()
        .unwrap();
    assert_eq!(error.send_state, UpstreamSendState::Sent);
    server.await.unwrap();
}
