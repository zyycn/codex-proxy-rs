//! 将 Chat 请求转换为 Responses 正文，协议选项留在 API，业务 JSON 与上游兼容规则保持原有归属

use serde_json::{Map, Value, json};

use crate::openai::responses::RequestDecodeError;

pub(in crate::openai) struct ConvertedChatRequest {
    pub(in crate::openai) body: Map<String, Value>,
    pub(in crate::openai) include_usage: bool,
}

pub(in crate::openai) fn convert_request(
    body: &[u8],
) -> Result<ConvertedChatRequest, RequestDecodeError> {
    let value = serde_json::from_slice(body).map_err(|_| RequestDecodeError::MalformedJson)?;
    let Value::Object(mut chat) = value else {
        return Err(RequestDecodeError::ExpectedObject);
    };
    validate_options(&chat)?;
    let model = required_string(&mut chat, "model", "model", false)?;
    let stream = optional_bool(&mut chat, "stream", "stream")?.unwrap_or(false);
    let include_usage = stream_options(optional(&mut chat, "stream_options"), stream)?;
    let input = messages(required(&mut chat, "messages", "messages")?)?;
    let mut body = Map::from_iter([
        ("model".into(), model.into()),
        ("input".into(), input.into()),
        ("stream".into(), stream.into()),
        ("store".into(), false.into()),
    ]);
    if let Some(value) = optional(&mut chat, "tools") {
        body.insert("tools".into(), function_tools(value)?.into());
    }
    if let Some(value) = optional(&mut chat, "tool_choice") {
        let choice = tool_choice(value, body.get("tools"))?;
        body.insert("tool_choice".into(), choice);
    }
    if let Some(value) = optional_bool(&mut chat, "parallel_tool_calls", "parallel_tool_calls")? {
        body.insert("parallel_tool_calls".into(), value.into());
    }

    let mut text = Map::new();
    if let Some(value) = optional(&mut chat, "response_format") {
        text.insert("format".into(), response_format(value)?);
    }
    if let Some(value) = optional(&mut chat, "verbosity") {
        string(&value, "verbosity", false)?;
        text.insert("verbosity".into(), value);
    }
    if !text.is_empty() {
        body.insert("text".into(), text.into());
    }
    if let Some(value) = optional(&mut chat, "reasoning_effort") {
        string(&value, "reasoning_effort", false)?;
        body.insert("reasoning".into(), json!({"effort": value}));
    }
    for field in ["max_tokens", "max_completion_tokens"] {
        if let Some(value) = optional(&mut chat, field) {
            if !value.as_u64().is_some_and(|tokens| tokens > 0) {
                return Err(invalid(field));
            }
            // 新字段优先，具体上游是否接受长度控制仍由现有 Provider 决定
            body.insert("max_output_tokens".into(), value);
        }
    }
    for (field, maximum) in [("temperature", 2.0), ("top_p", 1.0)] {
        if let Some(value) = optional(&mut chat, field) {
            if !value
                .as_f64()
                .is_some_and(|number| (0.0..=maximum).contains(&number))
            {
                return Err(invalid(field));
            }
            body.insert(field.into(), value);
        }
    }
    for field in [
        "user",
        "safety_identifier",
        "service_tier",
        "prompt_cache_key",
        "prompt_cache_retention",
    ] {
        if let Some(value) = optional(&mut chat, field) {
            string(&value, field, true)?;
            body.insert(field.into(), value);
        }
    }
    for field in ["metadata", "prompt_cache_options", "moderation"] {
        if let Some(value) = optional(&mut chat, field) {
            let object = object(value, field)?;
            body.insert(field.into(), object.into());
        }
    }
    // 未识别的客户端顶层扩展不等于 Responses 参数，不进入上游正文
    Ok(ConvertedChatRequest {
        body,
        include_usage,
    })
}

fn validate_options(chat: &Map<String, Value>) -> Result<(), RequestDecodeError> {
    for (field, value) in chat.iter().filter(|(_, value)| !value.is_null()) {
        let supported = match field.as_str() {
            "n" => value.as_u64() == Some(1),
            "store" | "logprobs" => value == false,
            "frequency_penalty" | "presence_penalty" => value.as_f64() == Some(0.0),
            "top_logprobs" => value.as_u64() == Some(0),
            "logit_bias" => value.as_object().is_some_and(Map::is_empty),
            "functions" | "stop" => value.as_array().is_some_and(Vec::is_empty),
            "modalities" => value == &json!(["text"]),
            "audio"
            | "prediction"
            | "seed"
            | "function_call"
            | "web_search_options"
            | "reasoning_content"
            | "input"
            | "previous_response_id"
            | "conversation"
            | "instructions"
            | "reasoning"
            | "text"
            | "include"
            | "background"
            | "max_output_tokens" => false,
            _ => true,
        };
        if !supported {
            return Err(unsupported(field));
        }
    }
    Ok(())
}

