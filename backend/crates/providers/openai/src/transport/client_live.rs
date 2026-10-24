//! Codex Live / Realtime 语音通话的账号级上游透明传输。
//!
//! 语音通话 SDP 引导与 hangup 都是一次性账号级 POST；
//! 正文与响应头（`Location` 携带 call id）均不经过 serde 往返，保持上游原始字节。

use std::time::Instant;

use bytes::Bytes;
use gateway_core::operation::ProviderHttpHeader;
use gateway_protocol::openai::is_transport_managed_request_header;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

use super::{
    client::{
        CodexBackendClient, CodexClientError, CodexClientResult, CodexRequestContext,
        CodexTransportMetrics, elapsed_duration_millis, http_version_name, retry_after_seconds,
    },
    diagnostics::CodexUpstreamDiagnostics,
    downstream::is_non_codex_request_header,
    endpoints::endpoint_url,
    headers::is_managed_identity_header,
    response_meta,
};

impl CodexBackendClient {
    /// Live 各端点共用画像与账号基线，不附加 Responses 的路由和续接字段
    pub(crate) fn live_request_headers(
        &self,
        context: CodexRequestContext<'_>,
        extra_protocol_headers: &[ProviderHttpHeader],
        content_type: Option<&str>,
    ) -> CodexClientResult<HeaderMap> {
        let mut headers = self.model_request_headers(&self.profile.snapshot(), context)?;
        for header in extra_protocol_headers {
            let name = HeaderName::from_bytes(header.name().as_bytes())?;
            if is_managed_identity_header(name.as_str())
                || is_transport_managed_request_header(name.as_str())
                || is_non_codex_request_header(name.as_str())
                // Live 不继承 Responses 的路由或连接续接状态
                || matches!(name.as_str(), "x-codex-routing-hint" | "x-codex-turn-state" | "x-codex-turn-id")
            {
                continue;
            }
            let value = if name == "x-codex-turn-metadata" {
                // Live 没有 Responses 续接归属，保留 metadata 扩展前先清理原账号绑定信息
                let Some(value) = context
                    .installation_id
                    .zip(std::str::from_utf8(header.value()).ok())
                    .and_then(|(installation_id, raw)| {
                        super::request::scope_turn_metadata(raw, installation_id, true)
                    })
                else {
                    continue;
                };
                HeaderValue::from_str(&value)?
            } else {
                HeaderValue::from_bytes(header.value())?
            };
            headers.append(name, value);
        }
        if let Some(content_type) = content_type {
            headers.insert(
                reqwest::header::CONTENT_TYPE,
                HeaderValue::from_str(content_type)?,
            );
        }
        self.append_middleware_headers(&mut headers)?;
        super::correlation::scope_headers(&mut headers, context.installation_id);
        Ok(headers)
    }

    /// 向 Codex realtime calls 端点发送账号级 POST；请求与响应正文不经过 serde。
    ///
    /// 业务扩展头保留多值字节，身份、环境和追踪边界由各端点共用的头组装处理
    pub(crate) async fn post_live_call(
        &self,
        endpoint_path: &'static str,
        query: Option<&str>,
        content_type: Option<&str>,
        extra_protocol_headers: &[ProviderHttpHeader],
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
        extra_protocol_headers: &[ProviderHttpHeader],
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
        extra_protocol_headers: &[ProviderHttpHeader],
        body: Bytes,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<CodexLiveCallResponse> {
        let headers = self.live_request_headers(
            context,
            extra_protocol_headers,
            Some(content_type.unwrap_or("application/json")),
        )?;

        let trace = context
            .trace
            .cloned()
            .unwrap_or_default()
            .exchange("http_json");
        let outbound = self.prepare_http_request(url, headers, body).await?;
        trace.headers(
            "upstream.request.headers",
            serde_json::json!({"method": outbound.method().as_str(), "transport": "http_json"}),
            outbound
                .headers()
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_bytes())),
        );
        trace.capture(
            "upstream.request.body",
            outbound
                .body()
                .and_then(reqwest::Body::as_bytes)
                .unwrap_or_default(),
        );
        let headers_started_at = Instant::now();
        let response = self
            .client
            .execute(outbound)
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
        let transport_metrics = CodexTransportMetrics {
            upstream_headers_ms: Some(upstream_headers_ms),
            http_version: Some(http_version),
            ..CodexTransportMetrics::default()
        };

        if !status.is_success() {
            return Err(super::client::http_json_upstream_error(
                response,
                &trace,
                transport_metrics,
            )
            .await);
        }

        let diagnostics = response_meta::diagnostics(Some(status.as_u16()), response.headers())
            .with_response_origin(Some(response.url().clone()));
        let set_cookie_headers = response_meta::set_cookie_headers(response.headers());
        let rate_limit_headers = response_meta::rate_limit_headers(response.headers());
        let retry_after_seconds = retry_after_seconds(response.headers(), None);

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

/// 共用响应隔离规则；原始正文的 Content-Type 由 Live adapter 单独保留
fn live_forwarded_headers(headers: &HeaderMap) -> Vec<(String, Bytes)> {
    let mut forwarded = response_meta::client_headers(headers);
    forwarded.extend(
        headers
            .get_all(reqwest::header::CONTENT_TYPE)
            .iter()
            .map(|value| {
                (
                    "content-type".to_owned(),
                    Bytes::copy_from_slice(value.as_bytes()),
                )
            }),
    );
    forwarded
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
    pub forwarded_headers: Vec<(String, Bytes)>,
    pub set_cookie_headers: Vec<String>,
    pub rate_limit_headers: Vec<(String, String)>,
    pub diagnostics: CodexUpstreamDiagnostics,
    pub transport_metrics: CodexTransportMetrics,
}
