//! Upstream response diagnostics captured at the transport boundary.

use super::protocol::responses::ResponsesSseFailure;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::{StatusCode, header::HeaderMap};
use serde_json::Value;

const UPSTREAM_REQUEST_ID_HEADERS: &[&str] = &[
    "x-request-id",
    "x-oai-request-id",
    "x-openai-request-id",
    "openai-request-id",
    "request-id",
];

const UPSTREAM_TRACE_HEADERS: &[&str] = &[
    "x-request-id",
    "x-oai-request-id",
    "x-openai-request-id",
    "openai-request-id",
    "request-id",
    "cf-ray",
];
const IDENTITY_AUTHORIZATION_ERROR_HEADER: &str = "x-openai-authorization-error";
const IDENTITY_ERROR_JSON_HEADER: &str = "x-error-json";
const PERSISTABLE_UPSTREAM_CODES: &[&str] = &[
    "access_token_expired",
    "account_banned",
    "account_deactivated",
    "account_disabled",
    "account_suspended",
    "authentication_error",
    "billing_limit",
    "cyber_policy",
    "deactivated_workspace",
    "identity_verification_required",
    "insufficient_quota",
    "invalid_api_key",
    "invalid_encrypted_content",
    "invalid_prompt",
    "invalid_request",
    "missing_tool_output",
    "model_not_available",
    "model_not_supported",
    "no_tool_output",
    "organization_disabled",
    "payment_required",
    "permission_denied",
    "previous_response_not_found",
    "quota_exceeded",
    "quota_exhausted",
    "rate_limit_error",
    "rate_limit_exceeded",
    "rate_limit_reached",
    "refresh_token_invalidated",
    "server_is_overloaded",
    "slow_down",
    "token_expired",
    "token_invalid",
    "token_invalidated",
    "token_revoked",
    "unauthorized",
    "unsupported",
    "unsupported_feature",
    "usage_limit_reached",
    "verification_required",
    "workspace_deactivated",
    "workspace_member_credits_depleted",
    "workspace_member_usage_limit_reached",
    "workspace_owner_credits_depleted",
    "workspace_owner_usage_limit_reached",
];

/// 上游拒绝相对业务 payload 的发送阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexUpstreamSendPhase {
    /// DNS/TCP/TLS/WS opening 阶段，业务 payload 尚未发送。
    BeforePayload,
    /// 上游已收到业务 payload，并返回了明确拒绝。
    AfterPayload,
    /// 无法证明 payload 是否到达上游。
    Ambiguous,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CodexFailureCategory {
    ModelUnsupported,
    CredentialExpired,
    IdentityVerificationRequired,
    Banned,
    RateLimited,
    QuotaExhausted,
    CloudflareChallenge,
    CloudflarePathBlocked,
    InvalidRequest,
    PermissionDenied,
    Timeout,
    Unavailable,
    Transport,
}

pub(crate) struct CodexUpstreamFailure {
    pub(crate) status: Option<StatusCode>,
    pub(crate) code: Option<String>,
    pub(crate) identity_error_code: Option<String>,
    pub(crate) retry_after_seconds: Option<u64>,
    pub(crate) request_id: Option<String>,
    pub(crate) set_cookie_headers: Vec<String>,
    pub(crate) rate_limit_headers: Vec<(String, String)>,
    pub(crate) send_phase: CodexUpstreamSendPhase,
    category: CodexFailureCategory,
}

impl CodexUpstreamFailure {
    pub(crate) fn from_response(
        status: StatusCode,
        body: &str,
        retry_after_seconds: Option<u64>,
        diagnostics: &CodexUpstreamDiagnostics,
        set_cookie_headers: &[String],
        rate_limit_headers: &[(String, String)],
        send_phase: CodexUpstreamSendPhase,
    ) -> Self {
        let fields = ParsedUpstreamError::from_body(body);
        let category = classify_upstream_failure(
            Some(status),
            body,
            &fields,
            diagnostics.identity_authorization_error.as_deref(),
            diagnostics.identity_error_code.as_deref(),
        );
        Self {
            status: Some(status),
            code: fields.code,
            identity_error_code: diagnostics.identity_error_code.clone(),
            retry_after_seconds,
            request_id: diagnostics.request_id.clone(),
            set_cookie_headers: set_cookie_headers.to_vec(),
            rate_limit_headers: rate_limit_headers.to_vec(),
            send_phase,
            category,
        }
    }

