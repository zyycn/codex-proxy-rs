//! Messages 与 Responses 请求的无状态转换

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use super::{
    MessagesModelCapabilities, TranslationError, content, thinking,
    tools::{ToolSet, messages_tools},
};

/// 原生 Messages 上游不能消费本网关封装的 OpenAI 思考，仅删除已知外来源载荷
pub fn strip_foreign_thinking_for_messages(request: &mut Map<String, Value>) {
    let Some(messages) = request.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    messages.retain_mut(|message| {
        let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) else {
            return true;
        };
        let previous_len = blocks.len();
        blocks.retain(|block| !content::is_openai_carrier(block));
        blocks.len() == previous_len || !blocks.is_empty()
    });
}

/// 原生 Responses 上游不能消费本网关封装的 Anthropic 思考
pub fn strip_foreign_reasoning_for_responses(request: &mut Map<String, Value>) {
    if let Some(input) = request.get_mut("input").and_then(Value::as_array_mut) {
        input.retain(|item| !content::is_anthropic_carrier(item));
    }
}

/// 校验 Messages 生成请求的必需字段
pub fn validate_request(request: &Map<String, Value>) -> Result<(), TranslationError> {
    validate_count_request(request)?;
    if request.get("max_tokens").and_then(Value::as_u64).is_none() {
        return Err(TranslationError::InvalidRequest(
            "The max_tokens field must be a non-negative integer.",
        ));
    }
    Ok(())
}

/// 计数请求没有生成预算要求
pub fn validate_count_request(request: &Map<String, Value>) -> Result<(), TranslationError> {
    if !request
        .get("model")
        .and_then(Value::as_str)
        .is_some_and(|v| !v.trim().is_empty())
    {
        return Err(TranslationError::InvalidRequest(
            "The model field must be a non-empty string.",
        ));
    }
    let messages = request.get("messages").and_then(Value::as_array).ok_or(
        TranslationError::InvalidRequest("The messages field must be an array."),
    )?;
    if messages.is_empty() {
        return Err(TranslationError::InvalidRequest(
            "The messages field must not be empty.",
        ));
    }
    for message in messages {
        if !message
            .get("role")
            .and_then(Value::as_str)
            .is_some_and(|v| matches!(v, "user" | "assistant" | "system"))
        {
            return Err(TranslationError::InvalidRequest(
                "Each message role must be user, assistant, or system.",
            ));
        }
        if message
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(|blocks| {
                blocks
                    .iter()
                    .any(|block| block.get("type").and_then(Value::as_str).is_none())
            })
        {
            return Err(TranslationError::InvalidRequest(
                "Each content block must contain a type string.",
            ));
        }
        if !message
            .get("content")
            .is_some_and(|v| v.is_string() || v.is_array())
        {
            return Err(TranslationError::InvalidRequest(
                "Each message content must be a string or an array.",
            ));
        }
    }
    if request.get("stream").is_some_and(|v| !v.is_boolean()) {
        return Err(TranslationError::InvalidRequest(
            "The stream field must be a boolean.",
        ));
    }
    if request.get("tools").is_some_and(|v| !v.is_array()) {
        return Err(TranslationError::InvalidRequest(
            "The tools field must be an array.",
        ));
    }
    Ok(())
}

/// Messages 请求转换为 Responses，外来 thinking 仅通过明确来源载荷恢复
pub fn messages_to_responses_request(
    request: &Map<String, Value>,
) -> Result<Map<String, Value>, TranslationError> {
    validate_request(request)?;
    if request
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| {
            tools.iter().any(|tool| {
                tool["type"]
                    .as_str()
                    .is_some_and(|kind| kind.starts_with("web_search_"))
                    && !tool["max_uses"].is_null()
            })
        })
    {
        return Err(TranslationError::InvalidRequest(
            "The Responses upstream cannot enforce the web search usage limit.",
        ));
    }
    messages_to_responses_body(request)
}

/// 计数请求复用内容转换，不引入生成预算
pub fn messages_to_responses_count_request(
    request: &Map<String, Value>,
) -> Result<Map<String, Value>, TranslationError> {
    validate_count_request(request)?;
    messages_to_responses_body(request)
}

