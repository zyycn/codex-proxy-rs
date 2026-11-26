//! Chat 字段、内容和工具历史直接构造 Messages 正文

use crate::anthropic::MessagesModelCapabilities;
use crate::chat_completions::{
    ChatError,
    error::{invalid, unsupported},
    request::string,
    validate_request,
};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// 模型能力来自 Provider 已选模型，不按名称猜测
pub fn convert_request(
    chat: &Map<String, Value>,
    capabilities: &MessagesModelCapabilities,
) -> Result<Map<String, Value>, ChatError> {
    validate_request(chat)?;
    let max_tokens = ["max_completion_tokens", "max_tokens", "max_output_tokens"]
        .into_iter()
        .find_map(|key| chat.get(key).filter(|v| !v.is_null()))
        .map(|value| {
            value
                .as_u64()
                .filter(|v| *v > 0)
                .ok_or_else(|| invalid("max_completion_tokens"))
        })
        .transpose()?
        .unwrap_or(32_000)
        .min(capabilities.max_tokens.unwrap_or(u64::MAX));
    let mut result = Map::from_iter([
        ("model".into(), chat["model"].clone()),
        ("max_tokens".into(), json!(max_tokens)),
    ]);
    for field in ["stream", "temperature", "top_p"] {
        if let Some(value) = chat.get(field).filter(|v| !v.is_null()) {
            result.insert(field.into(), value.clone());
        }
    }
    if let Some(tier) = chat.get("service_tier").and_then(Value::as_str) {
        match tier {
            "priority" => {
                result.insert("speed".into(), json!("fast"));
            }
            "default" => {
                result.insert("service_tier".into(), json!("standard_only"));
            }
            "auto" => {
                result.insert("service_tier".into(), json!("auto"));
            }
            _ => return Err(unsupported("service_tier")),
        }
    }
    let mut system = Vec::new();
    if let Some(instructions) = chat.get("instructions").filter(|v| !v.is_null()) {
        system.extend(content(instructions, "instructions", true)?);
    }
    let mut messages = Vec::new();
    let mut calls = BTreeMap::new();
    let mut used = BTreeSet::new();
    for (index, message) in chat["messages"]
        .as_array()
        .ok_or_else(|| invalid("messages"))?
        .iter()
        .enumerate()
    {
        let path = format!("messages[{index}]");
        let role = message["role"].as_str().ok_or_else(|| invalid(&path))?;
        if matches!(role, "system" | "developer") {
            system.extend(content(
                &message["content"],
                &format!("{path}.content"),
                true,
            )?);
            continue;
        }
        let blocks = if role == "tool" {
            let id = string(
                message.get("tool_call_id"),
                &format!("{path}.tool_call_id"),
                false,
            )?;
            if !calls.contains_key(id) || !used.insert(id.to_owned()) {
                return Err(invalid(&format!("{path}.tool_call_id")));
            }
            vec![
                json!({"type":"tool_result","tool_use_id":id,"content":content(&message["content"], &format!("{path}.content"), false)?}),
            ]
        } else {
            let mut blocks = if message["content"].is_null() {
                Vec::new()
            } else {
                content(&message["content"], &format!("{path}.content"), false)?
            };
            if let Some(refusal) = message.get("refusal").and_then(Value::as_str) {
                blocks.push(json!({"type":"text","text":refusal}));
            }
            for (call_index, call) in message
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
            {
                let call_path = format!("{path}.tool_calls[{call_index}]");
                let id = string(call.get("id"), &format!("{call_path}.id"), false)?;
                let kind = call["type"].as_str().ok_or_else(|| invalid(&call_path))?;
                let name = string(
                    call[kind].get("name"),
                    &format!("{call_path}.{kind}.name"),
                    false,
                )?;
                if calls.insert(id.to_owned(), name.to_owned()).is_some() {
                    return Err(invalid(&format!("{call_path}.id")));
                }
                let input = if kind == "custom" {
                    json!({"input":call[kind]["input"]})
                } else {
                    serde_json::from_str::<Value>(string(
                        call[kind].get("arguments"),
                        &format!("{call_path}.function.arguments"),
                        true,
                    )?)
                    .map_err(|_| invalid(&format!("{call_path}.function.arguments")))?
                };
                if !input.is_object() {
                    return Err(invalid(&format!("{call_path}.function.arguments")));
                }
                blocks.push(json!({"type":"tool_use","id":id,"name":name,"input":input}));
            }
            blocks
        };
        append_message(
            &mut messages,
            if role == "assistant" {
                "assistant"
            } else {
                "user"
            },
            blocks,
        );
    }
    if messages.is_empty() {
        return Err(invalid("messages"));
    }
    if !system.is_empty() {
        result.insert("system".into(), system.into());
    }
    result.insert("messages".into(), messages.into());
    tools(chat, &mut result, capabilities)?;
    let format = chat
        .get("response_format")
        .or_else(|| chat.get("text").and_then(|v| v.get("format")));
    if let Some(format) = format.filter(|v| !v.is_null()) {
        match format["type"].as_str() {
            Some("json_schema") => {
                let schema = format
                    .get("json_schema")
                    .unwrap_or(format)
                    .get("schema")
                    .filter(|v| v.is_object())
                    .ok_or_else(|| invalid("response_format.json_schema.schema"))?;
                result.insert(
                    "output_config".into(),
                    json!({"format":{"type":"json_schema","schema":schema}}),
                );
            }
            Some("json_object") => {
                result.insert(
                    "output_config".into(),
                    json!({"format":{"type":"json_schema","schema":{"type":"object"}}}),
                );
            }
            Some("text") => {}
            _ => return Err(unsupported("response_format.type")),
        }
    }
    reasoning(chat, &mut result, capabilities, max_tokens)?;
    Ok(result)
}

