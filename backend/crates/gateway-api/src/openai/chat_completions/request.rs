//! Chat Completions 请求转换；只生成已实现的 Responses 字段，不透传未知 Chat 语义。

use axum::http::HeaderMap;
use serde_json::{Map, Value, json};

use super::super::responses::{
    DecodedResponsesRequest, OpenAiRequestHeaders, RequestDecodeError, RequestDecodeSource,
    decode_request_object, decompress_request_body,
};

pub(super) struct DecodedChatRequest {
    pub(super) responses: DecodedResponsesRequest,
    pub(super) include_usage: bool,
    pub(super) legacy_functions: bool,
}

pub(super) fn decode_request_with_headers(
    body: &[u8],
    headers: &HeaderMap,
) -> Result<DecodedChatRequest, RequestDecodeError> {
    let body = decompress_request_body(body, headers)?;
    let value: Value =
        serde_json::from_slice(&body).map_err(|_| RequestDecodeError::MalformedJson)?;
    let Value::Object(mut chat) = value else {
        return Err(RequestDecodeError::ExpectedObject);
    };
    if chat.get("input").is_some_and(|value| !value.is_null()) {
        if chat.get("messages").is_some_and(|value| !value.is_null()) {
            return Err(invalid("messages"));
        }
        chat.remove("messages");
        let stream = take_bool(&mut chat, "stream")?.unwrap_or(false);
        chat.insert("stream".into(), stream.into());
        let include_usage = stream_options(chat.remove("stream_options"), stream)?;
        let mut chat_options = Map::new();
        for field in [
            "n",
            "logprobs",
            "top_logprobs",
            "stop",
            "frequency_penalty",
            "presence_penalty",
            "logit_bias",
            "modalities",
            "audio",
            "prediction",
            "seed",
        ] {
            if let Some(value) = chat.remove(field) {
                chat_options.insert(field.into(), value);
            }
        }
        validate_remaining(chat_options)?;
        let responses = decode_request_object(
            chat,
            &OpenAiRequestHeaders::from_headers(headers),
            RequestDecodeSource::Http,
        )?;
        return Ok(DecodedChatRequest {
            responses,
            include_usage,
            legacy_functions: false,
        });
    }
    let mut responses = Map::new();
    responses.insert("model".into(), take_required(&mut chat, "model")?);
    let stream = take_bool(&mut chat, "stream")?.unwrap_or(false);
    responses.insert("stream".into(), stream.into());
    responses.insert("store".into(), false.into());
    let include_usage = stream_options(chat.remove("stream_options"), stream)?;
    responses.insert(
        "input".into(),
        messages(take_required(&mut chat, "messages")?)?,
    );
    let modern_tools = take_optional(&mut chat, "tools");
    let functions = take_optional(&mut chat, "functions");
    let legacy_choice = take_optional(&mut chat, "function_call");
    let legacy_functions = modern_tools
        .as_ref()
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty)
        && (functions.is_some() || legacy_choice.is_some());
    let mut tools = match modern_tools {
        Some(tools) => function_tools(tools, false)?,
        None => Vec::new(),
    };
    if let Some(functions) = functions {
        tools.extend(function_tools(functions, true)?);
    }
    let mut merged = Vec::new();
    for tool in tools {
        if let Some(previous) = merged
            .iter()
            .find(|previous: &&Value| previous["name"] == tool["name"])
        {
            if previous != &tool {
                return Err(invalid("functions"));
            }
        } else {
            merged.push(tool);
        }
    }
    if !merged.is_empty() {
        responses.insert("tools".into(), merged.into());
    }
    let modern_choice = take_optional(&mut chat, "tool_choice")
        .map(|choice| tool_choice(choice, responses.get("tools")))
        .transpose()?;
    let legacy_choice = legacy_choice
        .map(|choice| {
            if matches!(choice.as_str(), Some("auto" | "none")) {
                return Ok(choice);
            }
            let mut choice = object(choice, "function_call")?;
            let name = take_string(&mut choice, "name", "function_call", false)?;
            Ok(json!({"type":"function","function":{"name":name}}))
        })
        .transpose()?
        .map(|choice| tool_choice(choice, responses.get("tools")))
        .transpose()?;
    if modern_choice.is_some() && legacy_choice.is_some() && modern_choice != legacy_choice {
        return Err(invalid("function_call"));
    }
    if let Some(choice) = modern_choice.or(legacy_choice) {
        responses.insert("tool_choice".into(), choice);
    }
    let parallel = take_bool(&mut chat, "parallel_tool_calls")?;
    if legacy_functions && parallel == Some(true) {
        return Err(unsupported("parallel_tool_calls"));
    }
    if let Some(parallel) = if legacy_functions {
        Some(false)
    } else {
        parallel
    } {
        responses.insert("parallel_tool_calls".into(), parallel.into());
    }
    if let Some(format) = take_optional(&mut chat, "response_format") {
        responses.insert("text".into(), json!({"format": response_format(format)?}));
    }
    if let Some(verbosity) = take_optional(&mut chat, "verbosity") {
        if !matches!(verbosity.as_str(), Some("low" | "medium" | "high")) {
            return Err(invalid("verbosity"));
        }
        responses.entry("text").or_insert_with(|| json!({}))["verbosity"] = verbosity;
    }
    if let Some(top_p) = take_optional(&mut chat, "top_p") {
        if !top_p
            .as_f64()
            .is_some_and(|value| (0.0..=1.0).contains(&value))
        {
            return Err(invalid("top_p"));
        }
        responses.insert("top_p".into(), top_p);
    }
    if let Some(value) = take_optional(&mut chat, "temperature") {
        if !value
            .as_f64()
            .is_some_and(|value| (0.0..=2.0).contains(&value))
        {
            return Err(invalid("temperature"));
        }
        responses.insert("temperature".into(), value);
    }
    for field in ["max_tokens", "max_completion_tokens"] {
        if let Some(value) = take_optional(&mut chat, field) {
            if !value.as_u64().is_some_and(|value| value > 0) {
                return Err(invalid(field));
            }
            responses.insert("max_output_tokens".into(), value);
        }
    }
    let mut reasoning = Map::new();
    if let Some(value) = take_optional(&mut chat, "reasoning") {
        let mut value = object(value, "reasoning")?;
        for field in ["effort", "summary"] {
            if let Some(value) = take_optional(&mut value, field) {
                let valid = match field {
                    "effort" => valid_reasoning_effort(&value),
                    _ => matches!(value.as_str(), Some("auto" | "concise" | "detailed")),
                };
                if !valid {
                    return Err(invalid(&format!("reasoning.{field}")));
                }
                reasoning.insert(field.into(), value);
            }
        }
    }
    if let Some(effort) = take_optional(&mut chat, "reasoning_effort") {
        if !valid_reasoning_effort(&effort) {
            return Err(invalid("reasoning_effort"));
        }
        reasoning.insert("effort".into(), effort);
    }
    if !reasoning.is_empty() {
        responses.insert("reasoning".into(), reasoning.into());
    }
    for field in [
        "instructions",
        "service_tier",
        "prompt_cache_key",
        "prompt_cache_retention",
    ] {
        if let Some(value) = take_optional(&mut chat, field) {
            if !value.is_string() {
                return Err(invalid(field));
            }
            responses.insert(field.into(), value);
        }
    }
    for field in ["user", "metadata", "safety_identifier"] {
        validate_metadata(&mut chat, field)?;
    }
    validate_remaining(chat)?;
    let responses = decode_request_object(
        responses,
        &OpenAiRequestHeaders::from_headers(headers),
        RequestDecodeSource::Http,
    )?;
    Ok(DecodedChatRequest {
        responses,
        include_usage,
        legacy_functions,
    })
}

