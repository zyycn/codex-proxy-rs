//! 上游响应元数据提取辅助。

use std::time::Instant;

use reqwest::header::{HeaderMap, SET_COOKIE};

use crate::upstream::openai::protocol::responses::update_first_response_event_ms;

use super::diagnostics::CodexUpstreamDiagnostics;

pub(super) fn diagnostics(
    status_code: Option<u16>,
    headers: &HeaderMap,
) -> CodexUpstreamDiagnostics {
    CodexUpstreamDiagnostics::from_headers(status_code, headers)
}

pub(super) fn turn_state(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-codex-turn-state")
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string)
}

pub(super) fn set_cookie_headers(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok().map(ToString::to_string))
        .collect()
}

pub(super) fn rate_limit_headers(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter(|(name, _)| is_rate_limit_header(name.as_str()))
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_string(), value.to_string()))
        })
        .collect()
}

pub(super) fn update_first_token_ms(
    started_at: Instant,
    body_bytes: &[u8],
    first_token_ms: &mut Option<i64>,
) {
    update_first_response_event_ms(started_at, body_bytes, first_token_ms);
}

fn is_rate_limit_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == "retry-after"
        || name.contains("ratelimit")
        || name.contains("rate-limit")
        || name.starts_with("x-codex-primary-")
        || name.starts_with("x-codex-secondary-")
        || name.starts_with("x-codex-code-review-")
        || name.starts_with("x-codex-review-")
        || name.starts_with("x-code-review-")
}
