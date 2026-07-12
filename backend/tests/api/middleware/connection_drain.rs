use std::{convert::Infallible, future::pending, time::Duration};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
    middleware,
    response::Response,
    routing::get,
};
use bytes::Bytes;
use codex_proxy_rs::api::middleware::connection_drain::{ConnectionDrain, drain_response_body};
use tokio::sync::oneshot;
use tower::ServiceExt;

#[tokio::test]
async fn connection_drain_should_end_streaming_response_bodies() {
    let connection_drain = ConnectionDrain::default();
    let app = Router::new()
        .route(
            "/stream",
            get(|| async {
                let stream = futures::stream::pending::<Result<Bytes, Infallible>>();
                Response::new(Body::from_stream(stream))
            }),
        )
        .layer(middleware::from_fn_with_state(
            connection_drain.clone(),
            drain_response_body,
        ));
    let response = app
        .oneshot(Request::get("/stream").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let body = tokio::spawn(to_bytes(response.into_body(), 1024));

    tokio::task::yield_now().await;
    assert!(!body.is_finished());

    connection_drain.begin_shutdown();
    let bytes = tokio::time::timeout(Duration::from_secs(1), body)
        .await
        .expect("streaming body should end when connection draining starts")
        .unwrap()
        .unwrap();

    assert!(bytes.is_empty());
}

#[tokio::test]
async fn connection_drain_should_cancel_tracked_connection_tasks() {
    let connection_drain = ConnectionDrain::default();
    let (dropped_tx, dropped_rx) = oneshot::channel();
    connection_drain.spawn(async move {
        let _drop_notification = DropNotification(Some(dropped_tx));
        pending::<()>().await;
    });

    tokio::task::yield_now().await;
    assert_eq!(connection_drain.begin_shutdown(), 1);
    tokio::time::timeout(Duration::from_secs(1), connection_drain.wait())
        .await
        .expect("tracked connection should stop after shutdown");
    tokio::time::timeout(Duration::from_secs(1), dropped_rx)
        .await
        .expect("tracked connection future should be dropped")
        .unwrap();

    assert!(connection_drain.is_shutting_down());
}

struct DropNotification(Option<oneshot::Sender<()>>);

impl Drop for DropNotification {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}