fn valid_reasoning_effort(value: &Value) -> bool {
    matches!(
        value.as_str(),
        Some("none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max")
    )
}

fn stream_options(value: Option<Value>, stream: bool) -> Result<bool, RequestDecodeError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(false);
    };
    let mut options = object(value, "stream_options")?;
    let include_usage = take_bool_at(
        &mut options,
        "include_usage",
        "stream_options.include_usage",
    )?
    .unwrap_or(false);
    take_bool_at(
        &mut options,
        "include_obfuscation",
        "stream_options.include_obfuscation",
    )?;
    if !stream {
        return Err(invalid("stream_options"));
    }
    Ok(include_usage)
}

fn validate_remaining(chat: Map<String, Value>) -> Result<(), RequestDecodeError> {
    for (field, value) in chat {
        let neutral = match field.as_str() {
            "n" => value.is_null() || value.as_u64() == Some(1),
            "store" | "logprobs" => value.is_null() || value == false,
            "frequency_penalty" | "presence_penalty" => {
                value.is_null() || value.as_f64() == Some(0.0)
            }
            "logit_bias" | "metadata" => {
                value.is_null() || value.as_object().is_some_and(Map::is_empty)
            }
            "modalities" => value.is_null() || value == json!(["text"]),
            "service_tier" => value.is_null() || value == "auto",
            "stop" => value.is_null() || value.as_array().is_some_and(Vec::is_empty),
            "top_logprobs" => value.is_null() || value.as_u64() == Some(0),
            "temperature"
            | "max_tokens"
            | "max_completion_tokens"
            | "reasoning_effort"
            | "seed"
            | "audio"
            | "prediction"
            | "functions"
            | "function_call"
            | "web_search_options"
            | "safety_identifier"
            | "prompt_cache_key"
            | "prompt_cache_retention"
            | "prompt_cache_options"
            | "user"
            | "verbosity"
            | "moderation" => value.is_null(),
            _ => true,
        };
        if !neutral {
            return Err(unsupported(&field));
        }
    }
    Ok(())
}

