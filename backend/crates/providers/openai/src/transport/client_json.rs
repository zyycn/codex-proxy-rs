//! Codex 非流式 JSON 上游透明传输。

use std::time::Instant;

use bytes::Bytes;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderName, HeaderValue};

use super::{
    client::{
        CodexBackendClient, CodexBackendJsonResponse, CodexBackendTransport, CodexClientError,
        CodexClientResult, CodexClientVisibleUpstreamResponse, CodexRequestContext,
        CodexTransportMetrics, elapsed_duration_millis, http_version_name,
        read_error_response_body, retry_after_seconds,
    },
    diagnostics::CodexUpstreamSendPhase,
    endpoints::endpoint_url,
    headers::{build_codex_base_headers, insert_optional_header, insert_optional_protocol_header},
    response_meta,
};

impl CodexBackendClient {
    /// 向固定 Provider 端点发送一次原始 JSON；请求与成功响应正文均不经过 serde。
    pub(crate) async fn post_raw_json(
        &self,
        endpoint_path: &'static str,
        body: Bytes,
        image_turn_id: Option<&str>,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<CodexBackendJsonResponse> {
        let profile = self.profile.snapshot();
        let mut headers =
            build_codex_base_headers(&profile, context.authorization, context.account_id)?;
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        insert_optional_header(&mut headers, "cookie", context.cookie_header)?;
        insert_optional_header(
            &mut headers,
            "x-codex-installation-id",
            context.installation_id,
        )?;
        insert_optional_protocol_header(&mut headers, "x-codex-image-turn-id", image_turn_id);
        headers.insert(
            HeaderName::from_static("x-client-request-id"),
            HeaderValue::from_str(context.request_id)?,
        );

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
        let diagnostics = response_meta::diagnostics(Some(status.as_u16()), response.headers());
        let set_cookie_headers = response_meta::set_cookie_headers(response.headers());
        let rate_limit_headers = response_meta::rate_limit_headers(response.headers());
        let response_metadata = response_meta::response_metadata(response.headers());
        let retry_after_seconds = retry_after_seconds(response.headers(), None);
        let transport_metrics = CodexTransportMetrics {
            upstream_headers_ms: Some(upstream_headers_ms),
            http_version: Some(http_version),
            ..CodexTransportMetrics::default()
        };

        if !status.is_success() {
            let content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .map(|value| value.as_bytes().to_vec());
            let client_headers = response_meta::client_headers(response.headers());
            let raw_body = read_error_response_body(response)
                .await
                .map_err(CodexClientError::HttpJson)?;
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

        let body = response.bytes().await.map_err(CodexClientError::HttpJson)?;
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
