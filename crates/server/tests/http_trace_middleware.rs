use std::{
    io::{self, Write},
    sync::{Arc, Mutex},
};

use axum::{
    body::Body,
    http::{Request, StatusCode},
    middleware::from_fn,
    routing::get,
    Router,
};
use codex_proxy_server::middleware::{request_id::attach_request_id, trace::http_trace_layer};
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

#[tokio::test]
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

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let output = logs.content();
    assert!(
        output.contains("req_trace")
            && output.contains("received HTTP request")
            && output.contains("completed HTTP request")
            && output.contains("\"status\":204")
            && output.contains("latency_ms"),
        "unexpected trace output: {output}"
    );
}
