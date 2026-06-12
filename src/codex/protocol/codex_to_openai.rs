use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::codex::transport::sse::{parse_sse_events, SseError};

pub fn openai_error(message: &str, code: &str) -> Value {
    json!({
        "error": {
            "message": message,
            "type": "server_error",
            "param": null,
            "code": code
        }
    })
}

pub fn chat_completion_from_codex_sse(
    body: &str,
    model: &str,
    include_reasoning: bool,
) -> Result<Option<Value>, SseError> {
    let events = parse_sse_events(body)?;
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut function_call_items = BTreeMap::new();
    let mut finished_call_ids = BTreeSet::new();
    let mut tool_calls = Vec::new();
    let mut completed_response = None;
    for event in events {
        let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
            continue;
        };
        match event.event.as_deref() {
            Some("response.output_text.delta") => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    content.push_str(delta);
                }
            }
            Some("response.reasoning_summary_text.delta") if include_reasoning => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    reasoning.push_str(delta);
                }
            }
            Some("response.output_item.added") => {
                if let Some((item_id, call_id, name)) = function_call_item(value.get("item")) {
                    function_call_items.insert(item_id, FunctionCallInfo { call_id, name });
                }
            }
            Some("response.function_call_arguments.done") => {
                if let Some(tool_call) =
                    tool_call_from_done_event(&value, &function_call_items, &mut finished_call_ids)
                {
                    tool_calls.push(tool_call);
                }
            }
            Some("response.output_item.done") => {
                if let Some(tool_call) = tool_call_from_output_item(
                    value.get("item"),
                    &function_call_items,
                    &mut finished_call_ids,
                ) {
                    tool_calls.push(tool_call);
                }
            }
            Some("response.completed") => {
                completed_response = value.get("response").cloned();
            }
            _ => {}
        }
    }
    let Some(response) = completed_response else {
        return Ok(None);
    };
    if content.is_empty() {
        content = output_text_from_response(&response);
    }
    let mut message = json!({
        "role": "assistant",
        "content": if content.is_empty() && !tool_calls.is_empty() {
            Value::Null
        } else {
            Value::String(content)
        },
    });
    if include_reasoning && !reasoning.is_empty() {
        message["reasoning_content"] = Value::String(reasoning);
    }
    let has_tool_calls = !tool_calls.is_empty();
    if has_tool_calls {
        message["tool_calls"] = Value::Array(tool_calls);
    }

    Ok(Some(json!({
        "id": format!("chatcmpl-{}", Uuid::new_v4().simple()),
        "object": "chat.completion",
        "created": Utc::now().timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": if has_tool_calls { "tool_calls" } else { "stop" },
        }],
        "usage": openai_usage(response.get("usage")),
    })))
}