fn append_message(messages: &mut Vec<Value>, role: &str, blocks: Vec<Value>) {
    if let Some(last) = messages.last_mut().filter(|v| v["role"] == role) {
        last["content"]
            .as_array_mut()
            .expect("已构造的消息内容为数组")
            .extend(blocks);
    } else {
        messages.push(json!({"role":role,"content":blocks}));
    }
}

fn content(value: &Value, path: &str, system: bool) -> Result<Vec<Value>, ChatError> {
    if let Some(text) = value.as_str() {
        return Ok(vec![json!({"type":"text","text":text})]);
    }
    let mut output: Vec<Value> = Vec::new();
    for (index, part) in value
        .as_array()
        .ok_or_else(|| invalid(path))?
        .iter()
        .enumerate()
    {
        let path = format!("{path}[{index}]");
        let kind = string(part.get("type"), &format!("{path}.type"), false)?;
        let block = match kind {
            "text" | "input_text" | "output_text" => {
                json!({"type":"text","text":string(part.get("text"), &format!("{path}.text"), true)?})
            }
            "refusal" if !system => {
                json!({"type":"text","text":string(part.get("refusal"), &format!("{path}.refusal"), true)?})
            }
            "image_url" | "input_image" if !system => {
                let url = part
                    .get("image_url")
                    .and_then(|v| v.as_str().or_else(|| v.get("url").and_then(Value::as_str)))
                    .ok_or_else(|| invalid(&format!("{path}.image_url")))?;
                json!({"type":"image","source":source(url, None, &path)?})
            }
            "file" | "input_file" if !system => {
                let file = if kind == "file" { &part["file"] } else { part };
                let source = if let Some(id) = file.get("file_id").and_then(Value::as_str) {
                    json!({"type":"file","file_id":id})
                } else if let Some(data) = file
                    .get("file_data")
                    .or_else(|| file.get("file_url"))
                    .and_then(Value::as_str)
                {
                    source(data, Some("application/pdf"), &path)?
                } else {
                    return Err(invalid(&path));
                };
                let mut block = json!({"type":"document","source":source});
                if let Some(name) = file.get("filename") {
                    block["title"] = name.clone();
                }
                block
            }
            "prompt_cache_breakpoint" => {
                let last = output.last_mut().ok_or_else(|| invalid(&path))?;
                last["cache_control"] = part
                    .get("cache_control")
                    .cloned()
                    .unwrap_or_else(|| json!({"type":"ephemeral"}));
                continue;
            }
            _ => return Err(unsupported(&format!("{path}.type"))),
        };
        output.push(block);
    }
    Ok(output)
}