    pub(crate) fn from_sse_failure(
        failure: &ResponsesSseFailure,
        diagnostics: &CodexUpstreamDiagnostics,
        set_cookie_headers: &[String],
        rate_limit_headers: &[(String, String)],
        send_phase: CodexUpstreamSendPhase,
    ) -> Self {
        let fields = ParsedUpstreamError {
            code: failure.upstream_code.clone(),
            error_type: failure.upstream_type.clone(),
            message: failure.message.clone(),
        };
        let status = failure
            .explicit_status_code
            .and_then(|code| StatusCode::from_u16(code).ok());
        let category = classify_upstream_failure(
            status,
            "",
            &fields,
            diagnostics.identity_authorization_error.as_deref(),
            diagnostics.identity_error_code.as_deref(),
        );
        Self {
            status,
            code: fields.code,
            identity_error_code: diagnostics.identity_error_code.clone(),
            retry_after_seconds: failure.retry_after_seconds,
            request_id: diagnostics.request_id.clone(),
            set_cookie_headers: set_cookie_headers.to_vec(),
            rate_limit_headers: rate_limit_headers.to_vec(),
            send_phase,
            category,
        }
    }

    pub(crate) const fn category(&self) -> CodexFailureCategory {
        self.category
    }

    pub(crate) const fn replay_is_safe(&self) -> bool {
        match self.send_phase {
            CodexUpstreamSendPhase::BeforePayload => true,
            CodexUpstreamSendPhase::Ambiguous => false,
            CodexUpstreamSendPhase::AfterPayload => matches!(
                self.category,
                CodexFailureCategory::ModelUnsupported
                    | CodexFailureCategory::CredentialExpired
                    | CodexFailureCategory::IdentityVerificationRequired
                    | CodexFailureCategory::Banned
                    | CodexFailureCategory::RateLimited
                    | CodexFailureCategory::QuotaExhausted
                    | CodexFailureCategory::CloudflareChallenge
                    | CodexFailureCategory::CloudflarePathBlocked
            ),
        }
    }

    pub(crate) fn persistable_code(&self) -> Option<&'static str> {
        self.code
            .as_deref()
            .and_then(persistable_upstream_code)
            .or_else(|| {
                self.identity_error_code
                    .as_deref()
                    .and_then(persistable_upstream_code)
            })
    }
}

struct ParsedUpstreamError {
    code: Option<String>,
    error_type: Option<String>,
    message: String,
}

