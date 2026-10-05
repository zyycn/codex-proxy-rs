//! Codex Live / Realtime 语音通话的账号级上游透明传输。
//!
//! 语音通话 SDP 引导与 hangup 都是一次性账号级 POST；
//! 正文与响应头（`Location` 携带 call id）均不经过 serde 往返，保持上游原始字节。

use std::time::Instant;

use bytes::Bytes;
use gateway_protocol::openai::is_transport_managed_request_header;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

use super::{
    client::{
        CodexBackendClient, CodexBackendTransport, CodexClientError, CodexClientResult,
        CodexClientVisibleUpstreamResponse, CodexRequestContext, CodexTransportMetrics,
        elapsed_duration_millis, http_version_name, read_error_response_body, retry_after_seconds,
    },
    diagnostics::{CodexUpstreamDiagnostics, CodexUpstreamSendPhase},
    endpoints::endpoint_url,
    headers::is_managed_identity_header,
    response_meta,
};

impl CodexBackendClient {
    /// Live 各端点共用画像与账号基线，不附加 Responses 的路由和续接字段
    pub(crate) fn live_request_headers(
        &self,
        context: CodexRequestContext<'_>,
        extra_protocol_headers: &[(String, String)],
    ) -> CodexClientResult<HeaderMap> {
        let mut headers = self.model_request_headers(&self.profile.snapshot(), context)?;
        for (name, value) in extra_protocol_headers {
            let name = HeaderName::from_bytes(name.as_bytes())?;
            if is_managed_identity_header(name.as_str())
                || is_transport_managed_request_header(name.as_str())
            {
                continue;
            }
            headers.append(name, HeaderValue::from_str(value)?);
        }
        Ok(headers)
    }