fn source(url: &str, mime: Option<&str>, path: &str) -> Result<Value, ChatError> {
    if let Some(data) = url.strip_prefix("data:") {
        let (media_type, data) = data
            .split_once(";base64,")
            .filter(|(_, data)| !data.is_empty())
            .ok_or_else(|| invalid(path))?;
        if mime.is_none() && !media_type.starts_with("image/") {
            return Err(invalid(path));
        }
        return Ok(json!({"type":"base64","media_type":media_type,"data":data}));
    }
    if let Ok(url) = url::Url::parse(url)
        && matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
    {
        return Ok(json!({"type":"url","url":url.as_str()}));
    }
    if let Some(mime) = mime
        && !url.is_empty()
    {
        return Ok(json!({"type":"base64","media_type":mime,"data":url}));
    }
    Err(invalid(path))
}

fn tools(
    chat: &Map<String, Value>,
    result: &mut Map<String, Value>,
    capabilities: &MessagesModelCapabilities,
) -> Result<(), ChatError> {
    let mut tools = Vec::new();
    if let Some(definitions) = chat.get("tools").filter(|v| !v.is_null()) {
        for (index, tool) in definitions
            .as_array()
            .ok_or_else(|| invalid("tools"))?
            .iter()
            .enumerate()
        {
            let path = format!("tools[{index}]");
            let kind = string(tool.get("type"), &format!("{path}.type"), false)?;
            if !matches!(kind, "function" | "custom") {
                return Err(unsupported(&format!("{path}.type")));
            }
            let declaration = &tool[kind];
            let name = string(
                declaration.get("name"),
                &format!("{path}.{kind}.name"),
                false,
            )?;
            let schema = if kind == "custom" {
                json!({"type":"object","properties":{"input":{"type":"string"}},"required":["input"]})
            } else {
                declaration
                    .get("parameters")
                    .cloned()
                    .unwrap_or_else(|| json!({"type":"object","properties":{}}))
            };
            if !schema.is_object() {
                return Err(invalid(&format!("{path}.function.parameters")));
            }
            let mut target = json!({"name":name,"input_schema":schema});
            for field in ["description", "strict", "cache_control"] {
                if let Some(value) = declaration.get(field) {
                    target[field] = value.clone();
                }
            }
            tools.push(target);
        }
    }
    let mut choice = chat.get("tool_choice").cloned().unwrap_or(Value::Null);
    if let Some(options) = chat.get("web_search_options").filter(|v| !v.is_null()) {
        if !options.is_object() || !tools.is_empty() || (!choice.is_null() && choice != "auto") {
            return Err(invalid("web_search_options"));
        }
        if capabilities.hosted_web_search != Some(true) {
            return Err(unsupported("web_search_options"));
        }
        let mut tool = json!({"type":"web_search_20250305","name":"web_search"});
        if let Some(location) = options.get("user_location") {
            if location["type"] != "approximate" || !location["approximate"].is_object() {
                return Err(invalid("web_search_options.user_location"));
            }
            tool["user_location"] = location["approximate"].clone();
            tool["user_location"]["type"] = json!("approximate");
        }
        tools.push(tool);
        choice = json!({"type":"function","function":{"name":"web_search"}});
    }
    if choice["type"] == "allowed_tools" {
        let allowed = choice.get("allowed_tools").unwrap_or(&choice);
        let permitted = allowed["tools"]
            .as_array()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| invalid("tool_choice.allowed_tools.tools"))?;
        tools.retain(|tool| {
            permitted.iter().any(|v| {
                v.get(v["type"].as_str().unwrap_or("function"))
                    .and_then(|v| v.get("name"))
                    .or_else(|| v.get("name"))
                    == tool.get("name")
            })
        });
        choice = allowed
            .get("mode")
            .cloned()
            .ok_or_else(|| invalid("tool_choice.allowed_tools.mode"))?;
    }
    if tools.is_empty() {
        if !choice.is_null() && choice != "auto" && choice != "none" {
            return Err(invalid("tool_choice"));
        }
        return Ok(());
    }
    let serial = chat.get("parallel_tool_calls") == Some(&Value::Bool(false));
    let kind = choice
        .as_str()
        .or_else(|| choice.get("type").and_then(Value::as_str))
        .unwrap_or("auto");
    let mut mapped = match kind {
        "auto" | "none" => json!({"type":kind}),
        "required" => json!({"type":"any"}),
        "function" | "custom" => {
            let name = string(
                choice.get(kind).and_then(|v| v.get("name")),
                "tool_choice.name",
                false,
            )?;
            if !tools.iter().any(|v| v["name"] == name) {
                return Err(invalid("tool_choice"));
            }
            json!({"type":"tool","name":name})
        }
        _ => return Err(unsupported("tool_choice")),
    };
    if serial && kind != "none" {
        mapped["disable_parallel_tool_use"] = json!(true);
    }
    if !choice.is_null() || serial {
        result.insert("tool_choice".into(), mapped);
    }
    result.insert("tools".into(), tools.into());
    Ok(())
}

