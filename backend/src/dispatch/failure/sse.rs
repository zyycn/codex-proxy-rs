//! Responses SSE 失败事实和流终态元数据。

use serde_json::Value;

use crate::upstream::openai::protocol::{events::TokenUsage, responses::ResponsesSseFailure};

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
