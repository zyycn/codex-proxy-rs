use reqwest::StatusCode;
use serde_json::Value;

use super::{
    client::{
        read_capped_error_body, retry_after_seconds, retry_after_seconds_from_body,
        truncate_for_error,
    },
    endpoints::usage_endpoint_urls,
    CodexBackendClient, CodexClientError, CodexClientResult, CodexRequestContext,
};

impl CodexBackendClient {
    /// 获取 Codex usage JSON。
    pub async fn fetch_usage(&self, context: CodexRequestContext<'_>) -> CodexClientResult<Value> {
        let mut last_invalid_body = None;

        for endpoint in usage_endpoint_urls(&self.base_url) {
            let headers = self.usage_request_headers(context)?;
            let response = self.client.get(endpoint).headers(headers).send().await?;
            let status = response.status();
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
                    set_cookie_headers: Vec::new(),
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
            body: last_invalid_body
                .map(|body| format!("invalid usage response: {}", truncate_for_error(&body)))
                .unwrap_or_else(|| "usage endpoint is unavailable".to_string()),
            set_cookie_headers: Vec::new(),
        })
    }
}