fn validate_metadata(chat: &mut Map<String, Value>, field: &str) -> Result<(), RequestDecodeError> {
    if let Some(value) = take_optional(chat, field) {
        let valid = if field == "metadata" {
            value
                .as_object()
                .is_some_and(|values| values.values().all(Value::is_string))
        } else {
            value.is_string()
        };
        if !valid {
            return Err(invalid(field));
        }
    }
    Ok(())
}

fn messages(value: Value) -> Result<Value, RequestDecodeError> {
    let Value::Array(messages) = value else {
        return Err(invalid("messages"));
    };
    if messages.is_empty() {
        return Err(RequestDecodeError::EmptyField {
            field: "messages".into(),
        });
    }
    let mut input = Vec::new();
    // 旧函数没有调用 ID；以历史位置生成身份，并只配对尚未返回的同名调用。
    let mut legacy_pending: Vec<(String, String)> = Vec::new();
    let mut used_ids = std::collections::HashSet::new();
    for message in &messages {
        if let Some(id) = message.get("tool_call_id").and_then(Value::as_str) {
            used_ids.insert(id.to_owned());
        }
        if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                if let Some(id) = call.get("id").and_then(Value::as_str) {
                    used_ids.insert(id.to_owned());
                }
            }
        }
    }
    for (index, message) in messages.into_iter().enumerate() {
        let path = format!("messages[{index}]");
        let mut message = object(message, &path)?;
        let role = take_string(&mut message, "role", &path, false)?;
        let name = take_optional(&mut message, "name");
        if name.as_ref().is_some_and(|value| !value.is_string()) {
            return Err(invalid(&format!("{path}.name")));
        }
        if take_optional(&mut message, "audio").is_some() {
            return Err(unsupported(&format!("{path}.audio")));
        }
        for (field, allowed_role) in [
            ("tool_calls", "assistant"),
            ("tool_call_id", "tool"),
            ("function_call", "assistant"),
            ("refusal", "assistant"),
        ] {
            if role != allowed_role && message.get(field).is_some_and(|value| !value.is_null()) {
                return Err(invalid(&format!("{path}.{field}")));
            }
        }
        if role == "tool" || role == "function" {
            let call_id = if role == "tool" {
                take_string(&mut message, "tool_call_id", &path, false)?
            } else {
                let name = name
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| invalid(&format!("{path}.name")))?;
                let pending = legacy_pending
                    .iter()
                    .position(|(pending, _)| pending == &name)
                    .ok_or_else(|| invalid(&format!("{path}.name")))?;
                legacy_pending.remove(pending).1
            };
            let output = content(
                take_required_at(&mut message, "content", &format!("{path}.content"))?,
                "tool",
                &format!("{path}.content"),
            )?;
            input.push(json!({"type":"function_call_output","call_id":call_id,"output":output}));
        } else if matches!(role.as_str(), "system" | "developer" | "user" | "assistant") {
            let text = message.remove("content");
            let tool_calls = take_optional(&mut message, "tool_calls");
            let legacy_call = take_optional(&mut message, "function_call");
            let refusal = take_optional(&mut message, "refusal");
            let reasoning_content = take_optional(&mut message, "reasoning_content");
            let reasoning = take_optional(&mut message, "reasoning");
            for (field, value) in [
                ("reasoning_content", &reasoning_content),
                ("reasoning", &reasoning),
            ] {
                if value.as_ref().is_some_and(|value| !value.is_string()) {
                    return Err(invalid(&format!("{path}.{field}")));
                }
            }
            let reasoning = reasoning_content.or(reasoning);
            if role == "assistant"
                && let Some(reasoning) = &reasoning
            {
                // 公开思考文本作为客户端历史正文保留，不伪造上游 reasoning 身份或加密状态。
                input.push(
                    json!({"role":"assistant","content":[{"type":"input_text","text":reasoning}]}),
                );
            }
            if refusal.as_ref().is_some_and(|value| !value.is_string()) {
                return Err(invalid(&format!("{path}.refusal")));
            }
            if tool_calls.is_some() && legacy_call.is_some() {
                return Err(invalid(&format!("{path}.function_call")));
            }
            if text.is_none()
                && tool_calls.is_none()
                && legacy_call.is_none()
                && refusal.is_none()
                && !(role == "assistant" && reasoning.is_some())
            {
                return Err(invalid(&format!("{path}.content")));
            }
            let text = text.filter(|value| {
                !value.is_null() || (tool_calls.is_none() && legacy_call.is_none())
            });
            let has_refusal_parts = role == "assistant"
                && text
                    .as_ref()
                    .and_then(Value::as_array)
                    .is_some_and(|parts| {
                        parts
                            .iter()
                            .any(|part| part.get("type").and_then(Value::as_str) == Some("refusal"))
                    });
            if refusal.is_some() || has_refusal_parts {
                let mut parts = assistant_output_content(text, &format!("{path}.content"))?;
                if let Some(refusal) = refusal {
                    parts.push(json!({"type":"refusal","refusal":refusal}));
                }
                input.push(json!({"type":"message","id":format!("msg_chat_{index}"),"role":"assistant","status":"completed","content":parts}));
            } else if let Some(text) = text {
                input.push(json!({"role":role,"content":content(text, &role, &format!("{path}.content"))?}));
            }
            if let Some(call) = legacy_call {
                let function_path = format!("{path}.function_call");
                let mut function = object(call, &function_path)?;
                let name = take_string(&mut function, "name", &function_path, false)?;
                let arguments = take_string(&mut function, "arguments", &function_path, true)?;
                let mut id = format!("call_chat_legacy_{index}");
                while !used_ids.insert(id.clone()) {
                    id.push('_');
                }
                legacy_pending.push((name.clone(), id.clone()));
                input.push(
                    json!({"type":"function_call","call_id":id,"name":name,"arguments":arguments}),
                );
            }
            if let Some(tool_calls) = tool_calls {
                let Value::Array(tool_calls) = tool_calls else {
                    return Err(invalid(&format!("{path}.tool_calls")));
                };
                for (index, call) in tool_calls.into_iter().enumerate() {
                    let call_path = format!("{path}.tool_calls[{index}]");
                    let mut call = object(call, &call_path)?;
                    let id = take_string(&mut call, "id", &call_path, false)?;
                    if take_string(&mut call, "type", &call_path, false)? != "function" {
                        return Err(unsupported(&format!("{call_path}.type")));
                    }
                    let function_path = format!("{call_path}.function");
                    let mut function = object(
                        take_required_at(&mut call, "function", &function_path)?,
                        &function_path,
                    )?;
                    let name = take_string(&mut function, "name", &function_path, false)?;
                    let arguments = take_string(&mut function, "arguments", &function_path, true)?;
                    input.push(json!({"type":"function_call","call_id":id,"name":name,"arguments":arguments}));
                }
            }
        } else {
            return Err(unsupported(&format!("{path}.role")));
        }
        // 消息对象可携带客户端扩展；只投影上面按角色提取的协议字段，不递归过滤正文或工具参数。
    }
    Ok(input.into())
}

