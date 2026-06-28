use std::{
    io::{self, Write},
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use axum::{
    body::{to_bytes, Body},
    extract::connect_info::ConnectInfo,
    http::HeaderMap,
    http::{Request, StatusCode},
    middleware::from_fn,
    routing::get,
    Router,
};
use codex_proxy_rs::http::middleware::{request_id::attach_request_id, trace::http_trace_layer};
use tokio::time::{sleep, Duration};
use tower::ServiceExt;
use tracing_subscriber::fmt::MakeWriter;

#[derive(Clone, Default)]
struct SharedLogBuffer {
    inner: Arc<Mutex<Vec<u8>>>,
}

impl SharedLogBuffer {
    fn content(&self) -> String {
        let bytes = self.inner.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
    }
}

impl<'a> MakeWriter<'a> for SharedLogBuffer {
    type Writer = SharedLogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        SharedLogWriter {
            inner: Arc::clone(&self.inner),
        }
    }
}

struct SharedLogWriter {
    inner: Arc<Mutex<Vec<u8>>>,
}

impl Write for SharedLogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

async fn wait_for_trace_output(logs: &SharedLogBuffer) -> String {
    for _ in 0..20 {
        let output = logs.content();
        if output.contains("completed HTTP request") {
            return output;
        }
        sleep(Duration::from_millis(5)).await;
    }
    logs.content()
}

#[tokio::test(flavor = "current_thread")]
async fn http_trace_should_include_request_id_and_completion_fields() {
    let logs = SharedLogBuffer::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_writer(logs.clone())
        .with_current_span(true)
        .with_span_list(true)
        .with_ansi(false)
        .finish();

    let app = Router::new()
        .route("/trace-test", get(|| async { StatusCode::NO_CONTENT }))
        .layer(http_trace_layer())
        .layer(from_fn(attach_request_id));

    let _guard = tracing::subscriber::set_default(subscriber);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/trace-test")
                .header("x-request-id", "req_trace")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let status = response.status();
    let _body = to_bytes(response.into_body(), 1024).await.unwrap();

    assert_eq!(status, StatusCode::NO_CONTENT);
    let output = wait_for_trace_output(&logs).await;
    assert!(
        output.contains("req_trace")
            && output.contains("received HTTP request")
            && output.contains("completed HTTP request")
            && output.contains("\"status\":204")
            && output.contains("latency_ms"),
        "unexpected trace output: {output}"
    );
}

#[tokio::test]
async fn request_context_should_attach_real_ip_from_connection() {
    let app = Router::new()
        .route(
            "/ip",
            get(|headers: HeaderMap| async move {
                headers
                    .get("x-real-ip")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_string()
            }),
        )
        .layer(from_fn(attach_request_id));
    let mut request = Request::builder()
        .uri("/ip")
        .body(Body::empty())
        .expect("request should build");
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 61234))));

    let response = app.oneshot(request).await.expect("response");
    let bytes = to_bytes(response.into_body(), 1024).await.unwrap();

    assert_eq!(std::str::from_utf8(&bytes).unwrap(), "127.0.0.1");
}

#[tokio::test]
async fn request_context_should_not_override_forwarded_ip_headers() {
    let app = Router::new()
        .route(
            "/ip",
            get(|headers: HeaderMap| async move {
                headers
                    .get("x-real-ip")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or("missing")
                    .to_string()
            }),
        )
        .layer(from_fn(attach_request_id));
    let mut request = Request::builder()
        .uri("/ip")
        .header("x-forwarded-for", "203.0.113.42, 10.0.0.2")
        .body(Body::empty())
        .expect("request should build");
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 61234))));

    let response = app.oneshot(request).await.expect("response");
    let bytes = to_bytes(response.into_body(), 1024).await.unwrap();

    assert_eq!(std::str::from_utf8(&bytes).unwrap(), "missing");
}