impl ParsedUpstreamError {
    fn from_body(body: &str) -> Self {
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return Self {
                code: None,
                error_type: None,
                message: body.to_owned(),
            };
        };
        let error = value
            .pointer("/response/error")
            .or_else(|| value.get("error"))
            .or_else(|| value.get("detail"))
            .unwrap_or(&value);
        let code = error
            .get("code")
            .or_else(|| value.get("code"))
            .and_then(Value::as_str)
            .and_then(non_empty_owned);
        let error_type = error
            .get("type")
            .or_else(|| value.get("type"))
            .and_then(Value::as_str)
            .and_then(non_empty_owned);
        let message = error
            .get("message")
            .or_else(|| value.get("message"))
            .and_then(Value::as_str)
            .or_else(|| error.as_str())
            .and_then(non_empty_owned)
            .unwrap_or_else(|| body.to_owned());
        Self {
            code,
            error_type,
            message,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexUpstreamDiagnostics {
    pub status_code: Option<u16>,
    pub request_id: Option<String>,
    pub identity_authorization_error: Option<String>,
    pub identity_error_code: Option<String>,
    pub trace_headers: Vec<(String, String)>,
}

impl CodexUpstreamDiagnostics {
    pub fn from_headers(status_code: Option<u16>, headers: &HeaderMap) -> Self {
        Self {
            status_code,
            request_id: first_header(headers, UPSTREAM_REQUEST_ID_HEADERS),
            identity_authorization_error: header_value(
                headers,
                IDENTITY_AUTHORIZATION_ERROR_HEADER,
            ),
            identity_error_code: header_value(headers, IDENTITY_ERROR_JSON_HEADER)
                .and_then(|encoded| decode_identity_error_code(&encoded)),
            trace_headers: trace_headers(headers),
        }
    }

    pub fn with_status(status_code: u16) -> Self {
        Self {
            status_code: Some(status_code),
            ..Self::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.status_code.is_none()
            && self.request_id.is_none()
            && self.identity_authorization_error.is_none()
            && self.identity_error_code.is_none()
            && self.trace_headers.is_empty()
    }
}

fn first_header(headers: &HeaderMap, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| header_value(headers, name))
}

fn trace_headers(headers: &HeaderMap) -> Vec<(String, String)> {
    UPSTREAM_TRACE_HEADERS
        .iter()
        .filter_map(|name| header_value(headers, name).map(|value| ((*name).to_string(), value)))
        .collect()
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn decode_identity_error_code(encoded: &str) -> Option<String> {
    let decoded = STANDARD.decode(encoded).ok()?;
    serde_json::from_slice::<Value>(&decoded)
        .ok()?
        .pointer("/error/code")?
        .as_str()
        .map(str::trim)
        .filter(|code| !code.is_empty())
        .map(ToString::to_string)
}

fn classify_upstream_failure(
    status: Option<StatusCode>,
    body: &str,
    fields: &ParsedUpstreamError,
    identity_authorization_error: Option<&str>,
    identity_error_code: Option<&str>,
) -> CodexFailureCategory {
    let code = normalized(fields.code.as_deref());
    let error_type = normalized(fields.error_type.as_deref());
    let identity_code = normalized(identity_error_code);
    let identity_authorization = normalized(identity_authorization_error);
    let message = fields.message.to_ascii_lowercase();
    let body = body.to_ascii_lowercase();

    if [code.as_str(), message.as_str(), body.as_str()]
        .into_iter()
        .any(is_model_unsupported)
    {
        return CodexFailureCategory::ModelUnsupported;
    }
    let identity_signals = [
        identity_code.as_str(),
        identity_authorization.as_str(),
        code.as_str(),
        error_type.as_str(),
        message.as_str(),
        body.as_str(),
    ];
    if identity_signals
        .into_iter()
        .any(is_identity_verification_required)
    {
        return CodexFailureCategory::IdentityVerificationRequired;
    }
    if identity_signals.into_iter().any(is_banned_account) {
        return CodexFailureCategory::Banned;
    }
    if identity_signals.into_iter().any(is_expired_credential)
        || status == Some(StatusCode::UNAUTHORIZED)
    {
        return CodexFailureCategory::CredentialExpired;
    }
    if status == Some(StatusCode::FORBIDDEN) && is_cloudflare_challenge(&body) {
        return CodexFailureCategory::CloudflareChallenge;
    }
    if status == Some(StatusCode::NOT_FOUND) && body.trim().is_empty() {
        return CodexFailureCategory::CloudflarePathBlocked;
    }
    if [code.as_str(), error_type.as_str()]
        .into_iter()
        .any(is_rate_limit_signal)
        || is_rate_limit_message(&message)
        || is_rate_limit_message(&body)
        || status == Some(StatusCode::TOO_MANY_REQUESTS)
    {
        return CodexFailureCategory::RateLimited;
    }
    if [code.as_str(), error_type.as_str()]
        .into_iter()
        .any(is_quota_signal)
        || is_quota_message(&message)
        || is_quota_message(&body)
        || status == Some(StatusCode::PAYMENT_REQUIRED)
    {
        return CodexFailureCategory::QuotaExhausted;
    }
    if is_upstream_overload(&code) || is_upstream_overload(&message) {
        return CodexFailureCategory::Unavailable;
    }
    match status.map(|status| status.as_u16()) {
        Some(status) => match status {
            400 | 404 | 409 | 422 => CodexFailureCategory::InvalidRequest,
            403 => CodexFailureCategory::PermissionDenied,
            408 | 504 => CodexFailureCategory::Timeout,
            500..=599 => CodexFailureCategory::Unavailable,
            _ => CodexFailureCategory::Transport,
        },
        None => CodexFailureCategory::Unavailable,
    }
}

fn normalized(value: Option<&str>) -> String {
    value.unwrap_or_default().trim().to_ascii_lowercase()
}

fn non_empty_owned(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn persistable_upstream_code(value: &str) -> Option<&'static str> {
    PERSISTABLE_UPSTREAM_CODES
        .iter()
        .copied()
        .find(|known| value.trim().eq_ignore_ascii_case(known))
}

fn is_model_unsupported(value: &str) -> bool {
    value.contains("model_not_supported")
        || value.contains("model_not_available")
        || (value.contains("model")
            && (value.contains("not supported")
                || value.contains("not available")
                || value.contains("not_supported")
                || value.contains("not_available")))
}

fn is_identity_verification_required(value: &str) -> bool {
    matches!(
        value,
        "identity_verification_required" | "verification_required"
    ) || value.contains("identity verification is required")
}

fn is_banned_account(value: &str) -> bool {
    matches!(
        value,
        "account_banned"
            | "account_deactivated"
            | "account_disabled"
            | "account_suspended"
            | "deactivated_workspace"
            | "organization_disabled"
            | "workspace_deactivated"
    ) || value.contains("account is banned")
        || value.contains("account has been banned")
        || value.contains("account deactivated")
        || value.contains("account has been deactivated")
        || value.contains("account disabled")
        || value.contains("account has been disabled")
        || value.contains("account suspended")
        || value.contains("organization has been disabled")
        || value.contains("workspace has been deactivated")
        || value.contains("deactivated_workspace")
}

fn is_expired_credential(value: &str) -> bool {
    matches!(
        value,
        "token_invalid"
            | "token_invalidated"
            | "token_expired"
            | "token_revoked"
            | "refresh_token_invalidated"
            | "unauthorized"
            | "invalid_api_key"
            | "authentication_error"
            | "access_token_expired"
    ) || value.contains("token revoked")
        || value.contains("token invalidated")
        || value.contains("token invalid")
        || value.contains("token expired")
        || value.contains("unauthorized")
        || value.contains("invalid api key")
}

fn is_rate_limit_signal(value: &str) -> bool {
    matches!(
        value,
        "usage_limit_reached"
            | "rate_limit_exceeded"
            | "rate_limit_reached"
            | "rate_limit_error"
            | "workspace_owner_usage_limit_reached"
            | "workspace_member_usage_limit_reached"
    )
}

fn is_quota_signal(value: &str) -> bool {
    matches!(
        value,
        "quota_exhausted"
            | "quota_exceeded"
            | "payment_required"
            | "insufficient_quota"
            | "workspace_owner_credits_depleted"
            | "workspace_member_credits_depleted"
    ) || value.starts_with("billing_limit")
}

fn is_rate_limit_message(value: &str) -> bool {
    value.contains("rate limit") || value.contains("usage limit")
}

fn is_quota_message(value: &str) -> bool {
    value.contains("quota") || value.contains("payment required") || value.contains("billing limit")
}

fn is_cloudflare_challenge(value: &str) -> bool {
    value.contains("cf-mitigated")
        || value.contains("cf-chl-bypass")
        || value.contains("_cf_chl")
        || value.contains("cf_chl")
        || value.contains("attention required")
        || value.contains("just a moment")
}

fn is_upstream_overload(value: &str) -> bool {
    matches!(value, "server_is_overloaded" | "slow_down") || value.contains("server_overloaded")
}
