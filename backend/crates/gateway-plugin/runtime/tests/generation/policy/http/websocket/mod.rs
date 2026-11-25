//! 验证真实插件 WebSocket 升级、双向大帧与会话资源生命周期

mod contract;
use super::*;
use futures::FutureExt as _;
use gateway_core::engine::middleware::{http::upgrade, websocket as ws};
use std::sync::atomic::AtomicBool;

struct Transport {
    incoming: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<ws::Message>>,
    outgoing: tokio::sync::mpsc::Sender<ws::Message>,
    dropped: Arc<AtomicBool>,
}
impl Drop for Transport {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}
impl ws::Sender for Transport {
    fn send(&self, message: ws::Message) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        Box::pin(async {
            self.outgoing
                .send(message)
                .await
                .map_err(|_| MiddlewareError::Fault)
        })
    }
}
impl ws::Session for Transport {
    fn receive(&self) -> BoxFuture<'_, Result<Option<ws::Message>, MiddlewareError>> {
        Box::pin(async { Ok(self.incoming.lock().await.recv().await) })
    }
}
struct Upgrade(Arc<Transport>);
impl upgrade::WebSocketUpgrade for Upgrade {
    fn accept(
        &self,
        parts: ::http::request::Parts,
        protocols: Vec<String>,
    ) -> BoxFuture<'static, Result<(core::Response, upgrade::PendingSession), MiddlewareError>>
    {
        assert_eq!(parts.uri, "/custom/session");
        assert_eq!(protocols, ["test-session"]);
        let session = self.0.clone();
        Box::pin(async move {
            let mut response = core::Response::new(core::empty_body());
            *response.status_mut() = ::http::StatusCode::SWITCHING_PROTOCOLS;
            Ok((
                response,
                async move { Ok(session as Arc<dyn ws::Session>) }
                    .boxed()
                    .shared(),
            ))
        })
    }
}