pub fn chat_completion_stream_from_codex_sse(
    body: &str,
    model: &str,
    include_reasoning: bool,
) -> Result<Option<String>, SseError> {
    let events = parse_sse_events(body)?;
    let chunk_id = format!("chatcmpl-{}", Uuid::new_v4().simple());
    let created = Utc::now().timestamp();
    let mut output = String::new();
    let mut completed_response = None;
    let mut has_tool_calls = false;
    let mut function_call_items = BTreeMap::new();
    let mut tool_call_indices = BTreeMap::new();
    let mut call_ids_with_deltas = BTreeSet::new();
    let mut next_tool_call_index = 0usize;

    push_sse_data(
        &mut output,
        json!({
            "id": chunk_id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {"role": "assistant"},
                "finish_reason": null,
            }],
        }),
    );

    for event in events {
        let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
            continue;
        };
        match event.event.as_deref() {
            Some("response.output_text.delta") => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    push_sse_data(
                        &mut output,
                        json!({
                            "id": chunk_id,
                            "object": "chat.completion.chunk",
                            "created": created,
                            "model": model,
                            "choices": [{
                                "index": 0,
                                "delta": {"content": delta},
                                "finish_reason": null,
                            }],
                        }),
                    );
                }
            }
            Some("response.reasoning_summary_text.delta") if include_reasoning => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    push_sse_data(
                        &mut output,
                        json!({
                            "id": chunk_id,
                            "object": "chat.completion.chunk",
                            "created": created,
                            "model": model,
                            "choices": [{
                                "index": 0,
                                "delta": {"reasoning_content": delta},
                                "finish_reason": null,
                            }],
                        }),
                    );
                }
            }
            Some("response.output_item.added") => {
                if let Some((item_id, call_id, name)) = function_call_item(value.get("item")) {
                    has_tool_calls = true;
                    let index = next_tool_call_index;
                    next_tool_call_index = next_tool_call_index.saturating_add(1);
                    function_call_items.insert(
                        item_id,
                        FunctionCallInfo {
                            call_id: call_id.clone(),
                            name: name.clone(),
                        },
                    );
                    tool_call_indices.insert(call_id.clone(), index);
                    push_sse_data(
                        &mut output,
                        json!({
                            "id": chunk_id,
                            "object": "chat.completion.chunk",
                            "created": created,
                            "model": model,
                            "choices": [{
                                "index": 0,
                                "delta": {
                                    "tool_calls": [{
                                        "index": index,
                                        "id": call_id,
                                        "type": "function",
                                        "function": {
                                            "name": name,
                                            "arguments": "",
                                        },
                                    }],
                                },
                                "finish_reason": null,
                            }],
                        }),
                    );
                }
            }
            Some("response.function_call_arguments.delta") => {
                if let Some((call_id, index, delta)) =
                    stream_tool_call_delta(&value, &function_call_items, &tool_call_indices)
                {
                    has_tool_calls = true;
                    call_ids_with_deltas.insert(call_id);
                    push_sse_data(
                        &mut output,
                        json!({
                            "id": chunk_id,
                            "object": "chat.completion.chunk",
                            "created": created,
                            "model": model,
                            "choices": [{
                                "index": 0,
                                "delta": {
                                    "tool_calls": [{
                                        "index": index,
                                        "function": {
                                            "arguments": delta,
                                        },
                                    }],
                                },
                                "finish_reason": null,
                            }],
                        }),
                    );
                }
            }
            Some("response.function_call_arguments.done") => {
                if let Some((call_id, index, arguments)) = stream_tool_call_done(
                    &value,
                    &function_call_items,
                    &tool_call_indices,
                    &call_ids_with_deltas,
                ) {
                    has_tool_calls = true;
                    push_sse_data(
                        &mut output,
                        json!({
                            "id": chunk_id,
                            "object": "chat.completion.chunk",
                            "created": created,
                            "model": model,
                            "choices": [{
                                "index": 0,
                                "delta": {
                                    "tool_calls": [{
                                        "index": index,
                                        "function": {
                                            "arguments": arguments,
                                        },
                                    }],
                                },
                                "finish_reason": null,
                            }],
                        }),
                    );
                    call_ids_with_deltas.insert(call_id);
                }
            }
            Some("response.output_item.done") if is_function_call_item(value.get("item")) => {
                has_tool_calls = true;
            }
            Some("response.completed") => {
                completed_response = value.get("response").cloned();
            }
            _ => {}
        }
    }

    let Some(response) = completed_response else {
        return Ok(None);
    };

    push_sse_data(
        &mut output,
        json!({
            "id": chunk_id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": if has_tool_calls { "tool_calls" } else { "stop" },
            }],
            "usage": openai_usage(response.get("usage")),
        }),
    );
    output.push_str("data: [DONE]\n\n");

    Ok(Some(output))
}

fn output_text_from_response(response: &Value) -> String {
    response
        .get("output_text")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .unwrap_or_else(|| {
            response
                .get("output")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(output_text_from_item)
                        .collect::<Vec<_>>()
                        .join("\n\n")
                })
                .unwrap_or_default()
        })
}

