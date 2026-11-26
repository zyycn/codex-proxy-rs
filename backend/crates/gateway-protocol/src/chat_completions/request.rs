//! Chat 公共请求校验、声明事实与字符串工具合同，选路前保持原文

use super::{
    ChatError,
    error::{invalid, unsupported},
};
use crate::capabilities::RequestFacts;
use serde_json::{Map, Value, json};

/// 仅解析当前协议，不构造目标平台报文
pub fn parse_request(bytes: &[u8]) -> Result<Map<String, Value>, ChatError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| ChatError::MalformedJson)?;
    let Value::Object(body) = value else {
        return Err(ChatError::ExpectedObject);
    };
    validate_request(&body)?;
    Ok(body)
}

/// 校验各目标共同遵守的 Chat 请求合同
pub fn validate_request(body: &Map<String, Value>) -> Result<(), ChatError> {
    string(body.get("model"), "model", false)?;
    let messages = body
        .get("messages")
        .ok_or_else(|| ChatError::MissingField {
            field: "messages".into(),
        })?
        .as_array()
        .ok_or_else(|| ChatError::InvalidType {
            field: "messages".into(),
            expected: "an array",
        })?;
    if messages.is_empty() {
        return Err(ChatError::EmptyField {
            field: "messages".into(),
        });
    }
    for (index, message) in messages.iter().enumerate() {
        let path = format!("messages[{index}]");
        let message = message.as_object().ok_or_else(|| invalid(&path))?;
        let role = string(message.get("role"), &format!("{path}.role"), false)?;
        if !matches!(role, "system" | "developer" | "user" | "assistant" | "tool") {
            return Err(unsupported(&format!("{path}.role")));
        }
        for (field, allowed) in [
            ("tool_calls", "assistant"),
            ("refusal", "assistant"),
            ("tool_call_id", "tool"),
        ] {
            if role != allowed && message.get(field).is_some_and(|v| !v.is_null()) {
                return Err(invalid(&format!("{path}.{field}")));
            }
        }
        for field in ["function_call", "audio"] {
            if message.get(field).is_some_and(|v| !v.is_null()) {
                return Err(unsupported(&format!("{path}.{field}")));
            }
        }
        for field in ["name", "refusal"] {
            if let Some(value) = message.get(field).filter(|v| !v.is_null()) {
                string(Some(value), &format!("{path}.{field}"), true)?;
            }
        }
        if role == "tool" {
            string(
                message.get("tool_call_id"),
                &format!("{path}.tool_call_id"),
                false,
            )?;
        }
        if let Some(content) = message.get("content").filter(|v| !v.is_null()) {
            if !content.is_string() && !content.is_array() {
                return Err(invalid(&format!("{path}.content")));
            }
            if let Some(parts) = content.as_array() {
                for (part_index, part) in parts.iter().enumerate() {
                    let part_path = format!("{path}.content[{part_index}]");
                    let kind = string(part.get("type"), &format!("{part_path}.type"), false)?;
                    match kind {
                        "text" | "input_text" | "output_text" => {
                            string(part.get("text"), &format!("{part_path}.text"), true)?;
                        }
                        "image_url" | "file" | "input_audio" | "input_image" | "input_file"
                            if matches!(role, "user" | "tool") => {}
                        "refusal" if role == "assistant" => {
                            string(part.get("refusal"), &format!("{part_path}.refusal"), true)?;
                        }
                        _ => return Err(unsupported(&format!("{part_path}.type"))),
                    }
                }
            }
        } else if role != "assistant"
            || (!message
                .get("tool_calls")
                .is_some_and(|v| v.as_array().is_some_and(|v| !v.is_empty()))
                && !message.get("refusal").is_some_and(Value::is_string))
        {
            return Err(invalid(&format!("{path}.content")));
        }
        if let Some(calls) = message.get("tool_calls").filter(|v| !v.is_null()) {
            for (call_index, call) in calls
                .as_array()
                .ok_or_else(|| invalid(&format!("{path}.tool_calls")))?
                .iter()
                .enumerate()
            {
                let call_path = format!("{path}.tool_calls[{call_index}]");
                string(call.get("id"), &format!("{call_path}.id"), false)?;
                let kind = string(call.get("type"), &format!("{call_path}.type"), false)?;
                if !matches!(kind, "function" | "custom") {
                    return Err(unsupported(&format!("{call_path}.type")));
                }
                string(
                    call.get(kind).and_then(|v| v.get("name")),
                    &format!("{call_path}.{kind}.name"),
                    false,
                )?;
                let data_field = if kind == "function" {
                    "arguments"
                } else {
                    "input"
                };
                string(
                    call.get(kind).and_then(|v| v.get(data_field)),
                    &format!("{call_path}.{kind}.{data_field}"),
                    true,
                )?;
            }
        }
    }
    for (field, value) in body.iter().filter(|(_, v)| !v.is_null()) {
        let valid = match field.as_str() {
            "n" => value.as_u64() == Some(1),
            "store" | "background" => value == false,
            "functions" => value.as_array().is_some_and(Vec::is_empty),
            "modalities" => value == &json!(["text"]),
            "audio" | "function_call" | "input" | "previous_response_id" | "conversation" => false,
            _ => true,
        };
        if !valid {
            return Err(unsupported(field));
        }
    }
    for field in ["stream", "parallel_tool_calls", "logprobs"] {
        if body
            .get(field)
            .is_some_and(|v| !v.is_null() && !v.is_boolean())
        {
            return Err(invalid(field));
        }
    }
    if let Some(options) = body.get("stream_options").filter(|v| !v.is_null()) {
        if body.get("stream") != Some(&Value::Bool(true)) || !options.is_object() {
            return Err(invalid("stream_options"));
        }
        for field in ["include_usage", "include_obfuscation"] {
            if options
                .get(field)
                .is_some_and(|v| !v.is_null() && !v.is_boolean())
            {
                return Err(invalid(&format!("stream_options.{field}")));
            }
        }
        if options.get("include_obfuscation") == Some(&Value::Bool(true)) {
            return Err(unsupported("stream_options.include_obfuscation"));
        }
    }
    Ok(())
}

