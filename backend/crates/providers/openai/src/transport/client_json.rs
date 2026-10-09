//! Codex 非流式 JSON 上游透明传输

use std::time::Instant;

use bytes::Bytes;
use reqwest::header::{CONTENT_TYPE, HeaderValue};

use super::{
    client::{
        CodexBackendClient, CodexBackendJsonResponse, CodexClientError, CodexClientResult,
        CodexRequestContext, CodexTransportMetrics, elapsed_duration_millis, http_version_name,
    },
    endpoints::endpoint_url,
    headers::insert_optional_protocol_header,
    response_meta,
};

impl CodexBackendClient {
    /// 向固定 Provider 端点发送 JSON，仅启用隐私规则时解码请求正文
    pub(crate) async fn post_raw_json(
        &self,
        endpoint_path: &'static str,
        body: Bytes,
        passthrough_headers: &reqwest::header::HeaderMap,
        image_turn_id: Option<&str>,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<CodexBackendJsonResponse> {
        // Provider 端点以 Codex 路径标识；API Key 在自己的 API 前缀下使用对应相对路径
        let endpoint_path = if self.protocol == super::client::OpenAiUpstreamProtocol::ResponsesApi
        {
            endpoint_path
                .strip_prefix("/codex")
                .unwrap_or(endpoint_path)
        } else {
            endpoint_path
        };
        let profile = self.profile.snapshot();
        let mut headers = self.model_request_headers(&profile, context)?;
        super::headers::append_passthrough_headers(&mut headers, passthrough_headers);
        // 独立端点没有 Responses 原连接归属，续接状态不能跨账号继承
        // turn metadata 随后仅使用当前 lease 已处理的值
        headers.remove("x-codex-turn-state");
        headers.remove("x-codex-turn-metadata");
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        insert_optional_protocol_header(&mut headers, "x-codex-image-turn-id", image_turn_id);
        insert_optional_protocol_header(
            &mut headers,
            "x-codex-turn-metadata",
            context.turn_metadata,
        );
        self.append_middleware_headers(&mut headers)?;
        let body = if self.privacy.is_some() {
            let mut value: serde_json::Value =
                serde_json::from_slice(&body).map_err(CodexClientError::RequestBodyEncode)?;
            let original = value.clone();
            self.apply_privacy(&mut value, &mut headers)?;
            if value == original {
                body
            } else {
                bytes::Bytes::from(
                    serde_json::to_vec(&value).map_err(CodexClientError::RequestBodyEncode)?,
                )
            }
        } else {
            body
        };

        let trace = context
            .trace
            .cloned()
            .unwrap_or_default()
            .exchange("http_json");
        trace.headers(
            "upstream.request.headers",
            serde_json::json!({
                "method": "POST", "endpoint": endpoint_path,
            }),
            headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_bytes())),
        );
        trace.capture("upstream.request.body", &body);
        let headers_started_at = Instant::now();
        let response = self
            .client
            .post(endpoint_url(&self.base_url, endpoint_path))
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

        let diagnostics = response_meta::diagnostics(Some(status.as_u16()), response.headers());
        let set_cookie_headers = response_meta::set_cookie_headers(response.headers());
        let rate_limit_headers = response_meta::rate_limit_headers(response.headers());
        let response_metadata = response_meta::response_metadata(response.headers());

        let body = response.bytes().await.map_err(CodexClientError::HttpJson)?;
        trace.capture("upstream.response.body", &body);
        Ok(CodexBackendJsonResponse {
            body,
            set_cookie_headers,
            rate_limit_headers,
            diagnostics,
            response_metadata,
            transport_metrics,
        })
    }
}
