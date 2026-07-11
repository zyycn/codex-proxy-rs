use serde_json::Value;

use crate::{
    dispatch::errors::{is_history_recovery_code, is_model_unsupported_signal},
    fleet::account::AccountStatus,
    upstream::openai::{
        protocol::{
            events::TokenUsage,
            responses::{response_from_codex_sse, CollectedResponse, ResponsesSseFailure},
            sse::SseError,
        },
        transport::is_banned_auth_signal,
    },
};

pub(in crate::dispatch) const STREAM_DISCONNECTED_CODE: &str = "stream_disconnected";
pub(in crate::dispatch) const STREAM_DISCONNECTED_MESSAGE: &str =
    "Upstream stream closed before response.completed";

pub(in crate::dispatch) fn sse_failure_error_body(failure: &ResponsesSseFailure) -> String {
    match failure.upstream_code.as_deref() {
        Some(code) => serde_json::json!({
            "error": {
                "code": code,
                "message": failure.message.as_str(),
            }
        })
        .to_string(),
        None => failure.message.clone(),
    }
}

fn sse_failure_matches<F>(failure: &ResponsesSseFailure, signal: F) -> bool
where
    F: Fn(&str) -> bool,
{
    failure.upstream_code.as_deref().is_some_and(&signal) || signal(&failure.message)
}

fn sse_failure_matches_parts<C, M>(
    failure: &ResponsesSseFailure,
    code_signal: C,
    message_signal: M,
) -> bool
where
    C: Fn(&str) -> bool,
    M: Fn(&str) -> bool,
{
    failure.upstream_code.as_deref().is_some_and(code_signal) || message_signal(&failure.message)
}

pub(crate) fn is_quota_exhausted_sse_failure(failure: &ResponsesSseFailure) -> bool {
    sse_failure_matches_parts(
        failure,
        |code| matches!(code, "quota_exceeded" | "insufficient_quota"),
        |message| message.to_ascii_lowercase().contains("quota"),
    )
}

fn is_auth_sse_failure_code(code: &str) -> bool {
    let code = code.to_ascii_lowercase();
    matches!(
        code.as_str(),
        "token_invalid"
            | "token_expired"
            | "token_revoked"
            | "account_deactivated"
            | "unauthorized"
            | "invalid_api_key"
    )
}

fn is_auth_sse_failure_message(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("token revoked")
        || message.contains("token invalid")
        || message.contains("token expired")
}

pub(crate) fn is_auth_sse_failure(failure: &ResponsesSseFailure) -> bool {
    sse_failure_matches_parts(
        failure,
        is_auth_sse_failure_code,
        is_auth_sse_failure_message,
    )
}

pub(in crate::dispatch) fn is_model_unsupported_sse_failure(failure: &ResponsesSseFailure) -> bool {
    sse_failure_matches(failure, is_model_unsupported_signal)
}

pub(in crate::dispatch) fn is_history_recovery_sse_failure(failure: &ResponsesSseFailure) -> bool {
    failure
        .upstream_code
        .as_deref()
        .is_some_and(is_history_recovery_code)
}

pub(crate) fn auth_sse_failure_account_status(failure: &ResponsesSseFailure) -> AccountStatus {
    if sse_failure_matches(failure, is_banned_auth_signal) {
        AccountStatus::Banned
    } else {
        AccountStatus::Expired
    }
}

pub(in crate::dispatch) fn first_sse_failure(
    prefetched: &[u8],
) -> Result<Option<ResponsesSseFailure>, SseError> {
    let body = String::from_utf8_lossy(prefetched);
    match response_from_codex_sse(&body, None)? {
        CollectedResponse::Failed(failure) => Ok(Some(failure)),
        CollectedResponse::Completed(_)
        | CollectedResponse::Incomplete(_)
        | CollectedResponse::MissingCompleted
        | CollectedResponse::Empty => Ok(None),
    }
}

pub(in crate::dispatch) fn stream_failure_metadata(
    failure: &ResponsesSseFailure,
    usage: Option<TokenUsage>,
) -> Value {
    let mut metadata = serde_json::json!({
        "stream": true,
        "failed": true,
        "failureEvent": failure.event,
        "failureMessage": failure.message,
        "upstreamCode": failure.upstream_code,
        "usage": usage,
    });
    enrich_stream_failure_source_metadata(&mut metadata, failure);
    metadata
}

fn enrich_stream_failure_source_metadata(metadata: &mut Value, failure: &ResponsesSseFailure) {
    let Some(object) = metadata.as_object_mut() else {
        return;
    };
    object.insert(
        "failureSource".to_string(),
        Value::String(stream_failure_source(failure).to_string()),
    );
    if let Some(detail) = synthetic_stream_disconnected_detail(failure) {
        object.insert("synthetic".to_string(), Value::Bool(true));
        if !detail.is_empty() {
            object.insert("failureDetail".to_string(), Value::String(detail));
        }
    }
}

pub(in crate::dispatch) fn stream_failure_source(failure: &ResponsesSseFailure) -> &'static str {
    if synthetic_stream_disconnected_detail(failure).is_some() {
        "proxy"
    } else {
        "upstream"
    }
}

pub(in crate::dispatch) fn synthetic_stream_disconnected_detail(
    failure: &ResponsesSseFailure,
) -> Option<String> {
    if failure.upstream_code.as_deref() != Some(STREAM_DISCONNECTED_CODE) {
        return None;
    }
    let detail = failure
        .message
        .strip_prefix(STREAM_DISCONNECTED_MESSAGE)?
        .strip_prefix(": ")
        .unwrap_or_default()
        .trim()
        .to_string();
    Some(detail)
}

pub(in crate::dispatch) fn status_code_for_stream_failure(failure: &ResponsesSseFailure) -> i64 {
    let code = failure
        .upstream_code
        .as_deref()
        .unwrap_or("error")
        .to_ascii_lowercase();
    if code.contains("model") && (code.contains("not_supported") || code.contains("not_available"))
    {
        return 400;
    }
    if code.contains("invalid_request") || code.contains("not_found") {
        return 400;
    }
    if code.contains("context_window")
        || code.contains("invalid_prompt")
        || code.contains("cyber_policy")
        || code.contains("bad_request")
    {
        return 400;
    }
    if code.contains("rate_limit") || code.contains("usage_limit") {
        return 429;
    }
    if code.contains("unauthorized")
        || code.contains("invalid_api_key")
        || code == "token_invalid"
        || code == "token_expired"
        || code == "account_deactivated"
    {
        return 401;
    }
    if code.contains("forbidden") || code.contains("banned") {
        return 403;
    }
    if code.contains("payment") || code.contains("quota") {
        return 429;
    }
    if code.contains("server_overloaded") {
        return 503;
    }
    502
}

pub(in crate::dispatch) fn stream_failure_http_status(failure: &ResponsesSseFailure) -> u16 {
    u16::try_from(status_code_for_stream_failure(failure)).unwrap_or(502)
}