fn assistant_output_content(
    text: Option<Value>,
    path: &str,
) -> Result<Vec<Value>, RequestDecodeError> {
    match text {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(text)) => Ok(vec![json!({"type":"output_text","text":text,"annotations":[]})]),
        Some(Value::Array(parts)) => parts.into_iter().enumerate().map(|(index, part)| {
            let path = format!("{path}[{index}]");
            let mut part = object(part, &path)?;
            match take_string(&mut part, "type", &path, false)?.as_str() {
                "text" => Ok(json!({"type":"output_text","text":take_string(&mut part,"text",&path,true)?,"annotations":[]})),
                "refusal" => Ok(json!({"type":"refusal","refusal":take_string(&mut part,"refusal",&path,true)?})),
                _ => Err(unsupported(&format!("{path}.type"))),
            }
        }).collect(),
        _ => Err(invalid(path)),
    }
}

fn content(value: Value, role: &str, path: &str) -> Result<Value, RequestDecodeError> {
    if value.is_null() {
        return Ok(Value::String(String::new()));
    }
    if value.is_string() {
        return Ok(value);
    }
    let Value::Array(parts) = value else {
        return Err(invalid(path));
    };
    let mut result = Vec::with_capacity(parts.len());
    for (index, part) in parts.into_iter().enumerate() {
        let path = format!("{path}[{index}]");
        let mut part = object(part, &path)?;
        let kind = take_string(&mut part, "type", &path, false)?;
        let converted = match kind.as_str() {
            "text" => {
                let text = take_string(&mut part, "text", &path, true)?;
                // EasyInputMessage 的 assistant 历史同样使用 input_text；output_text 需要完整输出消息身份。
                json!({"type":"input_text","text":text})
            }
            "image_url" if role == "user" => {
                let image_path = format!("{path}.image_url");
                let mut image = object(
                    take_required_at(&mut part, "image_url", &image_path)?,
                    &image_path,
                )?;
                let url = take_string(&mut image, "url", &image_path, false)?;
                if !url::Url::parse(&url).is_ok_and(|parsed| {
                    matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some()
                        || parsed.scheme() == "data"
                            && url.starts_with("data:image/")
                            && url.contains(";base64,")
                            && url
                                .split_once(";base64,")
                                .is_some_and(|(_, data)| !data.trim().is_empty())
                }) {
                    return Err(invalid(&format!("{image_path}.url")));
                }
                let mut converted = json!({"type":"input_image","image_url":url});
                if let Some(detail) = take_optional(&mut image, "detail") {
                    if !matches!(detail.as_str(), Some("auto" | "low" | "high")) {
                        return Err(invalid(&format!("{image_path}.detail")));
                    }
                    converted["detail"] = detail;
                }
                converted
            }
            "file" if role == "user" => {
                let file_path = format!("{path}.file");
                let mut file =
                    object(take_required_at(&mut part, "file", &file_path)?, &file_path)?;
                let mut converted = json!({"type":"input_file"});
                for field in ["file_data", "file_id", "filename"] {
                    if let Some(value) = take_optional(&mut file, field) {
                        if !value.as_str().is_some_and(|value| !value.trim().is_empty()) {
                            return Err(invalid(&format!("{file_path}.{field}")));
                        }
                        converted[field] = value;
                    }
                }
                if converted.get("file_data").is_none() && converted.get("file_id").is_none() {
                    return Err(invalid(&file_path));
                }
                converted
            }
            _ => return Err(unsupported(&format!("{path}.type"))),
        };
        result.push(converted);
    }
    Ok(result.into())
}

