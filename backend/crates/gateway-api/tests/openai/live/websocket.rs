//! 通过真实 WebSocket 验证 sideband 阻塞写入、关闭与生命周期释放

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use futures::{SinkExt, future::BoxFuture};
use gateway_core::lifecycle::{
    CancellationToken, ConnectionDraining, ConnectionGuard, ConnectionLifecycle,
};
use gateway_core::live::{
    LiveCallOutcome, LiveClose, LiveFrame, LiveGateway, LiveGatewayError, LiveHangupRequest,
    LiveRelay, LiveRelayGuard, LiveRelaySink, LiveRelayStream, LiveSidebandRequest,
};
use tokio::sync::Notify;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

use super::{LIVE_KEY, LiveCapture};

#[derive(Default)]
struct Trace {
    cancellation: CancellationToken,
    connections: AtomicUsize,
    claims: AtomicUsize,
    entered: Notify,
    released: Notify,
    block_close: bool,
}

struct Guard(Arc<Trace>);
impl ConnectionGuard for Guard {}
impl Drop for Guard {
    fn drop(&mut self) {
        self.0.connections.fetch_sub(1, Ordering::SeqCst);
        self.0.released.notify_one();
    }
}
struct Claim(Arc<Trace>);
impl LiveRelayGuard for Claim {}
impl Drop for Claim {
    fn drop(&mut self) {
        self.0.claims.fetch_sub(1, Ordering::SeqCst);
    }
}

struct Lifecycle(Arc<Trace>);
impl ConnectionLifecycle for Lifecycle {
    fn try_register(&self) -> Result<Box<dyn ConnectionGuard>, ConnectionDraining> {
        self.0.connections.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Guard(self.0.clone())))
    }
    fn cancellation(&self) -> CancellationToken {
        self.0.cancellation.clone()
    }
    fn is_draining(&self) -> bool {
        self.0.cancellation.is_cancelled()
    }
}

struct PendingStream;
impl LiveRelayStream for PendingStream {
    fn next_frame(&mut self) -> BoxFuture<'_, Option<LiveFrame>> {
        Box::pin(std::future::pending())
    }
}
struct PendingSink(Arc<Trace>);
impl LiveRelaySink for PendingSink {
    fn send_frame(&mut self, _: LiveFrame) -> BoxFuture<'_, Result<(), LiveGatewayError>> {
        Box::pin(async move {
            self.0.entered.notify_one();
            std::future::pending().await
        })
    }
    fn close(&mut self, _: Option<LiveClose>) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            if self.0.block_close {
                self.0.entered.notify_one();
                std::future::pending().await
            }
        })
    }
}
struct Gateway(Arc<Trace>);
impl LiveGateway for Gateway {
    fn open_sideband<'a>(
        &'a self,
        request: LiveSidebandRequest<'a>,
    ) -> BoxFuture<'a, Result<LiveRelay, LiveGatewayError>> {
        Box::pin(async move {
            assert!(
                request.account_scope.allows(
                    &gateway_core::account::ProviderAccountId::new("acct_api_test").unwrap()
                )
            );
            self.0.claims.fetch_add(1, Ordering::SeqCst);
            let mut relay = LiveRelay::new(
                None,
                Box::new(PendingStream),
                Box::new(PendingSink(self.0.clone())),
            );
            relay.with_guard(Box::new(Claim(self.0.clone())));
            Ok(relay)
        })
    }
    fn hangup<'a>(
        &'a self,
        _: LiveHangupRequest<'a>,
    ) -> BoxFuture<'a, Result<LiveCallOutcome, LiveGatewayError>> {
        unreachable!("no hangup in relay lifecycle test")
    }
}

async fn blocked_relay(
    trace: &Arc<Trace>,
) -> (
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tokio::task::JoinHandle<()>,
) {
    let admin = crate::admin::AdminTestFixture::new().await;
    let execution = Arc::new(LiveCapture {
        gateway: Some(Arc::new(Gateway(trace.clone()))),
        ..LiveCapture::default()
    });
    let router = gateway_api::initialize(
        gateway_api::ApiConfig {
            asset_directory: std::env::temp_dir(),
            cors_allowed_origins: Vec::new(),
            request_timeout_seconds: None,
            request_id_header: "x-request-id".to_owned(),
        },
        execution,
        admin.services,
        Vec::new(),
        Arc::new(crate::openai::EmptyWorkerHealth),
        Arc::new(Lifecycle(trace.clone())),
    )
    .unwrap()
    .router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let mut request = format!("ws://{address}/v1/live/call_regression")
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {LIVE_KEY}").parse().unwrap(),
    );
    let (mut socket, _) = connect_async(request).await.unwrap();
    if trace.block_close {
        socket.send(Message::Close(None)).await.unwrap();
    } else {
        socket.send(Message::Text("test".into())).await.unwrap();
    }
    tokio::time::timeout(Duration::from_secs(2), trace.entered.notified())
        .await
        .expect("relay entered blocked sink");
    (socket, server)
}

async fn assert_released(trace: &Trace) {
    tokio::time::timeout(Duration::from_secs(2), trace.released.notified())
        .await
        .expect("connection released promptly");
    assert_eq!(trace.connections.load(Ordering::SeqCst), 0);
    assert_eq!(trace.claims.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn live_shutdown_cancels_blocked_send_and_releases_both_guards() {
    let trace = Arc::new(Trace::default());
    let (_socket, server) = blocked_relay(&trace).await;
    trace.cancellation.cancel();
    assert_released(&trace).await;
    server.abort();
}

#[tokio::test]
async fn live_shutdown_cancels_blocked_close_and_releases_both_guards() {
    let trace = Arc::new(Trace {
        block_close: true,
        ..Trace::default()
    });
    let (_socket, server) = blocked_relay(&trace).await;
    trace.cancellation.cancel();
    assert_released(&trace).await;
    server.abort();
}

#[tokio::test]
async fn live_close_deadline_releases_both_guards_without_shutdown() {
    let trace = Arc::new(Trace {
        block_close: true,
        ..Trace::default()
    });
    let (_socket, server) = blocked_relay(&trace).await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(6)).await;
    assert_released(&trace).await;
    server.abort();
}
