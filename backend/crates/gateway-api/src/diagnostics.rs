//! 数据面 HTTP 入口诊断；执行前失败进入运维事件，不伪造模型请求或上游状态

use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use axum::{
    body::{Body, Bytes},
    extract::{MatchedPath, Request, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
};
use gateway_core::diagnostics::{OperationalDiagnostics, OperationalFailure, TraceContext};
use http_body::{Body as _, Frame, SizeHint};
use serde_json::json;
use tower_http::request_id::RequestId;

use crate::client_error::{AccessRejection, ProtocolError, ProtocolErrorBody};

pub(crate) async fn record_ingress(
    State(diagnostics): State<Arc<dyn OperationalDiagnostics>>,
    mut request: Request,
    next: Next,
) -> Response {
    if request.uri().path() != "/v1" && !request.uri().path().starts_with("/v1/") {
        return next.run(request).await;
    }
    let correlation_id = request
        .extensions()
        .get::<RequestId>()
        .and_then(|id| id.header_value().to_str().ok())
        .map(str::to_owned);
    let endpoint = request
        .extensions()
        .get::<MatchedPath>()
        .map(|path| path.as_str().to_owned());
    let method = request.method().clone();
    let dump = if tracing::enabled!(target: "request_dump", tracing::Level::INFO) {
        let trace = TraceContext::new(correlation_id.as_deref().unwrap_or("ingress"));
        trace.dump(
            "ingress.request",
            &serde_json::to_vec(&json!({
                "method":method.as_str(), "uri":request.uri().to_string(),
            }))
            .unwrap_or_default(),
        );
        trace.headers(
            "ingress.request.headers",
            json!({}),
            request
                .headers()
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_bytes())),
        );
        let body = std::mem::replace(request.body_mut(), Body::empty());
        *request.body_mut() = Body::new(DumpBody::new(body, trace.clone(), "ingress.request.body"));
        Some(trace)
    } else {
        None
    };
    let mut response = if endpoint.is_none() {
        // 数据面未知路径不能落入 SPA fallback，否则会把协议错误伪装成 HTML 成功
        crate::openai::error::protocol_error_response(
            StatusCode::NOT_FOUND,
            ProtocolErrorBody {
                error: ProtocolError {
                    kind: "invalid_request_error",
                    code: "unknown_endpoint",
                    message: "The requested API endpoint does not exist.".into(),
                    param: None,
                },
            },
        )
    } else {
        next.run(request).await
    };
    let protocol = response.extensions_mut().remove::<ProtocolErrorBody>();
    let gateway = response
        .extensions_mut()
        .remove::<gateway_core::error::GatewayError>();
    let access = response.extensions_mut().remove::<AccessRejection>();
    let execution_observed = response
        .extensions_mut()
        .remove::<crate::client_error::ExecutionObserved>()
        .is_some();
    let status = response.status();
    if status.is_client_error() || status.is_server_error() {
        let code = access
            .map(|error| error.0)
            .or_else(|| protocol.as_ref().map(|body| body.error.code))
            .or_else(|| gateway.as_ref().map(|error| error.kind().as_str()))
            .unwrap_or(match status {
                StatusCode::UNAUTHORIZED => "invalid_api_key",
                StatusCode::METHOD_NOT_ALLOWED => "method_not_allowed",
                StatusCode::NOT_FOUND => "not_found",
                _ => "http_rejection",
            });
        // 未知参数名可由客户端任意构造，只保留 decoder 生成的已知参数路径
        let parameter = protocol
            .as_ref()
            .filter(|body| body.error.code != "unknown_parameter")
            .and_then(|body| body.error.param.as_deref())
            .filter(|path| {
                path.len() <= 256
                    && path.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'[' | b']')
                    })
            });
        let endpoint = endpoint.as_deref().unwrap_or("/v1/<unmatched>");
        let model_request_id = execution_observed
            .then(|| {
                response
                    .headers()
                    .get("x-gateway-request-id")
                    .and_then(|id| id.to_str().ok())
            })
            .flatten();
        tracing::info!(target: "request_trace", stage="ingress.rejected", status=status.as_u16(),
            code, parameter, endpoint, model_request_id, method=method.as_str(), "Data-plane HTTP request rejected");
        if !execution_observed {
            let mut failure = OperationalFailure::new(
                "api",
                "http_ingress",
                code,
                json!({"method":method.as_str(), "endpoint":endpoint, "status":status.as_u16(),
                    "code":code, "param":parameter})
                .to_string(),
            );
            failure.correlation_id = correlation_id;
            failure.details = gateway.as_ref().and_then(|error| error.error_details());
            if diagnostics.record_failure(failure).await.is_err() {
                tracing::warn!("HTTP ingress diagnostic could not be recorded");
            }
        }
    }
    if let Some(trace) = dump {
        trace.dump(
            "ingress.response",
            &serde_json::to_vec(&json!({"status":status.as_u16()})).unwrap_or_default(),
        );
        trace.headers(
            "ingress.response.headers",
            json!({"status":status.as_u16()}),
            response
                .headers()
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_bytes())),
        );
        let body = std::mem::replace(response.body_mut(), Body::empty());
        *response.body_mut() = Body::new(DumpBody::new(body, trace, "ingress.response.body"));
    }
    response
}

struct DumpBody {
    inner: Body,
    trace: TraceContext,
    stage: &'static str,
    complete: bool,
}

impl DumpBody {
    fn new(inner: Body, trace: TraceContext, stage: &'static str) -> Self {
        let complete = inner.is_end_stream();
        Self {
            inner,
            trace,
            stage,
            complete,
        }
    }
}

impl http_body::Body for DumpBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        let result = Pin::new(&mut self.inner).poll_frame(cx);
        if let Poll::Ready(Some(Ok(frame))) = &result
            && let Some(bytes) = frame.data_ref()
        {
            self.trace.dump(self.stage, bytes);
        }
        if matches!(result, Poll::Ready(Some(Err(_)))) {
            self.complete = false;
        } else if matches!(result, Poll::Ready(None)) || self.inner.is_end_stream() {
            self.complete = true;
        }
        result
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for DumpBody {
    fn drop(&mut self) {
        // 不为诊断主动读取剩余正文；取消或未消费的请求必须明确标出转储不完整
        self.trace.dump(
            "ingress.body.finished",
            &serde_json::to_vec(&json!({
                "stage":self.stage, "complete":self.complete,
            }))
            .unwrap_or_default(),
        );
    }
}
