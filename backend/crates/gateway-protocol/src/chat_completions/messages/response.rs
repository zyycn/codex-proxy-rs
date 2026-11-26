//! Messages 完整响应直接投影为 Chat 响应

use crate::anthropic::MessageUsage;
use crate::chat_completions::{
    ChatError, ChatIdentity,
    response::{completion, custom_tools, tool_call},
};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;

/// 无 custom 工具上下文时的完整响应投影
pub fn project_response(message: &Value, identity: &ChatIdentity) -> Result<Value, ChatError> {
    project(message, identity, &BTreeSet::new())
}

/// 恢复 Chat 请求声明的 custom 工具语义
pub fn project_response_with_request(
    message: &Value,
    identity: &ChatIdentity,
    request: &Map<String, Value>,
) -> Result<Value, ChatError> {
    project(message, identity, &custom_tools(request))
}

fn project(
    message: &Value,
    identity: &ChatIdentity,
    custom: &BTreeSet<String>,
) -> Result<Value, ChatError> {
    if let Some(error) = message.get("error") {
        return Ok(json!({"error":error}));
    }
    if message["type"] != "message" || message["role"] != "assistant" {
        return Err(ChatError::InvalidResponse);
    }
    let reason = finish_reason(
        message
            .get("stop_reason")
            .and_then(Value::as_str)
            .ok_or(ChatError::InvalidResponse)?,
    )?;
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut calls = Vec::new();
    let mut annotations = Vec::new();
    let mut ids = BTreeSet::new();
    for block in message
        .get("content")
        .and_then(Value::as_array)
        .ok_or(ChatError::InvalidResponse)?
    {
        match block
            .get("type")
            .and_then(Value::as_str)
            .ok_or(ChatError::InvalidResponse)?
        {
            "text" => {
                let fragment = block
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or(ChatError::InvalidResponse)?;
                let start = text.chars().count();
                text.push_str(fragment);
                for citation in block
                    .get("citations")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(citation) = annotation(citation, start, text.chars().count()) {
                        annotations.push(citation);
                    }
                }
            }
            "thinking" => reasoning.push_str(
                block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .ok_or(ChatError::InvalidResponse)?,
            ),
            "tool_use" => {
                let id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|v| !v.is_empty())
                    .ok_or(ChatError::InvalidResponse)?;
                if !ids.insert(id) {
                    return Err(ChatError::InvalidResponse);
                }
                calls.push(tool_call(
                    id,
                    block
                        .get("name")
                        .and_then(Value::as_str)
                        .filter(|v| !v.is_empty())
                        .ok_or(ChatError::InvalidResponse)?,
                    &block["input"],
                    custom,
                )?);
            }
            "redacted_thinking" | "server_tool_use" | "web_search_tool_result" => {}
            _ => return Err(ChatError::InvalidResponse),
        }
    }
    let mut output = json!({"role":"assistant","content":if text.is_empty(){Value::Null}else{json!(text)},"refusal":null});
    if !reasoning.is_empty() {
        output["reasoning_content"] = json!(reasoning);
    }
    if !calls.is_empty() {
        output["tool_calls"] = calls.into();
    }
    if !annotations.is_empty() {
        output["annotations"] = annotations.into();
    }
    Ok(completion(
        identity,
        output,
        reason,
        usage(MessageUsage::from_value(&message["usage"])),
    ))
}

pub(super) fn annotation(citation: &Value, start: usize, end: usize) -> Option<Value> {
    let url = citation.get("url")?.as_str()?;
    Some(
        json!({"type":"url_citation","url_citation":{"url":url,"title":citation.get("title").and_then(Value::as_str).unwrap_or_default(),"start_index":start,"end_index":end}}),
    )
}

pub(super) fn finish_reason(reason: &str) -> Result<&'static str, ChatError> {
    match reason {
        "end_turn" | "stop_sequence" | "pause_turn" => Ok("stop"),
        "tool_use" => Ok("tool_calls"),
        "max_tokens" | "model_context_window_exceeded" => Ok("length"),
        "refusal" => Ok("content_filter"),
        _ => Err(ChatError::InvalidResponse),
    }
}

pub(super) fn usage(usage: MessageUsage) -> Option<Value> {
    let input = usage.total_input_tokens()?;
    let output = usage.output_tokens?;
    let mut value = json!({"prompt_tokens":input,"completion_tokens":output,"total_tokens":input.saturating_add(output)});
    if let Some(cached) = usage.cache_read_input_tokens {
        value["prompt_tokens_details"]["cached_tokens"] = json!(cached);
    }
    if let Some(written) = usage.cache_creation_input_tokens {
        value["prompt_tokens_details"]["cache_write_tokens"] = json!(written);
    }
    Some(value)
}
