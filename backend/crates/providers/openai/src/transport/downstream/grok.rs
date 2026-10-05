//! Grok 客户端经 OpenAI Responses 入口访问 Codex 上游时的隔离兼容规则

use reqwest::header::HeaderName;
use serde_json::{Map, Value};

const PASSTHROUGH_HEADERS_CONTEXT_KEY: &str = "opaque_request_headers";
const GROK_INTRODUCTION: &str = "You are Grok released by xAI.";

pub(super) fn is_client_header(name: &str) -> bool {
    name.starts_with("x-grok-") || name.starts_with("x-xai-")
}

fn is_request_marker(name: &str) -> bool {
    matches!(
        name,
        "x-grok-model-override" | "x-grok-turn-idx" | "x-grok-session-id"
    )
}

pub(super) fn normalize_request_body(body: &mut Map<String, Value>, context: &Map<String, Value>) {
    if !has_client_marker(context) {
        return;
    }
    let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) else {
        return;
    };
    for item in input {
        let Some(item) = item.as_object_mut() else {
            continue;
        };
        if item.get("type").and_then(Value::as_str) == Some("message")
            && item.get("role").and_then(Value::as_str) == Some("developer")
            && let Some(content) = item.get_mut("content")
        {
            normalize_instruction_identity(content);
        }
    }
}

fn has_client_marker(context: &Map<String, Value>) -> bool {
    context
        .get(PASSTHROUGH_HEADERS_CONTEXT_KEY)
        .and_then(Value::as_array)
        .is_some_and(|entries| {
            entries
                .iter()
                .filter_map(Value::as_array)
                .filter(|entry| entry.len() == 2)
                .filter_map(|entry| entry.first().and_then(Value::as_str))
                .filter_map(|name| HeaderName::from_bytes(name.as_bytes()).ok())
                // 未知的扩展头只做出站过滤，不据此修改请求正文
                .any(|name| is_request_marker(name.as_str()))
        })
}

// 只删除已观察到的指令开场白，保留其余文字、空白与内容块边界
fn normalize_instruction_identity(content: &mut Value) {
    let text = match content {
        Value::String(text) => Some(text),
        Value::Array(parts) => parts.first_mut().and_then(|part| {
            if part.get("type").and_then(Value::as_str) != Some("input_text") {
                return None;
            }
            match part.get_mut("text") {
                Some(Value::String(text)) => Some(text),
                _ => None,
            }
        }),
        _ => None,
    };
    if let Some(text) = text
        && text.starts_with(GROK_INTRODUCTION)
    {
        text.replace_range(..GROK_INTRODUCTION.len(), "");
    }
}
