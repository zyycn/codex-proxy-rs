//! Codex Desktop 主动额度重置卡 HTTP contract。

use chrono::{DateTime, Utc};
use reqwest::{
    StatusCode,
    header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, USER_AGENT},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    CodexBackendClient, CodexClientError, CodexClientResult, CodexRequestContext,
    client::{
        CodexBackendTransport, CodexTransportMetrics, read_capped_response_body,
        retry_after_seconds,
    },
    diagnostics::CodexUpstreamSendPhase,
    endpoints::{
        WHAM_RATE_LIMIT_RESET_CREDITS_CONSUME_PATH, WHAM_RATE_LIMIT_RESET_CREDITS_PATH,
        endpoint_url,
    },
    headers::insert_optional_header,
    response_meta,
};

/// 单次 reset-credit 响应允许保留和解析的最大字节数。
pub const MAX_CODEX_RESET_CREDITS_BODY_BYTES: usize = 1024 * 1024;
const MAX_RESET_CREDITS: usize = 1024;
const MAX_IDENTIFIER_BYTES: usize = 512;
const MAX_TEXT_BYTES: usize = 2048;

/// 上游返回的一张安全重置卡投影。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexRateLimitResetCredit {
    pub id: String,
    pub status: Option<String>,
    pub title: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub reset_type: Option<String>,
}

/// 上游重置卡列表。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexRateLimitResetCredits {
    pub available_count: u64,
    pub credits: Vec<CodexRateLimitResetCredit>,
}

/// 上游消费结果。`code` 的业务含义由调用方按官方状态机解释。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexRateLimitResetCreditsConsumeResult {
    pub code: String,
    pub credit: Option<CodexRateLimitResetCredit>,
}

#[derive(Deserialize)]
struct ResetCreditWire {
    id: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    expires_at: Option<String>,
    #[serde(default)]
    reset_type: Option<String>,
}

#[derive(Deserialize)]
struct ResetCreditsWire {
    available_count: u64,
    credits: Vec<ResetCreditWire>,
}

#[derive(Deserialize)]
struct ConsumeResultWire {
    code: String,
    #[serde(default)]
    credit: Option<ResetCreditWire>,
}

#[derive(Serialize)]
struct ConsumeRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    credit_id: Option<&'a str>,
    redeem_request_id: String,
}

impl CodexBackendClient {
    /// 查询当前账号的 Codex Desktop 主动额度重置卡。
    pub async fn list_rate_limit_reset_credits(
        &self,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<CodexRateLimitResetCredits> {
        let response = self
            .client
            .get(endpoint_url(
                &self.base_url,
                WHAM_RATE_LIMIT_RESET_CREDITS_PATH,
            ))
            .headers(self.reset_credits_headers(context, false)?)
            .send()
            .await
            .map_err(CodexClientError::HttpJson)?;
        let body = read_reset_credits_response(response).await?;
        let wire = serde_json::from_str::<ResetCreditsWire>(&body)
            .map_err(|_| invalid_response("invalid reset-credit list response"))?;
        if wire.credits.len() > MAX_RESET_CREDITS {
            return Err(invalid_response(
                "reset-credit list exceeded the item limit",
            ));
        }
        Ok(CodexRateLimitResetCredits {
            available_count: wire.available_count,
            credits: wire
                .credits
                .into_iter()
                .map(parse_credit)
                .collect::<CodexClientResult<Vec<_>>>()?,
        })
    }

    /// 消费一张 Codex Desktop 主动额度重置卡。
    ///
    /// 此方法不做任何自动重试；请求发出后的传输失败必须由上层按不确定结果处理。
    pub async fn consume_rate_limit_reset_credit(
        &self,
        context: CodexRequestContext<'_>,
        credit_id: Option<&str>,
        redeem_request_id: Uuid,
    ) -> CodexClientResult<CodexRateLimitResetCreditsConsumeResult> {
        let response = self
            .client
            .post(endpoint_url(
                &self.base_url,
                WHAM_RATE_LIMIT_RESET_CREDITS_CONSUME_PATH,
            ))
            .headers(self.reset_credits_headers(context, true)?)
            .json(&ConsumeRequest {
                credit_id,
                redeem_request_id: redeem_request_id.to_string(),
            })
            .send()
            .await
            .map_err(CodexClientError::HttpJson)?;
        let body = read_reset_credits_response(response).await?;
        let wire = serde_json::from_str::<ConsumeResultWire>(&body)
            .map_err(|_| invalid_response("invalid reset-credit consume response"))?;
        Ok(CodexRateLimitResetCreditsConsumeResult {
            code: required_text(wire.code, MAX_IDENTIFIER_BYTES, "reset-credit result code")?,
            credit: wire.credit.map(parse_credit).transpose()?,
        })
    }

    fn reset_credits_headers(
        &self,
        context: CodexRequestContext<'_>,
        json_body: bool,
    ) -> CodexClientResult<HeaderMap> {
        let profile = self.profile.snapshot();
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(context.authorization)?);
        insert_optional_header(&mut headers, "chatgpt-account-id", context.account_id)?;
        headers.insert(
            HeaderName::from_static("originator"),
            HeaderValue::from_str(&profile.originator)?,
        );
        headers.insert(
            HeaderName::from_static("oai-language"),
            HeaderValue::from_static("en"),
        );
        headers.insert(
            USER_AGENT,
            HeaderValue::from_str(&profile.desktop_user_agent())?,
        );
        // Electron `net.fetch` 为 renderer 请求附加的默认 Accept；该 endpoint 的
        // renderer 并没有显式改写为 Core auxiliary 请求的 application/json 画像。
        headers.insert(ACCEPT, HeaderValue::from_static("*/*"));
        if json_body {
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        }
        Ok(headers)
    }
}

