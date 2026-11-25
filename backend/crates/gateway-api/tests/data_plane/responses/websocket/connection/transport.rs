//! 实际 HTTP 升级及 WS 编解码下，慢速上传与半帧停传的心跳边界

use std::{
    io,
    pin::Pin,
    sync::{Arc, atomic::Ordering},
    task::{Context, Poll},
    time::Duration,
};

use futures::{SinkExt, StreamExt, ready};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf},
    time::{Instant, Sleep},
};
use tokio_tungstenite::{
    WebSocketStream, client_async,
    tungstenite::{Message, client::IntoClientRequest},
};
use tower::ServiceExt;

use super::{CrossingLimitExecution, CrossingLimitTrace, api_router, authenticated_client};

struct SlowUpload {
    inner: DuplexStream,
    slow: bool,
    next_write: Option<Pin<Box<Sleep>>>,
}

impl AsyncRead for SlowUpload {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buffer)
    }
}

impl AsyncWrite for SlowUpload {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.slow {
            if let Some(delay) = self.next_write.as_mut() {
                ready!(delay.as_mut().poll(cx));
            }
            let size = ready!(
                Pin::new(&mut self.inner).poll_write(cx, &buffer[..buffer.len().min(4096)])
            )?;
            self.next_write = Some(Box::pin(tokio::time::sleep(Duration::from_secs(1))));
            Poll::Ready(Ok(size))
        } else {
            Pin::new(&mut self.inner).poll_write(cx, buffer)
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

async fn connect() -> (WebSocketStream<SlowUpload>, Arc<CrossingLimitTrace>) {
    let trace = Arc::new(CrossingLimitTrace::default());
    let app = api_router(Arc::new(CrossingLimitExecution {
        client: authenticated_client("sk_ws_crossing_limit"),
        trace: trace.clone(),
    }))
    .await;
    let (client, server) = tokio::io::duplex(65536);
    tokio::spawn(async move {
        hyper::server::conn::http1::Builder::new()
            .serve_connection(
                TokioIo::new(server),
                hyper::service::service_fn(move |request| app.clone().oneshot(request)),
            )
            .with_upgrades()
            .await
            .expect("serve Responses upgrade");
    });
    let mut request = "ws://localhost/v1/responses".into_client_request().unwrap();
    request.headers_mut().insert(
        http::header::AUTHORIZATION,
        "Bearer sk_ws_crossing_limit".parse().unwrap(),
    );
    let (socket, response) = client_async(
        request,
        SlowUpload {
            inner: client,
            slow: false,
            next_write: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(response.status(), http::StatusCode::SWITCHING_PROTOCOLS);
    (socket, trace)
}

async fn receive_event(socket: &mut WebSocketStream<SlowUpload>, expected: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let message = socket
                .next()
                .await
                .expect("connection stays open")
                .expect("valid frame");
            if let Message::Text(text) = message {
                let event: Value = serde_json::from_str(&text).unwrap();
                if event["type"] == expected {
                    break;
                }
                assert_eq!(event["type"], "response.metadata");
            } else {
                assert!(matches!(message, Message::Ping(_)));
            }
        }
    })
    .await
    .expect("receive response after upload");
}

#[tokio::test(start_paused = true)]
async fn slow_unfragmented_uploads_survive_heartbeats_on_first_and_later_requests() {
    let (mut socket, trace) = connect().await;
    let input = "x".repeat(320 * 1024);
    for round in 1..=2 {
        let started = Instant::now();
        socket.get_mut().slow = true;
        // 对齐 Codex 的发送语义：整个 send 完成前不读 Ping，也不主动发送 Pong
        tokio::time::timeout(
            Duration::from_secs(100),
            socket.send(Message::Text(
                json!({"type": "response.create", "model": "model-a", "input": input})
                    .to_string()
                    .into(),
            )),
        )
        .await
        .expect("upload completes")
        .expect("heartbeat must not interrupt an active upload");
        assert!(started.elapsed() >= Duration::from_secs(80));
        socket.get_mut().slow = false;
        receive_event(&mut socket, "response.created").await;
        assert_eq!(trace.starts.load(Ordering::Acquire), round);
        trace.release_terminal.notify_one();
        receive_event(&mut socket, "response.completed").await;
    }
    assert_eq!(
        *trace.inputs.lock().unwrap(),
        vec![json!(input), json!(input)]
    );
    socket.close(None).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn stalled_partial_frame_without_pongs_still_releases_the_connection() {
    let (mut socket, trace) = connect().await;
    // 已掩码的文本帧声明 4096 字节，只交付一个字节，之后不再收发
    socket
        .get_mut()
        .write_all(&[0x81, 0xfe, 0x10, 0x00, 0, 0, 0, 0, b'x'])
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(56)).await;
    let error = socket
        .get_mut()
        .write_all(b"x")
        .await
        .expect_err("idle partial frame must be closed");
    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    assert_eq!(trace.starts.load(Ordering::Acquire), 0);
}
