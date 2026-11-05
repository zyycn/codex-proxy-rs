//! 将 Chat 请求转换为 Responses 正文，协议选项留在 API，业务 JSON 与上游兼容规则保持原有归属

use std::collections::HashSet;

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
    if let Some(options) = optional(&mut chat, "web_search_options") {
        let tool = web_search_tool(options)?;
        // Chat 搜索要求先检索，不能用强制搜索覆盖调用方的函数工具或选择
        if let Some(tools) = optional(&mut chat, "tools")
            && !array(tools, "tools")?.is_empty()
        {
            return Err(RequestDecodeError::UnsupportedCombination {
                field: "tools",
                other: "web_search_options",
            });
        }
        if let Some(choice) = optional(&mut chat, "tool_choice")
            && choice != "auto"
        {
            return Err(RequestDecodeError::UnsupportedCombination {
                field: "tool_choice",
                other: "web_search_options",
            });
        }
        body.insert("tools".into(), Value::Array(vec![tool]));
        body.insert("tool_choice".into(), json!({"type": "web_search"}));
    } else {
        if let Some(value) = optional(&mut chat, "tools") {
            body.insert("tools".into(), tools(value)?.into());
        }
        if let Some(value) = optional(&mut chat, "tool_choice") {
            let choice = tool_choice(value, "tool_choice")?;
            body.insert("tool_choice".into(), choice);
        }
    }
    if let Some(value) = optional_bool(&mut chat, "parallel_tool_calls", "parallel_tool_calls")? {
        body.insert("parallel_tool_calls".into(), value.into());
    }

    let mut text = optional(&mut chat, "text")
        .map(|value| object(value, "text"))
        .transpose()?
        .unwrap_or_default();
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
    let mut reasoning = optional(&mut chat, "reasoning")
        .map(|value| object(value, "reasoning"))
        .transpose()?
        .unwrap_or_default();
    if let Some(value) = optional(&mut chat, "reasoning_effort") {
        string(&value, "reasoning_effort", false)?;
        reasoning.insert("effort".into(), value);
    }
    if !reasoning.is_empty() {
        body.insert("reasoning".into(), reasoning.into());
    }
    for field in ["max_output_tokens", "max_tokens", "max_completion_tokens"] {
        if let Some(value) = optional(&mut chat, field) {
            // 新字段优先，具体上游是否接受长度控制仍由现有 Provider 决定
            body.insert("max_output_tokens".into(), value);
        }
    }
    // 同语义字段不在入口预判模型支持范围，交由既有 Provider 与上游校验
    for field in [
        "temperature",
        "top_p",
        "user",
        "safety_identifier",
        "service_tier",
        "prompt_cache_key",
        "prompt_cache_retention",
        "metadata",
        "prompt_cache_options",
        "moderation",
        "instructions",
        "include",
        "max_tool_calls",
        "truncation",
        "context_management",
    ] {
        if let Some(value) = optional(&mut chat, field) {
            body.insert(field.into(), value);
        }
    }
    if optional_bool(&mut chat, "logprobs", "logprobs")? == Some(true) {
        let mut include = body
            .remove("include")
            .map(|value| array(value, "include"))
            .transpose()?
            .unwrap_or_default();
        let logprobs = json!("message.output_text.logprobs");
        if !include.contains(&logprobs) {
            include.push(logprobs);
        }
        body.insert("include".into(), include.into());
        if let Some(value) = optional(&mut chat, "top_logprobs") {
            body.insert("top_logprobs".into(), value);
        }
    }
    let ignored = [
        "stop",
        "seed",
        "frequency_penalty",
        "presence_penalty",
        "logit_bias",
        "prediction",
    ]
    .into_iter()
    .filter(|field| {
        chat.get(*field).is_some_and(|value| {
            !value.is_null()
                && value.as_f64() != Some(0.0)
                && !value.as_array().is_some_and(Vec::is_empty)
                && !value.as_object().is_some_and(Map::is_empty)
        })
    })
    .collect::<Vec<_>>();
    if !ignored.is_empty() {
        tracing::info!(target: "request_trace", stage="chat.compatibility", ignored_options=?ignored,
            "Chat-only optional controls omitted from Responses request");
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
            "store" | "background" => value == false,
            "functions" => value.as_array().is_some_and(Vec::is_empty),
            "modalities" => value == &json!(["text"]),
            "audio" | "function_call" | "input" | "previous_response_id" | "conversation" => false,
            _ => true,
        };
        if !supported {
            return Err(unsupported(field));
        }
    }
    Ok(())
}