async fn read_reset_credits_response(response: reqwest::Response) -> CodexClientResult<String> {
    let status = response.status();
    let diagnostics = response_meta::diagnostics(Some(status.as_u16()), response.headers());
    let retry_after_seconds = retry_after_seconds(response.headers(), None);
    let body = read_capped_response_body(response, MAX_CODEX_RESET_CREDITS_BODY_BYTES)
        .await
        .map_err(CodexClientError::HttpJson)?;
    if body.limit_exceeded() {
        return Err(upstream_error(
            if status.is_success() {
                StatusCode::BAD_GATEWAY
            } else {
                status
            },
            "upstream reset-credit response exceeded the body limit".to_owned(),
            retry_after_seconds,
            diagnostics,
        ));
    }
    let body = body.into_string();
    if status.is_success() {
        Ok(body)
    } else {
        Err(upstream_error(
            status,
            body,
            retry_after_seconds,
            diagnostics,
        ))
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
        transport: CodexBackendTransport::HttpJson,
        transport_metrics: Box::<CodexTransportMetrics>::default(),
        send_phase: CodexUpstreamSendPhase::AfterPayload,
    }
}

fn invalid_response(message: &'static str) -> CodexClientError {
    upstream_error(
        StatusCode::BAD_GATEWAY,
        message.to_owned(),
        None,
        super::CodexUpstreamDiagnostics::default(),
    )
}

fn parse_credit(wire: ResetCreditWire) -> CodexClientResult<CodexRateLimitResetCredit> {
    let expires_at = wire
        .expires_at
        .map(|value| {
            DateTime::parse_from_rfc3339(value.trim())
                .map(|value| value.with_timezone(&Utc))
                .map_err(|_| invalid_response("invalid reset-credit expiry"))
        })
        .transpose()?;
    Ok(CodexRateLimitResetCredit {
        id: required_text(wire.id, MAX_IDENTIFIER_BYTES, "reset-credit ID")?,
        status: optional_text(wire.status, MAX_IDENTIFIER_BYTES, "reset-credit status")?,
        title: optional_text(wire.title, MAX_TEXT_BYTES, "reset-credit title")?,
        expires_at,
        reset_type: optional_text(wire.reset_type, MAX_IDENTIFIER_BYTES, "reset-credit type")?,
    })
}

fn required_text(
    value: String,
    max_bytes: usize,
    field: &'static str,
) -> CodexClientResult<String> {
    let value = value.trim();
    if value.is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        return Err(invalid_response(field));
    }
    Ok(value.to_owned())
}

fn optional_text(
    value: Option<String>,
    max_bytes: usize,
    field: &'static str,
) -> CodexClientResult<Option<String>> {
    value
        .map(|value| required_text(value, max_bytes, field))
        .transpose()
}