fn reasoning(
    chat: &Map<String, Value>,
    result: &mut Map<String, Value>,
    capabilities: &MessagesModelCapabilities,
    max_tokens: u64,
) -> Result<(), ChatError> {
    let Some(effort) = chat
        .get("reasoning_effort")
        .or_else(|| chat.get("reasoning").and_then(|v| v.get("effort")))
        .filter(|v| !v.is_null())
    else {
        return Ok(());
    };
    let effort = effort.as_str().ok_or_else(|| invalid("reasoning_effort"))?;
    if effort == "none" {
        if capabilities.disabled_thinking == Some(false) {
            return Err(unsupported("reasoning_effort"));
        }
        result.insert("thinking".into(), json!({"type":"disabled"}));
        return Ok(());
    }
    let budget = match effort {
        "minimal" | "low" => 1024,
        "medium" | "auto" => 8192,
        "high" => 24576,
        "xhigh" => 32768,
        "max" => 128000,
        _ => return Err(invalid("reasoning_effort")),
    };
    if capabilities.thinking == Some(false) {
        return Err(unsupported("reasoning_effort"));
    }
    if result
        .get("tool_choice")
        .and_then(|v| v.get("type"))
        .and_then(Value::as_str)
        .is_some_and(|v| matches!(v, "any" | "tool"))
    {
        return Err(ChatError::UnsupportedCombination {
            field: "reasoning_effort",
            other: "tool_choice",
        });
    }
    if capabilities.adaptive_thinking == Some(true) {
        result.insert("thinking".into(), json!({"type":"adaptive"}));
        let effort = if effort == "minimal" {
            "low"
        } else if effort == "auto" {
            "high"
        } else {
            effort
        };
        if capabilities
            .efforts
            .as_ref()
            .is_some_and(|supported| !supported.iter().any(|v| v == effort))
        {
            return Err(unsupported("reasoning_effort"));
        }
        result.entry("output_config").or_insert_with(|| json!({}))["effort"] = json!(effort);
    } else {
        if capabilities.manual_thinking == Some(false) || max_tokens <= 1024 {
            return Err(unsupported("reasoning_effort"));
        }
        result.insert(
            "thinking".into(),
            json!({"type":"enabled","budget_tokens":budget.min(max_tokens-1)}),
        );
    }
    result.remove("temperature");
    result.remove("top_p");
    Ok(())
}
