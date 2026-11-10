//! Messages HTTP 请求复用账号代理、出站中间件和诊断，不携带 Codex 会话身份

use std::time::Instant;

use bytes::Bytes;
use futures::StreamExt;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue};

use super::{
    CodexBackendClient, CodexBackendSseStream, CodexClientError, CodexClientResult,
    CodexRequestContext, CodexTransportMetrics, CodexUpstreamDiagnostics,
    client::{elapsed_duration_millis, http_json_upstream_error, http_version_name},
    response_meta::{self, CodexResponseMetadata},
};

pub(crate) struct MessagesHttpResponse {
    pub body: CodexBackendSseStream,
    pub streaming: bool,
    pub diagnostics: CodexUpstreamDiagnostics,
    pub response_metadata: CodexResponseMetadata,
    pub transport_metrics: CodexTransportMetrics,
}

impl CodexBackendClient {
    pub(crate) async fn post_messages(
        &self,
        endpoint: &'static str,
        body: Bytes,
        passthrough_headers: &HeaderMap,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<MessagesHttpResponse> {
        let mut headers = self.model_request_headers(&self.profile.snapshot(), context)?;
        super::headers::append_passthrough_headers(&mut headers, passthrough_headers);
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        self.append_middleware_headers(&mut headers)?;
        let body = if self.privacy.is_some() {
            let mut value =
                serde_json::from_slice(&body).map_err(CodexClientError::RequestBodyEncode)?;
            self.apply_privacy(&mut value, &mut headers)?;
            Bytes::from(serde_json::to_vec(&value).map_err(CodexClientError::RequestBodyEncode)?)
        } else {
            body
        };
        let trace = context
            .trace
            .cloned()
            .unwrap_or_default()
            .exchange("messages_http");
        let request = self
            .prepare_http_request(super::endpoint_url(&self.base_url, endpoint), headers, body)
            .await?;
        trace.headers(
            "upstream.request.headers",
            serde_json::json!({"method":"POST"}),
            request
                .headers()
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_bytes())),
        );
        trace.capture(
            "upstream.request.body",
            request
                .body()
                .and_then(reqwest::Body::as_bytes)
                .unwrap_or_default(),
        );
        let started = Instant::now();
        let response = self
            .http_opening_client()?
            .execute(request)
            .await
            .map_err(CodexClientError::Http)?;
        let transport_metrics = CodexTransportMetrics {
            upstream_headers_ms: Some(elapsed_duration_millis(started.elapsed())),
            http_version: Some(http_version_name(response.version()).to_owned()),
            ..CodexTransportMetrics::default()
        };
        trace.headers(
            "upstream.response.headers",
            serde_json::json!({"status":response.status().as_u16()}),
            response
                .headers()
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_bytes())),
        );
        if !response.status().is_success() {
            return Err(http_json_upstream_error(response, &trace, transport_metrics).await);
        }
        let streaming = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|header| header.to_str().ok())
            .is_some_and(|value| {
                value
                    .split(';')
                    .next()
                    .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"))
            });
        let diagnostics =
            response_meta::diagnostics(Some(response.status().as_u16()), response.headers())
                .with_response_origin(Some(response.url().clone()));
        let response_metadata = response_meta::response_metadata(response.headers());
        let body = Box::pin(
            response
                .bytes_stream()
                .map(|chunk| chunk.map_err(CodexClientError::Http)),
        );
        Ok(MessagesHttpResponse {
            body,
            streaming,
            diagnostics,
            response_metadata,
            transport_metrics,
        })
    }
}
