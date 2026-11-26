//! Chat 内容、工具与生成设置直接构造 GenAI 正文

use crate::chat_completions::{
    ChatError, custom_tool_schema,
    error::{invalid, unsupported},
    request::string,
    validate_request,
};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub fn convert_request(chat: &Map<String, Value>) -> Result<Map<String, Value>, ChatError> {
    validate_request(chat)?;
    let mut system = Vec::new();
    if let Some(instructions) = chat.get("instructions").filter(|v| !v.is_null()) {
        system.extend(parts(instructions, "instructions", true)?);
    }
    let mut contents = Vec::new();
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
            system.extend(parts(
                &message["content"],
                &format!("{path}.content"),
                true,
            )?);
            continue;
        }
        let content = if role == "tool" {
            let id = string(
                message.get("tool_call_id"),
                &format!("{path}.tool_call_id"),
                false,
            )?;
            let name = calls
                .get(id)
                .ok_or_else(|| invalid(&format!("{path}.tool_call_id")))?;
            if !used.insert(id.to_owned()) {
                return Err(invalid(&format!("{path}.tool_call_id")));
            }
            let mut text = Vec::new();
            let mut media = Vec::new();
            for part in parts(&message["content"], &format!("{path}.content"), false)? {
                if let Some(value) = part.get("text").and_then(Value::as_str) {
                    text.push(value.to_owned());
                } else {
                    media.push(part);
                }
            }
            let mut response = json!({"functionResponse":{"id":id,"name":name,"response":{"output":text.join("\n")}}});
            if !media.is_empty() {
                response["functionResponse"]["parts"] = media.into();
            }
            vec![response]
        } else {
            let mut content = if message["content"].is_null() {
                Vec::new()
            } else {
                parts(&message["content"], &format!("{path}.content"), false)?
            };
            if let Some(refusal) = message.get("refusal").and_then(Value::as_str) {
                content.push(json!({"text":refusal}));
            }
            for (call_index, call) in message
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
            {
                let path = format!("{path}.tool_calls[{call_index}]");
                let id = string(call.get("id"), &format!("{path}.id"), false)?;
                let kind = call["type"].as_str().ok_or_else(|| invalid(&path))?;
                let name = string(
                    call[kind].get("name"),
                    &format!("{path}.{kind}.name"),
                    false,
                )?;
                if calls.insert(id.to_owned(), name.to_owned()).is_some() {
                    return Err(invalid(&format!("{path}.id")));
                }
                let arguments = if kind == "custom" {
                    json!({"input":call[kind]["input"]})
                } else {
                    serde_json::from_str::<Value>(string(
                        call[kind].get("arguments"),
                        &format!("{path}.function.arguments"),
                        true,
                    )?)
                    .map_err(|_| invalid(&format!("{path}.function.arguments")))?
                };
                if !arguments.is_object() {
                    return Err(invalid(&format!("{path}.function.arguments")));
                }
                // Chat 历史没有 Google 签名，采用官方允许的外部工具历史标记
                content.push(json!({"functionCall":{"id":id,"name":name,"args":arguments},"thoughtSignature":"skip_thought_signature_validator"}));
            }
            content
        };
        append(
            &mut contents,
            if role == "assistant" { "model" } else { "user" },
            content,
        );
    }
    if contents.is_empty() {
        return Err(invalid("messages"));
    }
    let mut result = Map::from_iter([("contents".into(), contents.into())]);
    if !system.is_empty() {
        result.insert("systemInstruction".into(), json!({"parts":system}));
    }
    let mut generation = Map::new();
    if let Some(value) = ["max_completion_tokens", "max_tokens", "max_output_tokens"]
        .into_iter()
        .find_map(|key| chat.get(key).filter(|v| !v.is_null()))
    {
        let max = value
            .as_u64()
            .filter(|v| *v > 0)
            .ok_or_else(|| invalid("max_completion_tokens"))?;
        generation.insert("maxOutputTokens".into(), json!(max));
    }
    for (from, to) in [("temperature", "temperature"), ("top_p", "topP")] {
        if let Some(value) = chat.get(from).filter(|v| !v.is_null()) {
            generation.insert(to.into(), value.clone());
        }
    }
    if let Some(effort) = chat
        .get("reasoning_effort")
        .or_else(|| chat.get("reasoning").and_then(|v| v.get("effort")))
        .filter(|v| !v.is_null())
    {
        let budget: i64 = match effort.as_str() {
            Some("none") => 0,
            Some("minimal" | "low") => 1024,
            Some("medium") => 8192,
            Some("high") => 24576,
            Some("xhigh") => 32768,
            Some("max") => 128000,
            Some("auto") => -1,
            _ => return Err(invalid("reasoning_effort")),
        };
        generation.insert(
            "thinkingConfig".into(),
            if budget == 0 {
                json!({"thinkingBudget":0})
            } else {
                json!({"thinkingBudget":budget,"includeThoughts":true})
            },
        );
    }
    if let Some(format) = chat
        .get("response_format")
        .or_else(|| chat.get("text").and_then(|v| v.get("format")))
        .filter(|v| !v.is_null())
    {
        match format["type"].as_str() {
            Some("json_schema") => {
                generation.insert("responseMimeType".into(), json!("application/json"));
                let schema = format
                    .get("json_schema")
                    .unwrap_or(format)
                    .get("schema")
                    .filter(|v| v.is_object())
                    .ok_or_else(|| invalid("response_format.json_schema.schema"))?;
                generation.insert("responseJsonSchema".into(), schema.clone());
            }
            Some("json_object") => {
                generation.insert("responseMimeType".into(), json!("application/json"));
            }
            Some("text") => {}
            _ => return Err(unsupported("response_format.type")),
        }
    }
    if chat.get("logprobs") == Some(&Value::Bool(true)) {
        generation.insert("responseLogprobs".into(), json!(true));
        if let Some(value) = chat.get("top_logprobs") {
            generation.insert("logprobs".into(), value.clone());
        }
    }
    tools(chat, &mut result)?;
    if !generation.is_empty() {
        result.insert("generationConfig".into(), generation.into());
    }
    Ok(result)
}

