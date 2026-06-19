//! tracing 中间件。

use std::time::Duration;

use axum::{
    body::Body,
    http::{Request, Response},
};
use tower_http::{
    classify::{ServerErrorsAsFailures, ServerErrorsFailureClass, SharedClassifier},
    trace::TraceLayer,
};
use tracing::{info, warn, Span};

use crate::middleware::request_id::RequestId;

/// 默认 tracing 事件名。
pub const TRACE_LAYER_NAME: &str = "http";

/// HTTP tracing layer type used by the server router.
pub type HttpTraceLayer = TraceLayer<
    SharedClassifier<ServerErrorsAsFailures>,
    fn(&Request<Body>) -> Span,
    fn(&Request<Body>, &Span),
    fn(&Response<Body>, Duration, &Span),
    tower_http::trace::DefaultOnBodyChunk,
    tower_http::trace::DefaultOnEos,
    fn(ServerErrorsFailureClass, Duration, &Span),
>;

/// 构造 HTTP tracing layer。
pub fn http_trace_layer() -> HttpTraceLayer {
    TraceLayer::new_for_http()
        .make_span_with(make_http_span as fn(&Request<Body>) -> Span)
        .on_request(on_http_request as fn(&Request<Body>, &Span))
        .on_response(on_http_response as fn(&Response<Body>, Duration, &Span))
        .on_failure(on_http_failure as fn(ServerErrorsFailureClass, Duration, &Span))
}

fn make_http_span(request: &Request<Body>) -> Span {
    let request_id = request_id(request);
    tracing::info_span!(
        "http",
        request_id = request_id.as_deref(),
        method = %request.method(),
        uri = %request.uri()
    )
}

fn on_http_request(request: &Request<Body>, _span: &Span) {
    let request_id = request_id(request);
    info!(
        request_id = request_id.as_deref(),
        method = %request.method(),
        uri = %request.uri(),
        "received HTTP request"
    );
}

fn on_http_response(response: &Response<Body>, latency: Duration, _span: &Span) {
    info!(
        status = response.status().as_u16(),
        latency_ms = latency.as_millis(),
        "completed HTTP request"
    );
}

fn on_http_failure(failure_class: ServerErrorsFailureClass, latency: Duration, _span: &Span) {
    warn!(
        failure = %failure_class,
        latency_ms = latency.as_millis(),
        "failed HTTP request"
    );
}

fn request_id(request: &Request<Body>) -> Option<String> {
    request
        .extensions()
        .get::<RequestId>()
        .map(|request_id| request_id.as_str().to_string())
}