fn messages_to_responses_body(
    request: &Map<String, Value>,
) -> Result<Map<String, Value>, TranslationError> {
    let mut result = Map::new();
    copy_fields(
        request,
        &mut result,
        &["model", "stream", "temperature", "top_p"],
    );
    // speed 与服务档位不是同一概念，按 CPA 映射显式 fast，其余使用目标协议的档位
    if request.get("speed").and_then(Value::as_str) == Some("fast") {
        result.insert("service_tier".into(), json!("priority"));
    } else if let Some(tier) = request.get("service_tier").and_then(Value::as_str) {
        match tier {
            "auto" => {
                result.insert("service_tier".into(), json!("auto"));
            }
            "standard_only" => {
                result.insert("service_tier".into(), json!("default"));
            }
            _ => {}
        }
    }
    if let Some(max_tokens) = request.get("max_tokens") {
        result.insert("max_output_tokens".into(), max_tokens.clone());
    }
    let mut input = Vec::new();
    let system = content::blocks(request.get("system"))
        .iter()
        .filter_map(|block| content::to_responses_content(block, false))
        .collect::<Vec<_>>();
    if !system.is_empty() {
        input.push(json!({"type":"message","role":"developer","content":system}));
    }
    for message in request
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        let mut parts = Vec::new();
        for block in content::blocks(message.get("content")) {
            match block
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default()
            {
                "tool_use" => {
                    flush_responses(&mut input, role, &mut parts);
                    input.push(json!({"type":"function_call","call_id":block.get("id"),"name":block.get("name"),"arguments":block.get("input").cloned().unwrap_or_else(||json!({})).to_string()}));
                }
                "tool_result" => {
                    flush_responses(&mut input, role, &mut parts);
                    let output = tool_output_to_responses(block.get("content"));
                    input.push(json!({"type":"function_call_output","call_id":block.get("tool_use_id"),"output":output}));
                }
                "thinking" | "redacted_thinking" => {
                    flush_responses(&mut input, role, &mut parts);
                    if let Some(item) = content::restore_openai(&block) {
                        input.push(item);
                    }
                }
                _ => {
                    if let Some(part) = content::to_responses_content(&block, role == "assistant") {
                        parts.push(part);
                    }
                }
            }
        }
        flush_responses(&mut input, role, &mut parts);
    }
    result.insert("input".into(), Value::Array(input));
    if let Some(tools) = request.get("tools").and_then(Value::as_array) {
        result.insert("tools".into(), Value::Array(messages_tools(tools)));
    }
    if let Some(choice) = request.get("tool_choice") {
        let choice = match choice.get("type").and_then(Value::as_str).unwrap_or("auto") {
            "any" => json!("required"),
            "none" => json!("none"),
            "tool" => json!({"type":"function","name":choice.get("name")}),
            _ => json!("auto"),
        };
        result.insert("tool_choice".into(), choice);
    }
    if let Some(disabled) = request
        .get("tool_choice")
        .and_then(|v| v.get("disable_parallel_tool_use"))
        .and_then(Value::as_bool)
    {
        result.insert("parallel_tool_calls".into(), json!(!disabled));
    }
    result.insert("reasoning".into(), thinking::to_responses(request));
    result.insert("include".into(), json!(["reasoning.encrypted_content"]));
    if let Some(format) = request.get("output_config").and_then(|v| v.get("format")) {
        let mut format = format.clone();
        if format.get("type").and_then(Value::as_str) == Some("json_schema") {
            format["name"] = json!("response");
        }
        result.insert("text".into(), json!({"format":format}));
    }
    Ok(result)
}