fn append(contents: &mut Vec<Value>, role: &str, parts: Vec<Value>) {
    if let Some(last) = contents.last_mut().filter(|v| v["role"] == role) {
        last["parts"]
            .as_array_mut()
            .expect("已构造的内容为数组")
            .extend(parts);
    } else {
        contents.push(json!({"role":role,"parts":parts}));
    }
}

fn parts(value: &Value, path: &str, system: bool) -> Result<Vec<Value>, ChatError> {
    if let Some(text) = value.as_str() {
        return Ok(vec![json!({"text":text})]);
    }
    value
        .as_array()
        .ok_or_else(|| invalid(path))?
        .iter()
        .enumerate()
        .map(|(index, part)| {
            let path = format!("{path}[{index}]");
            let kind = string(part.get("type"), &format!("{path}.type"), false)?;
            match kind {
                "text" | "input_text" | "output_text" => {
                    Ok(json!({"text":string(part.get("text"), &format!("{path}.text"), true)?}))
                }
                "refusal" if !system => Ok(
                    json!({"text":string(part.get("refusal"), &format!("{path}.refusal"), true)?}),
                ),
                "image_url" | "input_image" if !system => {
                    let url = part
                        .get("image_url")
                        .and_then(|v| v.as_str().or_else(|| v.get("url").and_then(Value::as_str)))
                        .ok_or_else(|| invalid(&format!("{path}.image_url")))?;
                    media(url, part.get("mime_type").and_then(Value::as_str), &path)
                }
                "file" | "input_file" if !system => {
                    let file = if kind == "file" { &part["file"] } else { part };
                    let url = file
                        .get("file_data")
                        .or_else(|| file.get("file_url"))
                        .and_then(Value::as_str)
                        .ok_or_else(|| unsupported(&format!("{path}.file_id")))?;
                    let mime = file.get("mime_type").and_then(Value::as_str).or_else(|| {
                        file.get("filename")
                            .and_then(Value::as_str)
                            .filter(|v| v.ends_with(".pdf"))
                            .map(|_| "application/pdf")
                    });
                    media(url, mime, &path)
                }
                "input_audio" if !system => {
                    let audio = &part["input_audio"];
                    let data = string(
                        audio.get("data"),
                        &format!("{path}.input_audio.data"),
                        false,
                    )?;
                    let format = string(
                        audio.get("format"),
                        &format!("{path}.input_audio.format"),
                        false,
                    )?;
                    let mime = match format {
                        "wav" => "audio/wav",
                        "mp3" => "audio/mpeg",
                        _ => return Err(unsupported(&format!("{path}.input_audio.format"))),
                    };
                    Ok(json!({"inlineData":{"mimeType":mime,"data":data}}))
                }
                _ => Err(unsupported(&format!("{path}.type"))),
            }
        })
        .collect()
}

