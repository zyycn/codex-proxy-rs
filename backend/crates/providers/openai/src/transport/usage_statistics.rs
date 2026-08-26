//! Codex 官方每日用量报表 transport。

use chrono::NaiveDate;
use gateway_protocol::openai::events::retry_after_seconds_from_body;
use reqwest::StatusCode;
use serde_json::Value;

use super::{
    CodexBackendClient, CodexClientError, CodexClientResult, CodexRequestContext,
    client::{
        CodexBackendTransport, CodexTransportMetrics, read_capped_response_body,
        retry_after_seconds, truncate_for_error,
    },
    diagnostics::CodexUpstreamSendPhase,
    endpoints::{
        WHAM_DAILY_TOKEN_USAGE_PATH, WHAM_DAILY_WORKSPACE_USAGE_COUNTS_PATH, endpoint_url,
    },
    response_meta,
};

const MAX_CODEX_DAILY_USAGE_BODY_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodexDailyUsageReport {
    PersonalModelCredits,
    PersonalTokenTotals,
}

impl CodexDailyUsageReport {
    const fn path(self) -> &'static str {
        match self {
            Self::PersonalModelCredits => WHAM_DAILY_TOKEN_USAGE_PATH,
            Self::PersonalTokenTotals => WHAM_DAILY_WORKSPACE_USAGE_COUNTS_PATH,
        }
    }

    const fn workspace_user(self) -> bool {
        matches!(self, Self::PersonalTokenTotals)
    }
}

impl CodexBackendClient {
    pub(crate) async fn fetch_daily_usage(
        &self,
        context: CodexRequestContext<'_>,
        report: CodexDailyUsageReport,
        start_date: NaiveDate,
        end_date: NaiveDate,
    ) -> CodexClientResult<Value> {
        let headers = self.usage_request_headers(context)?;
        let workspace_user = if report.workspace_user() {
            "&workspace_user=true"
        } else {
            ""
        };
        let url = format!(
            "{}?start_date={start_date}&end_date={end_date}&group_by=day{workspace_user}",
            endpoint_url(&self.base_url, report.path()),
        );
        let response = self.client.get(url).headers(headers).send().await?;
        let status = response.status();
        let diagnostics = response_meta::diagnostics(Some(status.as_u16()), response.headers());
        let retry_after_seconds = retry_after_seconds(response.headers(), None);
        let body = read_capped_response_body(response, MAX_CODEX_DAILY_USAGE_BODY_BYTES).await?;
        if body.limit_exceeded() {
            return Err(upstream_error(
                if status.is_success() {
                    StatusCode::BAD_GATEWAY
                } else {
                    status
                },
                "upstream daily usage response exceeded the body limit".to_owned(),
                retry_after_seconds,
                diagnostics,
            ));
        }
        let body = body.into_string();
        if !status.is_success() {
            return Err(upstream_error(
                status,
                body.clone(),
                retry_after_seconds.or_else(|| retry_after_seconds_from_body(&body)),
                diagnostics,
            ));
        }

        serde_json::from_str::<Value>(&body).map_err(|_| {
            upstream_error(
                StatusCode::BAD_GATEWAY,
                format!(
                    "invalid daily usage response: {}",
                    truncate_for_error(&body)
                ),
                None,
                diagnostics,
            )
        })
    }
}

fn upstream_error(
    status: StatusCode,
    body: String,
    retry_after_seconds: Option<u64>,
    diagnostics: super::CodexUpstreamDiagnostics,
) -> CodexClientError {
    CodexClientError::Upstream {
        status,
        body,
        client_response: None,
        retry_after_seconds,
        diagnostics: Box::new(diagnostics),
        set_cookie_headers: Vec::new(),
        rate_limit_headers: Vec::new(),
        transport: CodexBackendTransport::HttpSse,
        transport_metrics: Box::<CodexTransportMetrics>::default(),
        send_phase: CodexUpstreamSendPhase::AfterPayload,
    }
}