    /// 向 Codex realtime calls 端点发送账号级 POST；请求与响应正文不经过 serde。
    ///
    /// `extra_protocol_headers` 是客户端协议头经过允许清单过滤后的结果，
    /// 仅追加业务语义字段，身份与画像不能覆盖账号基线
    pub(crate) async fn post_live_call(
        &self,
        endpoint_path: &'static str,
        query: Option<&str>,
        content_type: Option<&str>,
        extra_protocol_headers: &[(String, String)],
        body: Bytes,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<CodexLiveCallResponse> {
        let mut url = endpoint_url(&self.base_url, endpoint_path);
        if let Some(query) = query {
            url.push('?');
            url.push_str(query);
        }
        self.send_live_request(url, content_type, extra_protocol_headers, body, context)
            .await
    }

    /// 向显式上游 URL（hangup 位于 api.openai.com）发送账号级 POST。
    pub(crate) async fn post_live_hangup(
        &self,
        url: String,
        content_type: Option<&str>,
        extra_protocol_headers: &[(String, String)],
        body: Bytes,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<CodexLiveCallResponse> {
        self.send_live_request(url, content_type, extra_protocol_headers, body, context)
            .await
    }

    async fn send_live_request(
        &self,
        url: String,
        content_type: Option<&str>,
        extra_protocol_headers: &[(String, String)],
        body: Bytes,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<CodexLiveCallResponse> {
        let mut headers = self.live_request_headers(context, extra_protocol_headers)?;
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_str(content_type.unwrap_or("application/json"))?,
        );
        self.append_middleware_headers(&mut headers)?;

        let trace = context
            .trace
            .cloned()
            .unwrap_or_default()
            .exchange("http_json");
        trace.headers(
            "upstream.request.headers",
            serde_json::json!({
                "method": "POST", "endpoint": url,
            }),
            headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_bytes())),
        );
        trace.capture("upstream.request.body", &body);

        let headers_started_at = Instant::now();
        let response = self
            .client
            .post(url)
            .headers(headers)
            .body(body)
            .send()
            .await
            .map_err(CodexClientError::HttpJson)?;
        let upstream_headers_ms = elapsed_duration_millis(headers_started_at.elapsed());
        let http_version = http_version_name(response.version()).to_owned();
        let status = response.status();
        trace.headers(
            "upstream.response.headers",
            serde_json::json!({
                "status": status.as_u16(), "httpVersion": http_version,
                "headersMs": upstream_headers_ms,
            }),
            response
                .headers()
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_bytes())),
        );
        let diagnostics = response_meta::diagnostics(Some(status.as_u16()), response.headers());
        let set_cookie_headers = response_meta::set_cookie_headers(response.headers());
        let rate_limit_headers = response_meta::rate_limit_headers(response.headers());
        let retry_after_seconds = retry_after_seconds(response.headers(), None);
        let transport_metrics = CodexTransportMetrics {
            upstream_headers_ms: Some(upstream_headers_ms),
            http_version: Some(http_version),
            ..CodexTransportMetrics::default()
        };

        if !status.is_success() {
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .map(|value| value.as_bytes().to_vec());
            let client_headers = response_meta::client_headers(response.headers());
            let raw_body = read_error_response_body(response).await.map_err(|source| {
                CodexClientError::ErrorBodyRead {
                    source,
                    status,
                    diagnostics: Box::new(diagnostics.clone()),
                    transport: CodexBackendTransport::HttpJson,
                    transport_metrics: Box::new(transport_metrics.clone()),
                }
            })?;
            trace.capture("upstream.error.body", &raw_body);
            let error_body = String::from_utf8_lossy(&raw_body).into_owned();
            return Err(CodexClientError::Upstream {
                status,
                body: error_body,
                client_response: Some(Box::new(CodexClientVisibleUpstreamResponse::new(
                    status,
                    content_type,
                    client_headers,
                    raw_body,
                ))),
                retry_after_seconds,
                diagnostics: Box::new(diagnostics),
                set_cookie_headers,
                rate_limit_headers,
                transport: CodexBackendTransport::HttpJson,
                transport_metrics: Box::new(transport_metrics),
                send_phase: CodexUpstreamSendPhase::AfterPayload,
            });
        }

        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let forwarded_headers = live_forwarded_headers(response.headers());
        let body = response.bytes().await.map_err(CodexClientError::HttpJson)?;
        trace.capture("upstream.response.body", &body);
        Ok(CodexLiveCallResponse {
            status: status.as_u16(),
            body,
            location,
            content_type,
            retry_after_seconds,
            forwarded_headers,
            set_cookie_headers,
            rate_limit_headers,
            diagnostics,
            transport_metrics,
        })
    }
}

/// 上游 2xx 响应中可回传客户端的头；`Location` 是客户端获知 call id 的合同。
fn live_forwarded_headers(headers: &HeaderMap) -> Vec<(String, String)> {
    const FORWARDABLE: [&str; 5] = [
        "location",
        "content-type",
        "retry-after",
        "x-request-id",
        "openai-request-id",
    ];
    headers
        .iter()
        .filter(|(name, _)| FORWARDABLE.contains(&name.as_str()))
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_owned(), value.to_owned()))
        })
        .collect()
}

/// Live 通话引导与 hangup 的账号级响应。
#[derive(Debug)]
pub struct CodexLiveCallResponse {
    /// 上游 2xx 状态码。
    pub status: u16,
    /// 上游原始正文（引导成功时为 SDP answer）。
    pub body: Bytes,
    /// `Location` 响应头；成功引导响应携带 call id 的唯一来源。
    pub location: Option<String>,
    /// 上游 Content-Type。
    pub content_type: Option<String>,
    pub retry_after_seconds: Option<u64>,
    /// 可回传客户端的响应头子集。
    pub forwarded_headers: Vec<(String, String)>,
    pub set_cookie_headers: Vec<String>,
    pub rate_limit_headers: Vec<(String, String)>,
    pub diagnostics: CodexUpstreamDiagnostics,
    pub transport_metrics: CodexTransportMetrics,
}