fn stream_options(value: Option<Value>, stream: bool) -> Result<bool, RequestDecodeError> {
    let Some(value) = value else {
        return Ok(false);
    };
    if !stream {
        return Err(invalid("stream_options"));
    }
    let mut options = object(value, "stream_options")?;
    let include_usage = optional_bool(
        &mut options,
        "include_usage",
        "stream_options.include_usage",
    )?
    .unwrap_or(false);
    if optional_bool(
        &mut options,
        "include_obfuscation",
        "stream_options.include_obfuscation",
    )? == Some(true)
    {
        // Chat 重编码会改变帧长度，不能声称继承上游流的长度混淆保证
        return Err(unsupported("stream_options.include_obfuscation"));
    }
    Ok(include_usage)
}

fn messages(value: Value) -> Result<Vec<Value>, RequestDecodeError> {
    let messages = array(value, "messages")?;
    if messages.is_empty() {
        return Err(RequestDecodeError::EmptyField {
            field: "messages".into(),
        });
    }
    let mut input = Vec::with_capacity(messages.len());
    for (index, value) in messages.into_iter().enumerate() {
        let path = format!("messages[{index}]");
        let mut message = object(value, &path)?;
        let role = required_string(&mut message, "role", &format!("{path}.role"), false)?;
        if !matches!(
            role.as_str(),
            "system" | "developer" | "user" | "assistant" | "tool"
        ) {
            return Err(unsupported(&format!("{path}.role")));
        }
        if let Some(name) = optional(&mut message, "name")
            && !string(&name, &format!("{path}.name"), true)?.is_empty()
        {
            return Err(unsupported(&format!("{path}.name")));
        }
        for field in ["function_call", "reasoning_content", "reasoning", "audio"] {
            if optional(&mut message, field).is_some() {
                return Err(unsupported(&format!("{path}.{field}")));
            }
        }
        for (field, allowed_role) in [
            ("tool_calls", "assistant"),
            ("refusal", "assistant"),
            ("tool_call_id", "tool"),
        ] {
            if role != allowed_role && message.get(field).is_some_and(|value| !value.is_null()) {
                return Err(invalid(&format!("{path}.{field}")));
            }
        }
        if role == "tool" {
            let call_id = required_string(
                &mut message,
                "tool_call_id",
                &format!("{path}.tool_call_id"),
                false,
            )?;
            let content_path = format!("{path}.content");
            let content = required(&mut message, "content", &content_path)?;
            let output = if content.is_string() {
                content
            } else {
                content_parts(content, &role, &content_path, false)?.into()
            };
            input.push(
                json!({"type": "function_call_output", "call_id": call_id, "output": output}),
            );
            continue;
        }

        let calls = optional(&mut message, "tool_calls")
            .map(|value| tool_calls(value, &format!("{path}.tool_calls")))
            .transpose()?
            .unwrap_or_default();
        let refusal = optional(&mut message, "refusal");
        if let Some(value) = &refusal {
            string(value, &format!("{path}.refusal"), true)?;
        }
        let content = optional(&mut message, "content");
        if content.is_none() && refusal.is_none() && calls.is_empty() {
            return Err(invalid(&format!("{path}.content")));
        }
        let is_assistant = role == "assistant";
        let mut parts = content
            .map(|value| content_parts(value, &role, &format!("{path}.content"), is_assistant))
            .transpose()?;
        if let Some(refusal) = refusal {
            parts
                .get_or_insert_with(Vec::new)
                .push(json!({"type": "refusal", "refusal": refusal}));
        }
        if let Some(parts) = parts {
            let mut item = json!({"type": "message", "role": role, "content": parts});
            if is_assistant {
                // assistant 历史采用输出消息合同，普通文本和 refusal 都不能编码成 input_text
                item["id"] = format!("msg_chat_{index}").into();
                item["status"] = "completed".into();
            }
            input.push(item);
        }
        input.extend(calls);
    }
    Ok(input)
}

