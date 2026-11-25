//! 保留上游错误事实与本地来源，敏感凭据不得进入运维正文

use gateway_core::{
    error::{ProviderError, ProviderErrorKind as Kind, RawUpstreamError},
    upstream::UpstreamSendState,
};
use reqwest::{Response, StatusCode};
use std::time::{Duration, SystemTime};

const MAX_ERROR_BYTES: usize = 64 * 1024;
pub(crate) fn unsent(kind: Kind) -> ProviderError {
    ProviderError::new(kind, UpstreamSendState::NotSent)
}
pub(crate) fn sent(kind: Kind) -> ProviderError {
    ProviderError::new(kind, UpstreamSendState::Sent)
}
pub(super) fn network(error: reqwest::Error) -> ProviderError {
    let kind = if error.is_timeout() {
        Kind::Timeout
    } else {
        Kind::Transport
    };
    let state = if error.is_connect() {
        UpstreamSendState::NotSent
    } else {
        UpstreamSendState::Ambiguous
    };
    ProviderError::new(kind, state).with_source(error.without_url())
}
fn status_kind(status: StatusCode) -> Kind {
    match status {
        StatusCode::UNAUTHORIZED => Kind::Unauthorized,
        StatusCode::FORBIDDEN => Kind::PermissionDenied,
        StatusCode::TOO_MANY_REQUESTS => Kind::RateLimited,
        StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => Kind::Timeout,
        StatusCode::PAYLOAD_TOO_LARGE => Kind::MessageTooBig,
        s if s.is_server_error() => Kind::Unavailable,
        s if s.is_client_error() => Kind::InvalidRequest,
        _ => Kind::Protocol,
    }
}
pub(super) fn body_error(status: Option<u16>, body: &str, token: &str) -> ProviderError {
    let status = status.and_then(|code| StatusCode::from_u16(code).ok());
    let error = sent(status.map_or(Kind::Unavailable, status_kind));
    let error = if let Some(status) = status {
        error.with_status(status.as_u16())
    } else {
        error
    };
    attach_body(error, body, token)
}
fn attach_body(error: ProviderError, body: &str, token: &str) -> ProviderError {
    let error = if let Some(delay) = retry_info(body) {
        let delay = error
            .retry_after()
            .map_or(delay, |header| header.max(delay));
        error.with_retry_after(delay)
    } else {
        error
    };
    if !token.is_empty() && body.contains(token) {
        return error.redact_sensitive_context(body);
    }
    if body.is_empty() {
        return error;
    }
    let end = body.floor_char_boundary(MAX_ERROR_BYTES.min(body.len()));
    let text = if end < body.len() {
        format!("{}\n[truncated]", &body[..end])
    } else {
        body.to_owned()
    };
    error.with_raw_upstream_error(RawUpstreamError::new(text))
}

fn retry_info(body: &str) -> Option<Duration> {
    let document: serde_json::Value = serde_json::from_str(body).ok()?;
    let error = document
        .get("error")
        .or_else(|| document.pointer("/response/error"))?;
    let detail = error.get("details")?.as_array()?.iter().find(|detail| {
        detail.get("@type").and_then(serde_json::Value::as_str)
            == Some("type.googleapis.com/google.rpc.RetryInfo")
    })?;
    let delay = detail.get("retryDelay")?.as_str()?.strip_suffix('s')?;
    let (seconds, fraction) = delay.split_once('.').unwrap_or((delay, ""));
    if seconds.is_empty()
        || !seconds.bytes().all(|byte| byte.is_ascii_digit())
        || fraction.len() > 9
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let nanos = if fraction.is_empty() {
        0
    } else {
        fraction
            .parse::<u32>()
            .ok()?
            .checked_mul(10_u32.pow(9 - u32::try_from(fraction.len()).ok()?))?
    };
    Some(Duration::new(seconds.parse().ok()?, nanos))
}

fn retry_after(value: &str) -> Option<Duration> {
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let until: SystemTime = chrono::DateTime::parse_from_rfc2822(value).ok()?.into();
    Some(until.duration_since(SystemTime::now()).unwrap_or_default())
}
pub(super) async fn successful(
    mut response: Response,
    token: &str,
) -> Result<Response, ProviderError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(retry_after);
    let mut error = sent(status_kind(status)).with_status(status.as_u16());
    if let Some(delay) = retry_after {
        error = error.with_retry_after(delay);
    }
    let mut body = Vec::new();
    let mut truncated = false;
    let read = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(chunk) = response.chunk().await? {
            let remaining = MAX_ERROR_BYTES.saturating_sub(body.len());
            body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
            if chunk.len() > remaining {
                truncated = true;
                break;
            }
        }
        Ok::<_, reqwest::Error>(())
    })
    .await;
    match read {
        Ok(Ok(())) => {}
        Ok(Err(source)) => error = error.with_source(source.without_url()),
        Err(source) => error = error.with_source(source),
    }
    let text = String::from_utf8_lossy(&body).into_owned();
    if truncated {
        return Err(error.redact_sensitive_context(&text));
    }
    Err(attach_body(error, &text, token))
}
