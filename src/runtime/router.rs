use std::time::Duration;

use axum::{
    http::{Request, Response},
    middleware::from_fn,
    routing::get,
    Router,
};
use tower_http::{
    classify::ServerErrorsFailureClass,
    trace::{HttpMakeClassifier, MakeSpan, OnFailure, OnRequest, OnResponse, TraceLayer},
};
use tracing::Span;

use crate::{
    admin::api as admin_api,
    codex::serving::http::{
        diagnostics::{debug_fingerprint, debug_upstream, diagnostics},
        router as serving_http,
    },
    platform::http::{
        health::health,
        request_id::{attach_request_id, RequestId},
    },
};

use super::state::AppState;

pub type HttpTraceLayer = TraceLayer<
    HttpMakeClassifier,
    HttpMakeSpan,
    HttpOnRequest,
    HttpOnResponse,
    (),
    (),
    HttpOnFailure,
>;

#[derive(Debug, Clone, Copy)]
pub struct HttpMakeSpan;

#[derive(Debug, Clone, Copy)]
pub struct HttpOnRequest;

#[derive(Debug, Clone, Copy)]
pub struct HttpOnResponse;

#[derive(Debug, Clone, Copy)]
pub struct HttpOnFailure;

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/debug/diagnostics", get(diagnostics))
        .route("/debug/fingerprint", get(debug_fingerprint))
        .route("/debug/upstream", get(debug_upstream))
        .merge(serving_http::router())
        .merge(admin_api::router())
        .with_state(state)
        .layer(http_trace_layer())
        .layer(from_fn(attach_request_id))
}

pub fn http_trace_layer() -> HttpTraceLayer {
    TraceLayer::new_for_http()
        .make_span_with(HttpMakeSpan)
        .on_request(HttpOnRequest)
        .on_response(HttpOnResponse)
        .on_body_chunk(())
        .on_eos(())
        .on_failure(HttpOnFailure)
}

impl<B> MakeSpan<B> for HttpMakeSpan {
    fn make_span(&mut self, request: &Request<B>) -> Span {
        let request_id = request
            .extensions()
            .get::<RequestId>()
            .map(RequestId::as_str)
            .unwrap_or("missing");

        tracing::info_span!(
            "http_request",
            request_id = %request_id,
            method = %request.method(),
            uri = %request.uri(),
            status = tracing::field::Empty,
            latency_ms = tracing::field::Empty,
        )
    }
}

impl<B> OnRequest<B> for HttpOnRequest {
    fn on_request(&mut self, _request: &Request<B>, _span: &Span) {
        tracing::info!("收到 HTTP 请求");
    }
}

impl<B> OnResponse<B> for HttpOnResponse {
    fn on_response(self, response: &Response<B>, latency: Duration, span: &Span) {
        let status = response.status().as_u16();
        let latency_ms = latency.as_millis() as u64;
        span.record("status", status);
        span.record("latency_ms", latency_ms);
        tracing::info!(status, latency_ms, "HTTP 请求完成");
    }
}

impl OnFailure<ServerErrorsFailureClass> for HttpOnFailure {
    fn on_failure(
        &mut self,
        failure_class: ServerErrorsFailureClass,
        latency: Duration,
        span: &Span,
    ) {
        let latency_ms = latency.as_millis() as u64;
        span.record("latency_ms", latency_ms);
        tracing::warn!(error = %failure_class, latency_ms, "HTTP 请求失败");
    }
}
