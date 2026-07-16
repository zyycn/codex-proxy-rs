use reqwest::StatusCode;
use serde_json::Value;

use crate::upstream::openai::protocol::events::retry_after_seconds_from_body;

use super::{
    CodexBackendClient, CodexClientError, CodexClientResult, CodexRequestContext,
    CodexUpstreamDiagnostics,
    client::{read_capped_error_body, retry_after_seconds, truncate_for_error},
    endpoints::usage_endpoint_urls,
    response_meta,
};

impl CodexBackendClient {
    /// 获取 Codex usage JSON。
    pub async fn fetch_usage(&self, context: CodexRequestContext<'_>) -> CodexClientResult<Value> {
        let mut last_invalid_body = None;

        for endpoint in usage_endpoint_urls(&self.base_url) {
            let headers = self.usage_request_headers(context)?;
            let response = self.client.get(endpoint).headers(headers).send().await?;
            let status = response.status();
            let diagnostics = response_meta::diagnostics(Some(status.as_u16()), response.headers());
            let retry_after_seconds = retry_after_seconds(response.headers(), None);

            if status == StatusCode::NOT_FOUND {
                last_invalid_body = Some(read_capped_error_body(response).await?);
                continue;
            }
            if !status.is_success() {
                let body = read_capped_error_body(response).await?;
                return Err(CodexClientError::Upstream {
                    status,
                    retry_after_seconds: retry_after_seconds
                        .or_else(|| retry_after_seconds_from_body(&body)),
                    body,
                    diagnostics: Box::new(diagnostics),
                    set_cookie_headers: Vec::new(),
                    rate_limit_headers: Vec::new(),
                    transport: super::client::CodexBackendTransport::HttpSse,
                });
            }

            let body = response.text().await?;
            match serde_json::from_str::<Value>(&body) {
                Ok(parsed) if parsed.get("rate_limit").is_some() => return Ok(parsed),
                _ => last_invalid_body = Some(body),
            }
        }

        Err(CodexClientError::Upstream {
            status: StatusCode::BAD_GATEWAY,
            retry_after_seconds: None,
            body: last_invalid_body.map_or_else(
                || "usage endpoint is unavailable".to_string(),
                |body| format!("invalid usage response: {}", truncate_for_error(&body)),
            ),
            diagnostics: Box::new(CodexUpstreamDiagnostics::with_status(
                StatusCode::BAD_GATEWAY.as_u16(),
            )),
            set_cookie_headers: Vec::new(),
            rate_limit_headers: Vec::new(),
            transport: super::client::CodexBackendTransport::HttpSse,
        })
    }
}