fn function_tools(value: Value, legacy: bool) -> Result<Vec<Value>, RequestDecodeError> {
    let root = if legacy { "functions" } else { "tools" };
    let Value::Array(tools) = value else {
        return Err(invalid(root));
    };
    let mut result: Vec<Value> = Vec::with_capacity(tools.len());
    for (index, tool) in tools.into_iter().enumerate() {
        let path = format!("{root}[{index}]");
        let mut tool = object(tool, &path)?;
        let (function_path, mut function) = if legacy {
            (path, tool)
        } else {
            if take_string(&mut tool, "type", &path, false)? != "function" {
                return Err(unsupported(&format!("{path}.type")));
            }
            let function_path = format!("{path}.function");
            let function = object(
                take_required_at(&mut tool, "function", &function_path)?,
                &function_path,
            )?;
            (function_path, function)
        };
        let name = take_string(&mut function, "name", &function_path, false)?;
        // Chat 函数工具默认非 strict，不能让 Responses 的默认规范化收紧调用合同。
        let mut converted = Map::from_iter([
            ("type".into(), "function".into()),
            ("name".into(), name.into()),
            ("strict".into(), false.into()),
        ]);
        for field in ["description", "parameters", "strict"] {
            if let Some(value) = take_optional(&mut function, field) {
                let valid = match field {
                    "description" => value.is_string(),
                    "parameters" => value.is_object(),
                    _ => value.is_boolean(),
                };
                if !valid {
                    return Err(invalid(&format!("{function_path}.{field}")));
                }
                converted.insert(field.into(), value);
            }
        }
        result.push(converted.into());
    }
    Ok(result)
}

