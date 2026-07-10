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
    Extension, Router,
};
use codex_proxy_rs::api::middleware::{
    request_id::{attach_request_id, ClientIp},
    trace::http_trace_layer,
};
use serde_json::Value;
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
        .with_max_level(tracing::Level::DEBUG)
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
    let received = json_events(&output)
        .into_iter()
        .find(|event| event["fields"]["message"] == "received HTTP request")
        .expect("received event should exist");
    assert!(received["fields"].get("request_id").is_none());
    assert!(received["fields"].get("method").is_none());
    assert!(received["fields"].get("uri").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn http_trace_should_include_resolved_client_ip_in_span() {
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
    let mut request = Request::builder()
        .uri("/trace-test")
        .header("cf-connecting-ip", "203.0.113.43")
        .body(Body::empty())
        .unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 61234))));

    let _guard = tracing::subscriber::set_default(subscriber);
    let response = app.oneshot(request).await.unwrap();
    let _body = to_bytes(response.into_body(), 1024).await.unwrap();

    let output = wait_for_trace_output(&logs).await;
    assert!(
        output.contains("\"client_ip\":\"203.0.113.43\""),
        "unexpected trace output: {output}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn http_trace_should_emit_one_terminal_event_for_server_error() {
    let logs = SharedLogBuffer::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_writer(logs.clone())
        .with_current_span(true)
        .with_span_list(true)
        .with_ansi(false)
        .finish();
    let app = Router::new()
        .route(
            "/trace-test",
            get(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
        )
        .layer(http_trace_layer())
        .layer(from_fn(attach_request_id));

    let _guard = tracing::subscriber::set_default(subscriber);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/trace-test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let _body = to_bytes(response.into_body(), 1024).await.unwrap();
    sleep(Duration::from_millis(10)).await;

    let output = logs.content();
    assert_eq!(output.matches("completed HTTP request").count(), 0);
    assert_eq!(output.matches("failed HTTP request").count(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn health_route_outside_trace_layer_should_not_emit_http_events() {
    let logs = SharedLogBuffer::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_writer(logs.clone())
        .with_current_span(true)
        .with_span_list(true)
        .with_ansi(false)
        .finish();
    let traced_routes = Router::new()
        .route("/trace-test", get(|| async { StatusCode::NO_CONTENT }))
        .layer(http_trace_layer());
    let app = Router::new()
        .route("/healthz", get(|| async { StatusCode::NO_CONTENT }))
        .merge(traced_routes)
        .layer(from_fn(attach_request_id));

    let _guard = tracing::subscriber::set_default(subscriber);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let _body = to_bytes(response.into_body(), 1024).await.unwrap();
    sleep(Duration::from_millis(10)).await;

    assert!(logs.content().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn http_trace_should_redact_request_uri_query_values() {
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
                .uri("/trace-test?token=secret-token&code=secret-code")
                .header("x-request-id", "req_trace_query")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let _body = to_bytes(response.into_body(), 1024).await.unwrap();

    let output = wait_for_trace_output(&logs).await;
    assert!(
        output.contains("/trace-test?token=<redacted>&code=<redacted>")
            && !output.contains("secret-token")
            && !output.contains("secret-code")
            && output.contains("token=<redacted>")
            && output.contains("code=<redacted>"),
        "unexpected trace output: {output}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn http_trace_should_redact_bare_query_segments() {
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
                .uri("/trace-test?bare-secret&empty=&page=2&page=3")
                .header("x-request-id", "req_trace_query_bare")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let _body = to_bytes(response.into_body(), 1024).await.unwrap();

    let output = wait_for_trace_output(&logs).await;
    assert!(
        output.contains("/trace-test?<redacted>&empty=<redacted>&page=<redacted>&page=<redacted>")
            && !output.contains("bare-secret")
            && !output.contains("page=2")
            && !output.contains("page=3"),
        "unexpected trace output: {output}"
    );
}

#[tokio::test]
async fn request_context_should_generate_new_request_id_for_invalid_header() {
    let app = Router::new()
        .route("/request-id", get(|| async { StatusCode::NO_CONTENT }))
        .layer(from_fn(attach_request_id));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/request-id")
                .header("x-request-id", "req invalid")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();

    assert!(
        request_id.starts_with("req_") && request_id != "req invalid",
        "unexpected request id: {request_id}"
    );
}

fn json_events(output: &str) -> Vec<Value> {
    output
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

#[tokio::test]
async fn request_context_should_attach_client_ip_from_connection() {
    let app = Router::new()
        .route(
            "/ip",
            get(|Extension(client_ip): Extension<ClientIp>| async move {
                client_ip.as_str().to_string()
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
async fn request_context_should_auto_detect_forwarded_ip_without_trusted_proxy_config() {
    let app = Router::new()
        .route(
            "/ip",
            get(
                |Extension(client_ip): Extension<ClientIp>, headers: HeaderMap| async move {
                    let cf_ip = headers
                        .get("cf-connecting-ip")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("missing");
                    let forwarded_for = headers
                        .get("x-forwarded-for")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("missing");
                    let real_ip = headers
                        .get("x-real-ip")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("missing");
                    format!(
                        "{}|cf={cf_ip}|xff={forwarded_for}|real={real_ip}",
                        client_ip.as_str()
                    )
                },
            ),
        )
        .layer(from_fn(attach_request_id));
    let mut request = Request::builder()
        .uri("/ip")
        .header("x-forwarded-for", "203.0.113.42, 10.0.0.2")
        .header("cf-connecting-ip", "203.0.113.43")
        .header("x-real-ip", "203.0.113.44")
        .body(Body::empty())
        .expect("request should build");
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 61234))));

    let response = app.oneshot(request).await.expect("response");
    let bytes = to_bytes(response.into_body(), 1024).await.unwrap();

    // CF-Connecting-IP 优先于 X-Real-IP / X-Forwarded-For。
    assert_eq!(
        std::str::from_utf8(&bytes).unwrap(),
        "203.0.113.43|cf=203.0.113.43|xff=203.0.113.42, 10.0.0.2|real=203.0.113.44"
    );
}

#[tokio::test]
async fn request_context_should_skip_private_forwarded_hops_in_auto_mode() {
    let app = Router::new()
        .route(
            "/ip",
            get(|Extension(client_ip): Extension<ClientIp>| async move {
                client_ip.as_str().to_string()
            }),
        )
        .layer(from_fn(attach_request_id));
    let mut request = Request::builder()
        .uri("/ip")
        .header("x-forwarded-for", "10.0.0.2, 172.16.0.3, 203.0.113.42")
        .body(Body::empty())
        .expect("request should build");
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 61234))));

    let response = app.oneshot(request).await.expect("response");
    let bytes = to_bytes(response.into_body(), 1024).await.unwrap();

    // 自动模式下应跳过 X-Forwarded-For 链中的内网跳板，取首个公网 IP。
    assert_eq!(std::str::from_utf8(&bytes).unwrap(), "203.0.113.42");
}

#[tokio::test]
async fn request_context_should_accept_real_ip_when_forwarded_for_missing() {
    let app = Router::new()
        .route(
            "/ip",
            get(|Extension(client_ip): Extension<ClientIp>| async move {
                client_ip.as_str().to_string()
            }),
        )
        .layer(from_fn(attach_request_id));
    let mut request = Request::builder()
        .uri("/ip")
        .header("x-real-ip", "198.51.100.23")
        .body(Body::empty())
        .expect("request should build");
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 61234))));

    let response = app.oneshot(request).await.expect("response");
    let bytes = to_bytes(response.into_body(), 1024).await.unwrap();

    assert_eq!(std::str::from_utf8(&bytes).unwrap(), "198.51.100.23");
}

#[tokio::test]
async fn request_context_should_prefer_real_ip_over_forwarded_for() {
    let app = Router::new()
        .route(
            "/ip",
            get(|Extension(client_ip): Extension<ClientIp>| async move {
                client_ip.as_str().to_string()
            }),
        )
        .layer(from_fn(attach_request_id));
    let mut request = Request::builder()
        .uri("/ip")
        .header("x-forwarded-for", "198.51.100.99, 203.0.113.42")
        .header("x-real-ip", "203.0.113.42")
        .body(Body::empty())
        .expect("request should build");
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 61234))));

    let response = app.oneshot(request).await.expect("response");
    let bytes = to_bytes(response.into_body(), 1024).await.unwrap();

    assert_eq!(std::str::from_utf8(&bytes).unwrap(), "203.0.113.42");
}

#[tokio::test]
async fn request_context_should_not_inject_real_ip_header() {
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
        .header("cf-connecting-ip", "203.0.113.43")
        .body(Body::empty())
        .expect("request should build");
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 61234))));

    let response = app.oneshot(request).await.expect("response");
    let bytes = to_bytes(response.into_body(), 1024).await.unwrap();

    assert_eq!(std::str::from_utf8(&bytes).unwrap(), "missing");
}
