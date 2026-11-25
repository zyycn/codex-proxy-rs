//! Codex 账号 usage 查询、响应体限制与错误处理

use gateway_protocol::openai::events::retry_after_seconds_from_body;
use reqwest::StatusCode;
use serde_json::Value;

use super::{
    CodexBackendClient, CodexClientError, CodexClientResult, CodexRequestContext,
    client::{read_capped_response_body, retry_after_seconds, truncate_for_error},
    endpoints::usage_endpoint_url,
    response_meta,
};

/// 单次 Codex usage 响应允许保留和解析的最大字节数
pub const MAX_CODEX_USAGE_BODY_BYTES: usize = 1024 * 1024;

impl CodexBackendClient {
    /// 获取 Codex usage JSON
    pub async fn fetch_usage(&self, context: CodexRequestContext<'_>) -> CodexClientResult<Value> {
        let headers = self.account_request_headers(context)?;
        let request = |base_url| {
            self.client
                .get(usage_endpoint_url(base_url))
                .headers(headers.clone())
        };
        let response = self
            .send_account_request(request(&self.base_url), request(&self.official_base_url))
            .await?;
        let status = response.status();
        let diagnostics = response_meta::diagnostics(Some(status.as_u16()), response.headers());
        let retry_after_seconds = retry_after_seconds(response.headers(), None);
        let body = read_capped_response_body(response, MAX_CODEX_USAGE_BODY_BYTES).await?;
        if body.limit_exceeded() {
            return Err(CodexClientError::Upstream {
                status: if status.is_success() {
                    StatusCode::BAD_GATEWAY
                } else {
                    status
                },
                retry_after_seconds,
                body: "upstream usage response exceeded the body limit".to_owned(),
                client_response: None,
                diagnostics: Box::new(diagnostics),
                set_cookie_headers: Vec::new(),
                rate_limit_headers: Vec::new(),
                transport: super::client::CodexBackendTransport::HttpSse,
                transport_metrics: Box::default(),
                send_phase: super::diagnostics::CodexUpstreamSendPhase::AfterPayload,
            });
        }
        let body = body.into_string();

        if !status.is_success() {
            return Err(CodexClientError::Upstream {
                status,
                retry_after_seconds: retry_after_seconds
                    .or_else(|| retry_after_seconds_from_body(&body)),
                body,
                client_response: None,
                diagnostics: Box::new(diagnostics),
                set_cookie_headers: Vec::new(),
                rate_limit_headers: Vec::new(),
                transport: super::client::CodexBackendTransport::HttpSse,
                transport_metrics: Box::default(),
                send_phase: super::diagnostics::CodexUpstreamSendPhase::AfterPayload,
            });
        }

        match serde_json::from_str::<Value>(&body) {
            Ok(parsed) if is_usage_response(&parsed) => Ok(parsed),
            _ => Err(CodexClientError::Upstream {
                status: StatusCode::BAD_GATEWAY,
                retry_after_seconds: None,
                body: format!("invalid usage response: {}", truncate_for_error(&body)),
                client_response: None,
                diagnostics: Box::new(diagnostics),
                set_cookie_headers: Vec::new(),
                rate_limit_headers: Vec::new(),
                transport: super::client::CodexBackendTransport::HttpSse,
                transport_metrics: Box::default(),
                send_phase: super::diagnostics::CodexUpstreamSendPhase::AfterPayload,
            }),
        }
    }
}

fn is_usage_response(value: &Value) -> bool {
    value.as_object().is_some_and(|object| {
        object.get("rate_limit").is_some_and(Value::is_object)
            || object
                .get("additional_rate_limits")
                .is_some_and(Value::is_array)
            || object.get("spend_control").is_some_and(Value::is_object)
            || object.get("credits").is_some_and(Value::is_object)
    })
}