fn tool_choice(value: Value, tools: Option<&Value>) -> Result<Value, RequestDecodeError> {
    if value == "required"
        && !tools
            .and_then(Value::as_array)
            .is_some_and(|tools| !tools.is_empty())
    {
        return Err(invalid("tool_choice"));
    }
    if matches!(value.as_str(), Some("none" | "auto" | "required")) {
        return Ok(value);
    }
    let mut choice = object(value, "tool_choice")?;
    if take_string(&mut choice, "type", "tool_choice", false)? != "function" {
        return Err(unsupported("tool_choice.type"));
    }
    let mut function = object(
        take_required_at(&mut choice, "function", "tool_choice.function")?,
        "tool_choice.function",
    )?;
    let name = take_string(&mut function, "name", "tool_choice.function", false)?;
    if !tools
        .and_then(Value::as_array)
        .is_some_and(|tools| tools.iter().any(|tool| tool["name"] == name))
    {
        return Err(invalid("tool_choice.function.name"));
    }
    Ok(json!({"type":"function","name":name}))
}

fn response_format(value: Value) -> Result<Value, RequestDecodeError> {
    let mut format = object(value, "response_format")?;
    let kind = take_string(&mut format, "type", "response_format", false)?;
    let converted = match kind.as_str() {
        "text" | "json_object" => json!({"type":kind}),
        "json_schema" => {
            let path = "response_format.json_schema";
            let mut schema = object(take_required_at(&mut format, "json_schema", path)?, path)?;
            let name = take_string(&mut schema, "name", path, false)?;
            let definition =
                take_required_at(&mut schema, "schema", "response_format.json_schema.schema")?;
            if !definition.is_object() {
                return Err(invalid("response_format.json_schema.schema"));
            }
            let mut converted = json!({"type":"json_schema","name":name,"schema":definition});
            if let Some(description) = take_optional(&mut schema, "description") {
                if !description.is_string() {
                    return Err(invalid("response_format.json_schema.description"));
                }
                converted["description"] = description;
            }
            if let Some(strict) =
                take_bool_at(&mut schema, "strict", "response_format.json_schema.strict")?
            {
                converted["strict"] = strict.into();
            }
            converted
        }
        _ => return Err(unsupported("response_format.type")),
    };
    Ok(converted)
}

fn object(value: Value, path: &str) -> Result<Map<String, Value>, RequestDecodeError> {
    match value {
        Value::Object(object) => Ok(object),
        _ => Err(invalid(path)),
    }
}

fn take_required(
    object: &mut Map<String, Value>,
    field: &str,
) -> Result<Value, RequestDecodeError> {
    take_required_at(object, field, field)
}

fn take_required_at(
    object: &mut Map<String, Value>,
    field: &str,
    path: &str,
) -> Result<Value, RequestDecodeError> {
    object
        .remove(field)
        .ok_or_else(|| RequestDecodeError::MissingField { field: path.into() })
}

fn take_optional(object: &mut Map<String, Value>, field: &str) -> Option<Value> {
    object.remove(field).filter(|value| !value.is_null())
}

fn take_bool(
    object: &mut Map<String, Value>,
    field: &str,
) -> Result<Option<bool>, RequestDecodeError> {
    take_bool_at(object, field, field)
}

fn take_bool_at(
    object: &mut Map<String, Value>,
    field: &str,
    path: &str,
) -> Result<Option<bool>, RequestDecodeError> {
    take_optional(object, field)
        .map(|value| value.as_bool().ok_or_else(|| invalid(path)))
        .transpose()
}

fn take_string(
    object: &mut Map<String, Value>,
    field: &str,
    path: &str,
    allow_empty: bool,
) -> Result<String, RequestDecodeError> {
    let value = object
        .remove(field)
        .ok_or_else(|| RequestDecodeError::MissingField {
            field: format!("{path}.{field}"),
        })?;
    match value {
        Value::String(value) if allow_empty || !value.trim().is_empty() => Ok(value),
        _ => Err(invalid(&format!("{path}.{field}"))),
    }
}

fn invalid(field: &str) -> RequestDecodeError {
    RequestDecodeError::InvalidValue {
        field: field.into(),
    }
}
fn unsupported(field: &str) -> RequestDecodeError {
    RequestDecodeError::UnsupportedField {
        field: field.into(),
    }
}
