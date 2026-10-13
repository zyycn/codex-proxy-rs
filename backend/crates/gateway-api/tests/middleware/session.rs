//! 验证升级后终态只记录一次安全分类，正常结束及已报告失败不重复输出

use super::*;
use tracing::{
    Event, Metadata, Subscriber,
    span::{Attributes, Id, Record},
};

#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<String>>>);

impl Subscriber for Recorder {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }
    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
    fn event(&self, event: &Event<'_>) {
        struct Fields(String);
        impl tracing::field::Visit for Fields {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                use std::fmt::Write as _;
                write!(&mut self.0, "{}={value:?} ", field.name()).unwrap();
            }
        }
        let mut fields = Fields(String::new());
        event.record(&mut fields);
        if fields.0.contains("middleware.session.failed") {
            self.0.lock().unwrap().push(fields.0);
        }
    }
}

#[tokio::test]
async fn upgraded_session_records_one_correlated_failure_and_ignores_expected_endings() {
    use tokio_tungstenite::{connect_async, tungstenite::client::IntoClientRequest as _};
    for kind in [
        None,
        Some("cancelled"),
        Some("reported"),
        Some("permission_denied"),
    ] {
        let recorder = Recorder::default();
        let _subscriber = tracing::subscriber::set_default(recorder.clone());
        let (router, plan) = router(Mode::SessionEnd { kind }, None).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let mut request = format!("ws://{address}/plugin-owned/session")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("x-request-id", "session-regression".parse().unwrap());
        let (_socket, response) = connect_async(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
        let cancellation = plan.cancellation.lock().unwrap().clone().unwrap();
        tokio::time::timeout(Duration::from_secs(3), cancellation.cancelled())
            .await
            .unwrap();
        let events = recorder.0.lock().unwrap();
        if kind == Some("permission_denied") {
            assert_eq!(events.len(), 1, "{events:?}");
            assert!(events[0].contains("session-regression"));
            assert!(events[0].contains("plugin-fixture"));
            assert!(events[0].contains("permission_denied"));
        } else {
            assert!(events.is_empty(), "{events:?}");
        }
        server.abort();
    }
}