fn media(data: &str, mime: Option<&str>, path: &str) -> Result<Value, ChatError> {
    if let Some(data) = data.strip_prefix("data:") {
        let (mime, data) = data
            .split_once(";base64,")
            .filter(|(mime, data)| !mime.is_empty() && !data.is_empty())
            .ok_or_else(|| invalid(path))?;
        return Ok(json!({"inlineData":{"mimeType":mime,"data":data}}));
    }
    let mime = mime.ok_or_else(|| unsupported(&format!("{path}.mime_type")))?;
    if let Ok(url) = url::Url::parse(data)
        && matches!(url.scheme(), "https" | "http" | "gs")
    {
        return Ok(json!({"fileData":{"mimeType":mime,"fileUri":data}}));
    }
    if !data.is_empty() {
        return Ok(json!({"inlineData":{"mimeType":mime,"data":data}}));
    }
    Err(invalid(path))
}

fn tools(chat: &Map<String, Value>, result: &mut Map<String, Value>) -> Result<(), ChatError> {
    let mut declarations = Vec::new();
    if let Some(tools) = chat.get("tools").filter(|v| !v.is_null()) {
        for (index, tool) in tools
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
            let definition = &tool[kind];
            let name = string(
                definition.get("name"),
                &format!("{path}.{kind}.name"),
                false,
            )?;
            let parameters = if kind == "custom" {
                custom_tool_schema(definition.get("format"), &format!("{path}.custom.format"))?
            } else {
                definition
                    .get("parameters")
                    .cloned()
                    .unwrap_or_else(|| json!({"type":"object","properties":{}}))
            };
            if !parameters.is_object() {
                return Err(invalid(&format!("{path}.function.parameters")));
            }
            let mut declaration = json!({"name":name,"parametersJsonSchema":parameters});
            if let Some(value) = definition.get("description") {
                declaration["description"] = value.clone();
            }
            declarations.push(declaration);
        }
    }
    let mut choice = chat.get("tool_choice").cloned().unwrap_or(Value::Null);
    if let Some(options) = chat.get("web_search_options").filter(|v| !v.is_null()) {
        if !options.is_object()
            || !declarations.is_empty()
            || (!choice.is_null() && choice != "auto")
        {
            return Err(invalid("web_search_options"));
        }
        for field in ["user_location", "search_context_size"] {
            if options.get(field).is_some_and(|v| !v.is_null()) {
                return Err(unsupported(&format!("web_search_options.{field}")));
            }
        }
        result.insert("tools".into(), json!([{"googleSearch":{}}]));
        return Ok(());
    }
    if choice["type"] == "allowed_tools" {
        let allowed = choice.get("allowed_tools").unwrap_or(&choice);
        let permitted = allowed["tools"]
            .as_array()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| invalid("tool_choice.allowed_tools.tools"))?;
        declarations.retain(|tool| {
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
    if declarations.is_empty() {
        if !choice.is_null() && choice != "auto" && choice != "none" {
            return Err(invalid("tool_choice"));
        }
        return Ok(());
    }
    let kind = choice
        .as_str()
        .or_else(|| choice.get("type").and_then(Value::as_str))
        .unwrap_or("auto");
    let mut config = match kind {
        "auto" => json!({"mode":"AUTO"}),
        "none" => json!({"mode":"NONE"}),
        "required" => json!({"mode":"ANY"}),
        "function" | "custom" => {
            let name = string(
                choice.get(kind).and_then(|v| v.get("name")),
                "tool_choice.name",
                false,
            )?;
            if !declarations.iter().any(|v| v["name"] == name) {
                return Err(invalid("tool_choice"));
            }
            json!({"mode":"ANY","allowedFunctionNames":[name]})
        }
        _ => return Err(unsupported("tool_choice")),
    };
    // Google 的函数调用配置没有禁止并行的同义字段，不伪造未受支持的约束
    if chat.get("parallel_tool_calls") == Some(&Value::Bool(false)) {
        return Err(unsupported("parallel_tool_calls"));
    }
    if config["mode"] == "ANY" && config.get("allowedFunctionNames").is_none() {
        config["allowedFunctionNames"] = declarations
            .iter()
            .map(|v| v["name"].clone())
            .collect::<Vec<_>>()
            .into();
    }
    result.insert(
        "tools".into(),
        json!([{"functionDeclarations":declarations}]),
    );
    if !choice.is_null() {
        result.insert("toolConfig".into(), json!({"functionCallingConfig":config}));
    }
    Ok(())
}