fn content_parts(
    value: Value,
    role: &str,
    path: &str,
    is_assistant: bool,
) -> Result<Vec<Value>, RequestDecodeError> {
    let parts = match value {
        Value::String(text) => return Ok(vec![text_part(text, is_assistant)]),
        Value::Array(parts) => parts,
        _ => return Err(invalid_type(path, "a string or an array")),
    };
    let mut result = Vec::with_capacity(parts.len());
    for (index, value) in parts.into_iter().enumerate() {
        let path = format!("{path}[{index}]");
        let mut part = object(value, &path)?;
        let kind = required_string(&mut part, "type", &format!("{path}.type"), false)?;
        let mut converted = match kind.as_str() {
            "text" => text_part(
                required_string(&mut part, "text", &format!("{path}.text"), true)?,
                is_assistant,
            ),
            "image_url" if role == "user" => image_part(
                required(&mut part, "image_url", &format!("{path}.image_url"))?,
                &format!("{path}.image_url"),
            )?,
            "refusal" if role == "assistant" => json!({
                "type": "refusal",
                "refusal": required_string(&mut part, "refusal", &format!("{path}.refusal"), true)?,
            }),
            _ => return Err(unsupported(&format!("{path}.type"))),
        };
        if let Some(value) = optional(&mut part, "prompt_cache_breakpoint") {
            if is_assistant {
                return Err(unsupported(&format!("{path}.prompt_cache_breakpoint")));
            }
            converted["prompt_cache_breakpoint"] =
                object(value, &format!("{path}.prompt_cache_breakpoint"))?.into();
        }
        result.push(converted);
    }
    Ok(result)
}

fn text_part(text: String, is_assistant: bool) -> Value {
    if is_assistant {
        json!({"type": "output_text", "text": text, "annotations": []})
    } else {
        json!({"type": "input_text", "text": text})
    }
}

fn image_part(value: Value, path: &str) -> Result<Value, RequestDecodeError> {
    let mut image = object(value, path)?;
    let url = required_string(&mut image, "url", &format!("{path}.url"), false)?;
    let valid = url::Url::parse(&url).is_ok_and(|parsed| {
        matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some()
            || parsed.scheme() == "data"
                && url.starts_with("data:image/")
                && url
                    .split_once(";base64,")
                    .is_some_and(|(_, data)| !data.trim().is_empty())
    });
    if !valid {
        return Err(invalid(&format!("{path}.url")));
    }
    let mut converted = json!({"type": "input_image", "image_url": url});
    if let Some(detail) = optional(&mut image, "detail") {
        string(&detail, &format!("{path}.detail"), false)?;
        converted["detail"] = detail;
    }
    Ok(converted)
}

fn tool_calls(value: Value, path: &str) -> Result<Vec<Value>, RequestDecodeError> {
    let calls = array(value, path)?;
    let mut result = Vec::with_capacity(calls.len());
    for (index, value) in calls.into_iter().enumerate() {
        let path = format!("{path}[{index}]");
        let mut call = object(value, &path)?;
        let id = required_string(&mut call, "id", &format!("{path}.id"), false)?;
        if required_string(&mut call, "type", &format!("{path}.type"), false)? != "function" {
            return Err(unsupported(&format!("{path}.type")));
        }
        let function_path = format!("{path}.function");
        let mut function = object(
            required(&mut call, "function", &function_path)?,
            &function_path,
        )?;
        let name = required_string(
            &mut function,
            "name",
            &format!("{function_path}.name"),
            false,
        )?;
        let arguments = required_string(
            &mut function,
            "arguments",
            &format!("{function_path}.arguments"),
            true,
        )?;
        result.push(
            json!({"type": "function_call", "call_id": id, "name": name, "arguments": arguments}),
        );
    }
    Ok(result)
}

fn function_tools(value: Value) -> Result<Vec<Value>, RequestDecodeError> {
    array(value, "tools")?
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            let path = format!("tools[{index}]");
            let mut tool = object(value, &path)?;
            if required_string(&mut tool, "type", &format!("{path}.type"), false)? != "function" {
                return Err(unsupported(&format!("{path}.type")));
            }
            let function_path = format!("{path}.function");
            let mut function = object(
                required(&mut tool, "function", &function_path)?,
                &function_path,
            )?;
            let name = required_string(
                &mut function,
                "name",
                &format!("{function_path}.name"),
                false,
            )?;
            // Chat 的缺省是宽松函数 schema，不能让 Responses 的缺省收紧原始参数合同
            let strict =
                optional_bool(&mut function, "strict", &format!("{function_path}.strict"))?
                    .unwrap_or(false);
            let mut converted = json!({"type": "function", "name": name, "strict": strict});
            if let Some(description) = optional(&mut function, "description") {
                string(&description, &format!("{function_path}.description"), true)?;
                converted["description"] = description;
            }
            if let Some(parameters) = optional(&mut function, "parameters") {
                converted["parameters"] = parameters;
            }
            Ok(converted)
        })
        .collect()
}