fn output_text_from_item(item: &Value) -> Option<String> {
    let content = item.get("content")?.as_array()?;
    let text = content
        .iter()
        .filter_map(|part| {
            let part_type = part.get("type")?.as_str()?;
            if part_type != "output_text" && part_type != "text" {
                return None;
            }
            part.get("text")?.as_str()
        })
        .collect::<String>();
    (!text.is_empty()).then_some(text)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FunctionCallInfo {
    call_id: String,
    name: String,
}

fn function_call_item(item: Option<&Value>) -> Option<(String, String, String)> {
    let item = item?;
    if item.get("type").and_then(Value::as_str) != Some("function_call") {
        return None;
    }
    let item_id = item.get("id").and_then(Value::as_str)?;
    let call_id = item.get("call_id").and_then(Value::as_str)?;
    let name = item.get("name").and_then(Value::as_str)?;
    Some((item_id.to_string(), call_id.to_string(), name.to_string()))
}

fn tool_call_from_done_event(
    event: &Value,
    function_call_items: &BTreeMap<String, FunctionCallInfo>,
    finished_call_ids: &mut BTreeSet<String>,
) -> Option<Value> {
    let event_id = event
        .get("call_id")
        .or_else(|| event.get("item_id"))
        .and_then(Value::as_str)?;
    let info = function_call_items.get(event_id);
    let call_id = info
        .map(|info| info.call_id.as_str())
        .unwrap_or(event_id)
        .to_string();
    if !finished_call_ids.insert(call_id.clone()) {
        return None;
    }
    let name = event
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .or_else(|| info.map(|info| info.name.as_str()))
        .unwrap_or("unknown");
    let arguments = event
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Some(openai_tool_call(call_id, name, arguments))
}

fn tool_call_from_output_item(
    item: Option<&Value>,
    function_call_items: &BTreeMap<String, FunctionCallInfo>,
    finished_call_ids: &mut BTreeSet<String>,
) -> Option<Value> {
    let item = item?;
    if item.get("type").and_then(Value::as_str) != Some("function_call") {
        return None;
    }
    let item_id = item.get("id").and_then(Value::as_str);
    let info = item_id.and_then(|item_id| function_call_items.get(item_id));
    let call_id = item
        .get("call_id")
        .and_then(Value::as_str)
        .or_else(|| info.map(|info| info.call_id.as_str()))
        .or(item_id)?;
    if !finished_call_ids.insert(call_id.to_string()) {
        return None;
    }
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .or_else(|| info.map(|info| info.name.as_str()))
        .unwrap_or("unknown");
    let arguments = item
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Some(openai_tool_call(call_id.to_string(), name, arguments))
}

fn openai_tool_call(call_id: String, name: &str, arguments: &str) -> Value {
    json!({
        "id": call_id,
        "type": "function",
        "function": {
            "name": name,
            "arguments": arguments,
        },
    })
}

fn stream_tool_call_delta(
    event: &Value,
    function_call_items: &BTreeMap<String, FunctionCallInfo>,
    tool_call_indices: &BTreeMap<String, usize>,
) -> Option<(String, usize, String)> {
    let event_id = event
        .get("call_id")
        .or_else(|| event.get("item_id"))
        .and_then(Value::as_str)?;
    let info = function_call_items.get(event_id);
    let call_id = info
        .map(|info| info.call_id.as_str())
        .unwrap_or(event_id)
        .to_string();
    let index = *tool_call_indices.get(&call_id).unwrap_or(&0);
    let delta = event.get("delta").and_then(Value::as_str)?.to_string();
    Some((call_id, index, delta))
}

fn stream_tool_call_done(
    event: &Value,
    function_call_items: &BTreeMap<String, FunctionCallInfo>,
    tool_call_indices: &BTreeMap<String, usize>,
    call_ids_with_deltas: &BTreeSet<String>,
) -> Option<(String, usize, String)> {
    let event_id = event
        .get("call_id")
        .or_else(|| event.get("item_id"))
        .and_then(Value::as_str)?;
    let info = function_call_items.get(event_id);
    let call_id = info
        .map(|info| info.call_id.as_str())
        .unwrap_or(event_id)
        .to_string();
    if call_ids_with_deltas.contains(&call_id) {
        return None;
    }
    let index = *tool_call_indices.get(&call_id).unwrap_or(&0);
    let arguments = event
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Some((call_id, index, arguments))
}

fn push_sse_data(output: &mut String, value: Value) {
    output.push_str("data: ");
    output.push_str(&value.to_string());
    output.push_str("\n\n");
}

fn is_function_call_item(item: Option<&Value>) -> bool {
    item.and_then(|item| item.get("type"))
        .and_then(Value::as_str)
        == Some("function_call")
}

fn openai_usage(usage: Option<&Value>) -> Value {
    let prompt_tokens = usage
        .and_then(|usage| usage.get("input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let completion_tokens = usage
        .and_then(|usage| usage.get("output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let cached_tokens = usage
        .and_then(|usage| usage.get("input_tokens_details"))
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let reasoning_tokens = usage
        .and_then(|usage| usage.get("output_tokens_details"))
        .and_then(|details| details.get("reasoning_tokens"))
        .and_then(Value::as_u64);
    let mut openai_usage = json!({
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
        "total_tokens": prompt_tokens.saturating_add(completion_tokens),
        "prompt_tokens_details": {
            "cached_tokens": cached_tokens,
        },
    });
    if let Some(reasoning_tokens) = reasoning_tokens {
        openai_usage["completion_tokens_details"] = json!({
            "reasoning_tokens": reasoning_tokens,
        });
    }
    openai_usage
}