fn web_search_tool(value: Value) -> Result<Value, RequestDecodeError> {
    let mut options = object(value, "web_search_options")?;
    let mut tool = json!({"type": "web_search"});
    if let Some(size) = optional(&mut options, "search_context_size") {
        string(&size, "web_search_options.search_context_size", false)?;
        tool["search_context_size"] = size;
    }
    if let Some(location) = optional(&mut options, "user_location") {
        let mut location = object(location, "web_search_options.user_location")?;
        let path = "web_search_options.user_location.type";
        if required_string(&mut location, "type", path, false)? != "approximate" {
            return Err(unsupported(path));
        }
        let path = "web_search_options.user_location.approximate";
        let mut approximate = object(required(&mut location, "approximate", path)?, path)?;
        let mut flat = json!({"type": "approximate"});
        for field in ["city", "country", "region", "timezone"] {
            if let Some(value) = optional(&mut approximate, field) {
                string(&value, &format!("{path}.{field}"), true)?;
                flat[field] = value;
            }
        }
        tool["user_location"] = flat;
    }
    Ok(tool)
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
    let mut custom_calls = HashSet::new();
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
        let name = optional(&mut message, "name").filter(|name| name != "");
        if let Some(name) = &name {
            let name_path = format!("{path}.name");
            string(name, &name_path, true)?;
        }
        // Chat 的显示名称和明文推理历史没有对应的 Responses 输入语义
        for field in ["function_call", "audio"] {
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
            let kind = if custom_calls.contains(&call_id) {
                "custom_tool_call_output"
            } else {
                "function_call_output"
            };
            let mut item = json!({"type": kind, "call_id": call_id, "output": output});
            if let Some(name) = name {
                item["name"] = name;
            }
            input.push(item);
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
        for call in calls {
            let id = string(&call["call_id"], &format!("{path}.tool_calls.id"), false)?.to_owned();
            if call["type"] == "custom_tool_call" {
                custom_calls.insert(id);
            } else {
                custom_calls.remove(&id);
            }
            input.push(call);
        }
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
            "text" | "input_text" | "output_text" => text_part(
                required_string(&mut part, "text", &format!("{path}.text"), true)?,
                is_assistant,
            ),
            "image_url" if matches!(role, "user" | "tool") => image_part(
                required(&mut part, "image_url", &format!("{path}.image_url"))?,
                &format!("{path}.image_url"),
            )?,
            "file" if matches!(role, "user" | "tool") => {
                let file_path = format!("{path}.file");
                file_part(required(&mut part, "file", &file_path)?, &file_path)?
            }
            "input_audio" if matches!(role, "user" | "tool") => audio_part(&mut part, &path)?,
            "input_image" | "input_file" if matches!(role, "user" | "tool") => {
                part.insert("type".into(), kind.into());
                Value::Object(std::mem::take(&mut part))
            }
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

fn file_part(value: Value, path: &str) -> Result<Value, RequestDecodeError> {
    let mut file = object(value, path)?;
    if !["file_id", "file_url", "file_data"]
        .iter()
        .any(|field| file.get(*field).is_some_and(|value| !value.is_null()))
    {
        return Err(invalid(path));
    }
    // 文件可用性、格式和组合由接收文件的上游判断，网关不下载或重编码
    file.insert("type".into(), "input_file".into());
    Ok(file.into())
}

fn audio_part(part: &mut Map<String, Value>, path: &str) -> Result<Value, RequestDecodeError> {
    if let Some(value) = optional(part, "input_audio") {
        let path = format!("{path}.input_audio");
        let mut audio = object(value, &path)?;
        let data = required_string(&mut audio, "data", &format!("{path}.data"), false)?;
        let format = required_string(&mut audio, "format", &format!("{path}.format"), false)?;
        let mime = if format == "mp3" { "mpeg" } else { &format };
        Ok(json!({"type":"input_audio", "audio_url":format!("data:audio/{mime};base64,{data}")}))
    } else {
        // Codex 使用 audio_url/file_id 引用，原生扩展保持其结构
        part.insert("type".into(), "input_audio".into());
        Ok(Value::Object(std::mem::take(part)))
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
    array(value, path)?
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            let path = format!("{path}[{index}]");
            let mut call = object(value, &path)?;
            let id = required_string(&mut call, "id", &format!("{path}.id"), false)?;
            let kind = required_string(&mut call, "type", &format!("{path}.type"), false)?;
            let (output_type, data_field) = match kind.as_str() {
                "function" => ("function_call", "arguments"),
                "custom" => ("custom_tool_call", "input"),
                _ => return Err(unsupported(&format!("{path}.type"))),
            };
            let nested_path = format!("{path}.{kind}");
            let mut nested = object(required(&mut call, &kind, &nested_path)?, &nested_path)?;
            let name = required_string(&mut nested, "name", &format!("{nested_path}.name"), false)?;
            let data = required_string(
                &mut nested,
                data_field,
                &format!("{nested_path}.{data_field}"),
                true,
            )?;
            Ok(json!({"type":output_type, "call_id":id, "name":name, data_field:data}))
        })
        .collect()
}

fn tools(value: Value) -> Result<Vec<Value>, RequestDecodeError> {
    array(value, "tools")?
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            let path = format!("tools[{index}]");
            let mut tool = object(value, &path)?;
            let kind = required_string(&mut tool, "type", &format!("{path}.type"), false)?;
            if matches!(kind.as_str(), "function" | "custom") {
                if let Some(nested) = optional(&mut tool, &kind) {
                    tool = object(nested, &format!("{path}.{kind}"))?;
                }
                if kind == "function" && tool.get("strict").is_none_or(Value::is_null) {
                    // Chat 的缺省是宽松 schema，不能让 Responses 收紧参数合同
                    tool.insert("strict".into(), false.into());
                }
            }
            // hosted 工具采用原生 Responses 结构，支持范围由选中的 Provider 决定
            tool.insert("type".into(), kind.into());
            Ok(tool.into())
        })
        .collect()
}

fn tool_choice(value: Value, path: &str) -> Result<Value, RequestDecodeError> {
    if value.is_string() {
        return Ok(value);
    }
    let mut choice = object(value, path)?;
    let kind = required_string(&mut choice, "type", &format!("{path}.type"), false)?;
    if matches!(kind.as_str(), "function" | "custom") {
        if let Some(nested) = optional(&mut choice, &kind) {
            choice = object(nested, &format!("{path}.{kind}"))?;
        }
    } else if kind == "allowed_tools" {
        if let Some(nested) = optional(&mut choice, "allowed_tools") {
            choice = object(nested, &format!("{path}.allowed_tools"))?;
        }
        if let Some(value) = optional(&mut choice, "tools") {
            let mapped = array(value, &format!("{path}.tools"))?
                .into_iter()
                .enumerate()
                .map(|(index, value)| tool_choice(value, &format!("{path}.tools[{index}]")))
                .collect::<Result<Vec<_>, _>>()?;
            choice.insert("tools".into(), mapped.into());
        }
    }
    choice.insert("type".into(), kind.into());
    Ok(choice.into())
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
        _ => {
            format.insert("type".into(), kind.into());
            Ok(format.into())
        }
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
