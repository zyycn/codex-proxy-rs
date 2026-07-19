//! 上游响应元数据提取辅助。

use std::time::Instant;

use gateway_protocol::openai::{events::is_rate_limit_header_name, sse::SseEventDecoder};
use reqwest::header::{HeaderMap, SET_COOKIE};

use crate::transport::protocol::responses::response_event_signals;

use super::diagnostics::CodexUpstreamDiagnostics;

/// Codex Responses 上游响应元数据。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexResponseMetadata {
    /// 上游实际选用的模型。
    pub effective_model: Option<String>,
    /// 模型目录版本。
    pub models_etag: Option<String>,
    /// 上游是否声明响应包含 reasoning。
    pub reasoning_included: bool,
    /// 允许透传给客户端的安全响应头。
    pub client_headers: Vec<(String, String)>,
}

const CLIENT_RESPONSE_HEADERS: [&str; 5] = [
    "x-request-id",
    "openai-model",
    "x-models-etag",
    "x-reasoning-included",
    "openai-processing-ms",
];

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
        .filter(|(name, _)| is_rate_limit_header_name(name.as_str()))
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_string(), value.to_string()))
        })
        .collect()
}

pub(super) fn response_metadata(headers: &HeaderMap) -> CodexResponseMetadata {
    response_metadata_from_pairs(
        headers
            .iter()
            .filter_map(|(name, value)| value.to_str().ok().map(|value| (name.as_str(), value))),
    )
}

pub(super) fn merge_response_metadata(
    metadata: &mut CodexResponseMetadata,
    headers: impl IntoIterator<Item = (String, String)>,
) {
    for (name, value) in headers {
        apply_response_header(metadata, &name, &value);
    }
}

fn response_metadata_from_pairs<'a>(
    headers: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> CodexResponseMetadata {
    let mut metadata = CodexResponseMetadata::default();
    for (name, value) in headers {
        apply_response_header(&mut metadata, name, value);
    }
    metadata
}

fn apply_response_header(metadata: &mut CodexResponseMetadata, name: &str, value: &str) {
    let name = name.to_ascii_lowercase();
    let value = value.trim();
    if value.is_empty() || !CLIENT_RESPONSE_HEADERS.contains(&name.as_str()) {
        return;
    }
    match name.as_str() {
        "openai-model" => metadata.effective_model = Some(value.to_string()),
        "x-models-etag" => metadata.models_etag = Some(value.to_string()),
        "x-reasoning-included" => metadata.reasoning_included = true,
        _ => {}
    }
    upsert_client_header(&mut metadata.client_headers, (name, value.to_string()));
}

fn upsert_client_header(headers: &mut Vec<(String, String)>, header: (String, String)) {
    if let Some(existing) = headers
        .iter_mut()
        .find(|(name, _)| name.eq_ignore_ascii_case(&header.0))
    {
        *existing = header;
    } else {
        headers.push(header);
    }
}

pub(super) fn update_response_timing_ms(
    started_at: Instant,
    decoder: &mut SseEventDecoder,
    chunk: &[u8],
    first_token_ms: &mut Option<i64>,
    first_reasoning_ms: &mut Option<i64>,
    first_text_ms: &mut Option<i64>,
) {
    let Ok(events) = decoder.push(chunk) else {
        return;
    };
    for event in events {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&event.data) else {
            continue;
        };
        let event_type = event.event.as_deref().or_else(|| {
            value
                .get("type")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty())
        });
        let signals = response_event_signals(event_type, &value);
        if !signals.semantic_output {
            continue;
        }
        let elapsed_ms = crate::transport::time::elapsed_millis_i64(started_at).max(1);
        if first_token_ms.is_none() {
            *first_token_ms = Some(elapsed_ms);
        }
        if signals.reasoning_output && first_reasoning_ms.is_none() {
            *first_reasoning_ms = Some(elapsed_ms);
        }
        if signals.text_output && first_text_ms.is_none() {
            *first_text_ms = Some(elapsed_ms);
        }
    }
}