pub(super) fn string<'a>(
    value: Option<&'a Value>,
    path: &str,
    empty: bool,
) -> Result<&'a str, ChatError> {
    let value = value
        .filter(|v| !v.is_null())
        .ok_or_else(|| ChatError::MissingField {
            field: path.to_owned(),
        })?;
    let value = value.as_str().ok_or_else(|| ChatError::InvalidType {
        field: path.to_owned(),
        expected: "a string",
    })?;
    if !empty && value.trim().is_empty() {
        return Err(ChatError::EmptyField {
            field: path.to_owned(),
        });
    }
    Ok(value)
}

pub(crate) fn request_facts(body: &Map<String, Value>) -> RequestFacts {
    RequestFacts {
        max_output_tokens: ["max_completion_tokens", "max_tokens", "max_output_tokens"]
            .into_iter()
            .find_map(|key| body.get(key).and_then(Value::as_u64)),
        tools: body
            .get("tools")
            .and_then(Value::as_array)
            .is_some_and(|v| !v.is_empty())
            || body.get("web_search_options").is_some_and(|v| !v.is_null()),
        // 只读取协议内容块，不将工具参数、扩展字段或 schema 示例解释为请求能力
        vision: body
            .get("messages")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|message| message.get("content").and_then(Value::as_array))
            .flatten()
            .any(|part| {
                matches!(
                    part.get("type").and_then(Value::as_str),
                    Some("image_url" | "input_image")
                )
            }),
        image_generation: body
            .get("tools")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|tool| tool.get("type").and_then(Value::as_str) == Some("image_generation")),
        reasoning: body.get("reasoning_effort").is_some_and(|v| !v.is_null())
            || body.get("reasoning").is_some_and(Value::is_object),
        json_schema: body
            .get("response_format")
            .and_then(|v| v.get("type"))
            .and_then(Value::as_str)
            == Some("json_schema")
            || body
                .get("text")
                .and_then(|v| v.pointer("/format/type"))
                .and_then(Value::as_str)
                == Some("json_schema"),
        ..RequestFacts::default()
    }
}

pub(crate) fn request_effort(body: &Map<String, Value>) -> Option<&str> {
    body.get("reasoning_effort")
        .and_then(Value::as_str)
        .or_else(|| {
            body.get("reasoning")
                .and_then(|value| value.get("effort"))
                .and_then(Value::as_str)
        })
}

/// 将无格式约束的 custom 工具表达为字符串参数，受约束格式必须由目标原生支持
pub fn custom_tool_schema(format: Option<&Value>, path: &str) -> Result<Value, ChatError> {
    if let Some(format) = format.filter(|value| !value.is_null()) {
        if !format.is_object() {
            return Err(invalid(path));
        }
        if string(format.get("type"), &format!("{path}.type"), false)? != "text" {
            return Err(unsupported(path));
        }
    }
    Ok(
        json!({"type":"object","properties":{"input":{"type":"string"}},"required":["input"],"additionalProperties":false}),
    )
}