/// Responses 请求转换为 Messages，工具结果采用 CPA 的配对与孤立结果降级规则
pub fn responses_to_messages_request(
    request: &Map<String, Value>,
    capabilities: &MessagesModelCapabilities,
) -> Result<Map<String, Value>, TranslationError> {
    let tools = ToolSet::from_responses(request);
    let mut result = Map::new();
    copy_fields(
        request,
        &mut result,
        &["model", "stream", "temperature", "top_p"],
    );
    let requested_maximum =
        match request.get("max_output_tokens") {
            None | Some(Value::Null) => 32000,
            Some(value) => value.as_u64().filter(|value| *value > 0).ok_or(
                TranslationError::InvalidRequest(
                    "The max_output_tokens field must be a positive integer.",
                ),
            )?,
        };
    let maximum = capabilities
        .max_tokens
        .map_or(requested_maximum, |limit| requested_maximum.min(limit));
    result.insert("max_tokens".into(), json!(maximum));
    match request.get("service_tier").and_then(Value::as_str) {
        Some("priority") => {
            result.insert("speed".into(), json!("fast"));
        }
        Some("default") => {
            result.insert("service_tier".into(), json!("standard_only"));
        }
        Some("auto") => {
            result.insert("service_tier".into(), json!("auto"));
        }
        _ => {}
    }
    let mut system = Vec::new();
    if let Some(instructions) = request.get("instructions").and_then(Value::as_str)
        && !instructions.is_empty()
    {
        system.push(json!({"type":"text","text":instructions}));
    }
    let mut messages = Vec::new();
    let inputs = match request.get("input") {
        Some(Value::String(input)) => {
            append_message(
                &mut messages,
                "user",
                vec![json!({"type":"text","text":input})],
            );
            &[][..]
        }
        Some(Value::Array(input)) => input.as_slice(),
        Some(Value::Null) | None => &[],
        _ => {
            return Err(TranslationError::InvalidRequest(
                "The input field must be a string or an array.",
            ));
        }
    };
    for (position, item) in inputs.iter().enumerate() {
        match item
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("message")
        {
            "function_call" | "custom_tool_call" => {
                let custom = item.get("type").and_then(Value::as_str) == Some("custom_tool_call");
                let input = if custom {
                    json!({"input":item.get("input").and_then(Value::as_str).unwrap_or_default()})
                } else {
                    item.get("arguments")
                        .and_then(Value::as_str)
                        .and_then(|v| serde_json::from_str::<Value>(v).ok())
                        .filter(Value::is_object)
                        .unwrap_or_else(|| json!({}))
                };
                let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
                let wire_name =
                    tools.wire_name(name, item.get("namespace").and_then(Value::as_str));
                append_message(
                    &mut messages,
                    "assistant",
                    vec![
                        json!({"type":"tool_use","id":item.get("call_id").cloned().unwrap_or_else(||json!(format!("call_{position}"))),"name":wire_name,"input":input}),
                    ],
                );
            }
            "function_call_output" | "custom_tool_call_output" => {
                let call_id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let output = item.get("output").unwrap_or(&Value::Null);
                let mut block = json!({"type":"tool_result","tool_use_id":call_id,"content":tool_output_to_messages(output)});
                if item.get("status").and_then(Value::as_str) == Some("incomplete") {
                    block["is_error"] = json!(true);
                }
                append_message(&mut messages, "user", vec![block]);
            }
            "reasoning" => {
                if let Some(block) = content::restore_anthropic(item) {
                    append_message(&mut messages, "assistant", vec![block]);
                }
            }
            "message" => {
                let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
                let parts = match item.get("content") {
                    Some(Value::String(text)) => vec![json!({"type":"text","text":text})],
                    Some(Value::Array(parts)) => parts
                        .iter()
                        .filter_map(content::to_messages_content)
                        .collect(),
                    _ => Vec::new(),
                };
                if matches!(role, "system" | "developer") {
                    system.extend(parts);
                } else {
                    append_message(&mut messages, role, parts);
                }
            }
            _ => {}
        }
    }
    if !system.is_empty() {
        result.insert("system".into(), Value::Array(system));
    }
    result.insert(
        "messages".into(),
        Value::Array(normalize_tool_pairing(messages)),
    );
    tools.write_messages_tools(request, &mut result)?;
    thinking::apply_to_messages(request, &mut result, capabilities, maximum);
    if request
        .get("tool_choice")
        .is_some_and(|c| c["type"] == "allowed_tools" && c["mode"] == "required")
        && result["tool_choice"]["type"] != "any"
    {
        return Err(TranslationError::InvalidRequest(
            "The Messages upstream cannot require an allowed tool with this thinking mode.",
        ));
    }
    if matches!(
        result
            .get("thinking")
            .and_then(|value| value.get("type"))
            .and_then(Value::as_str),
        Some("enabled" | "adaptive")
    ) {
        // CPA 在生成推理配置后移除不相容的采样参数；原生 Messages 不经过此转换
        result.shift_remove("temperature");
        result.shift_remove("top_p");
    }
    if let Some(format) = request.get("text").and_then(|v| v.get("format"))
        && format.get("type").and_then(Value::as_str) == Some("json_schema")
    {
        let config = result.entry("output_config").or_insert_with(|| json!({}));
        config["format"] = json!({"type":"json_schema","schema":format.get("schema").cloned().unwrap_or_else(||json!({}))});
    }
    Ok(result)
}

fn copy_fields(source: &Map<String, Value>, target: &mut Map<String, Value>, fields: &[&str]) {
    for field in fields {
        if let Some(value) = source.get(*field) {
            target.insert((*field).into(), value.clone());
        }
    }
}