fn tool_choice(value: Value, tools: Option<&Value>) -> Result<Value, RequestDecodeError> {
    let tools = tools.and_then(Value::as_array);
    if let Value::String(choice) = &value {
        return match choice.as_str() {
            "auto" | "none" => Ok(value),
            "required" if tools.is_some_and(|tools| !tools.is_empty()) => Ok(value),
            "required" => Err(invalid("tool_choice")),
            _ => Err(unsupported("tool_choice")),
        };
    }
    let mut choice = object(value, "tool_choice")?;
    if required_string(&mut choice, "type", "tool_choice.type", false)? != "function" {
        return Err(unsupported("tool_choice.type"));
    }
    let mut function = object(
        required(&mut choice, "function", "tool_choice.function")?,
        "tool_choice.function",
    )?;
    let name = required_string(&mut function, "name", "tool_choice.function.name", false)?;
    if !tools.is_some_and(|tools| {
        tools
            .iter()
            .any(|tool| tool.get("name").and_then(Value::as_str) == Some(name.as_str()))
    }) {
        return Err(invalid("tool_choice.function.name"));
    }
    Ok(json!({"type": "function", "name": name}))
}

fn response_format(value: Value) -> Result<Value, RequestDecodeError> {
    let mut format = object(value, "response_format")?;
    let kind = required_string(&mut format, "type", "response_format.type", false)?;
    match kind.as_str() {
        "text" | "json_object" => Ok(json!({"type": kind})),
        "json_schema" => {
            let mut schema = object(
                required(&mut format, "json_schema", "response_format.json_schema")?,
                "response_format.json_schema",
            )?;
            let name = required_string(
                &mut schema,
                "name",
                "response_format.json_schema.name",
                false,
            )?;
            let definition = required(&mut schema, "schema", "response_format.json_schema.schema")?;
            if definition.is_null() {
                return Err(invalid("response_format.json_schema.schema"));
            }
            let mut converted = json!({"type": "json_schema", "name": name, "schema": definition});
            if let Some(strict) =
                optional_bool(&mut schema, "strict", "response_format.json_schema.strict")?
            {
                converted["strict"] = strict.into();
            }
            if let Some(description) = optional(&mut schema, "description") {
                string(
                    &description,
                    "response_format.json_schema.description",
                    true,
                )?;
                converted["description"] = description;
            }
            Ok(converted)
        }
        _ => Err(unsupported("response_format.type")),
    }
}

fn optional(object: &mut Map<String, Value>, key: &str) -> Option<Value> {
    object.remove(key).filter(|value| !value.is_null())
}

fn required(
    object: &mut Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<Value, RequestDecodeError> {
    object
        .remove(key)
        .ok_or_else(|| RequestDecodeError::MissingField { field: path.into() })
}

fn object(value: Value, path: &str) -> Result<Map<String, Value>, RequestDecodeError> {
    match value {
        Value::Object(object) => Ok(object),
        _ => Err(invalid_type(path, "an object")),
    }
}

fn array(value: Value, path: &str) -> Result<Vec<Value>, RequestDecodeError> {
    match value {
        Value::Array(array) => Ok(array),
        _ => Err(invalid_type(path, "an array")),
    }
}

fn string<'a>(
    value: &'a Value,
    path: &str,
    allow_empty: bool,
) -> Result<&'a str, RequestDecodeError> {
    let value = value
        .as_str()
        .ok_or_else(|| invalid_type(path, "a string"))?;
    if !allow_empty && value.trim().is_empty() {
        return Err(RequestDecodeError::EmptyField { field: path.into() });
    }
    Ok(value)
}

fn required_string(
    object: &mut Map<String, Value>,
    key: &str,
    path: &str,
    allow_empty: bool,
) -> Result<String, RequestDecodeError> {
    let value = required(object, key, path)?;
    string(&value, path, allow_empty)?;
    match value {
        Value::String(value) => Ok(value),
        _ => Err(invalid_type(path, "a string")),
    }
}

fn optional_bool(
    object: &mut Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<Option<bool>, RequestDecodeError> {
    optional(object, key)
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| invalid_type(path, "a boolean"))
        })
        .transpose()
}

fn invalid_type(path: &str, expected: &'static str) -> RequestDecodeError {
    RequestDecodeError::InvalidType {
        field: path.into(),
        expected,
    }
}

fn invalid(path: &str) -> RequestDecodeError {
    RequestDecodeError::InvalidValue { field: path.into() }
}

fn unsupported(path: &str) -> RequestDecodeError {
    RequestDecodeError::UnsupportedField { field: path.into() }
}