fn flush_responses(input: &mut Vec<Value>, role: &str, parts: &mut Vec<Value>) {
    if !parts.is_empty() {
        input.push(json!({"type":"message","role":role,"content":std::mem::take(parts)}));
    }
}

fn append_message(messages: &mut Vec<Value>, role: &str, parts: Vec<Value>) {
    if parts.is_empty() {
        return;
    }
    if let Some(last) = messages.last_mut()
        && last.get("role").and_then(Value::as_str) == Some(role)
        && let Some(content) = last.get_mut("content").and_then(Value::as_array_mut)
    {
        content.extend(parts);
        return;
    }
    messages.push(json!({"role":role,"content":parts}));
}

fn tool_output_to_responses(value: Option<&Value>) -> Value {
    match value {
        Some(Value::String(text)) => json!(text),
        Some(Value::Array(blocks)) => Value::Array(
            blocks
                .iter()
                .filter_map(|block| content::to_responses_content(block, false))
                .collect(),
        ),
        Some(value) => json!(value.to_string()),
        None => json!(""),
    }
}

fn tool_output_to_messages(value: &Value) -> Value {
    match value {
        Value::String(text) => json!(text),
        Value::Array(parts) => Value::Array(
            parts
                .iter()
                .filter_map(content::to_messages_content)
                .collect(),
        ),
        _ => json!(value.to_string()),
    }
}

// Messages 要求每次工具调用在紧邻用户轮次闭合，不能用历史中的任意同 ID 调用配对
fn normalize_tool_pairing(messages: Vec<Value>) -> Vec<Value> {
    let mut normalized = Vec::with_capacity(messages.len());
    let mut pending = Vec::<String>::new();
    for mut message in messages {
        if message["role"] == "user" {
            if let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) {
                normalize_tool_results(blocks, &pending);
            }
            pending.clear();
        } else {
            if !pending.is_empty() {
                normalized.push(interrupted_tools_message(&pending));
            }
            pending = message
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|block| message["role"] == "assistant" && block["type"] == "tool_use")
                .filter_map(|block| block.get("id").and_then(Value::as_str))
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
                .collect();
        }
        normalized.push(message);
    }
    if !pending.is_empty() {
        normalized.push(interrupted_tools_message(&pending));
    }
    normalized
}

fn normalize_tool_results(blocks: &mut Vec<Value>, pending: &[String]) {
    // 只在本轮保留同 ID 的最后一个结果；延迟结果不能抹掉更早轮次已经闭合的调用
    let mut seen = BTreeSet::new();
    blocks.reverse();
    blocks.retain(|block| tool_result_id(block).is_none_or(|id| seen.insert(id.to_owned())));
    blocks.reverse();

    let mut unanswered = pending.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let mut results = Vec::new();
    let mut ordinary = Vec::new();
    for mut block in std::mem::take(blocks) {
        match tool_result_id(&block) {
            Some(id) if unanswered.remove(id) => results.push(block),
            Some(_) => {
                let content = block.get_mut("content").map(Value::take);
                let parts = content::blocks(content.as_ref())
                    .into_iter()
                    .filter(|part| {
                        part["type"] != "text"
                            || part
                                .get("text")
                                .and_then(Value::as_str)
                                .is_some_and(|text| !text.trim().is_empty())
                    })
                    .collect::<Vec<_>>();
                if parts.is_empty() {
                    ordinary.push(json!({"type":"text","text":"Tool result was empty."}));
                } else {
                    ordinary.extend(parts);
                }
            }
            None => ordinary.push(block),
        }
    }
    blocks.extend(
        pending
            .iter()
            .filter(|id| unanswered.contains(id.as_str()))
            .map(|id| interrupted_tool_result(id)),
    );
    blocks.extend(results);
    blocks.extend(ordinary);
}

fn tool_result_id(block: &Value) -> Option<&str> {
    (block["type"] == "tool_result")
        .then(|| block.get("tool_use_id").and_then(Value::as_str))
        .flatten()
}

fn interrupted_tool_result(id: &str) -> Value {
    json!({"type":"tool_result","tool_use_id":id,"is_error":true,"content":"Tool call was interrupted before any output was recorded."})
}

fn interrupted_tools_message(pending: &[String]) -> Value {
    json!({"role":"user","content":pending.iter().map(|id| interrupted_tool_result(id)).collect::<Vec<_>>()})
}
