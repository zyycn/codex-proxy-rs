//! Messages 内容到 GenAI 的纯协议映射，签名载荷只按明确来源恢复

use crate::anthropic::{GENAI_CARRIER as SIGNATURE_PREFIX, TranslationError};
use serde_json::{Map, Value, json};
use std::{borrow::Cow, collections::BTreeMap};

fn invalid() -> TranslationError {
    TranslationError::InvalidRequest(
        "The request contains content unsupported by the GenAI adapter.",
    )
}

/// 转换 Messages 请求；不支持的内容返回错误，避免静默丢失客户端输入
pub fn messages_to_genai_request(
    request: &Map<String, Value>,
) -> Result<Map<String, Value>, TranslationError> {
    let mut result = Map::new();
    if let Some(system) = request.get("system") {
        result.insert(
            "systemInstruction".into(),
            json!({"parts":plain_parts(system)?}),
        );
    }
    let mut calls = BTreeMap::new();
    let mut contents = Vec::new();
    for message in request
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(invalid)?
    {
        let blocks = match &message["content"] {
            Value::String(text) => Cow::Owned(vec![json!({"type":"text","text":text})]),
            Value::Array(blocks) => Cow::Borrowed(blocks.as_slice()),
            _ => return Err(invalid()),
        };
        // 空内容消息不承载对话；客户端的空占位不应进入上游历史
        if blocks.is_empty() {
            continue;
        }
        let role = match message["role"].as_str() {
            Some("assistant") => "model",
            Some("user") => "user",
            _ => return Err(invalid()),
        };
        let mut parts: Vec<Value> = Vec::new();
        let mut pending_signature = None;
        for block in blocks.iter() {
            let mut part = match block["type"].as_str() {
                Some("text") => json!({"text":block["text"]}),
                Some("image" | "document") => media(&block["source"])?,
                Some("thinking") => {
                    if let Some(raw) = block["signature"]
                        .as_str()
                        .and_then(|s| s.strip_prefix(SIGNATURE_PREFIX))
                    {
                        let carrier: Value = serde_json::from_str(raw).map_err(|_| invalid())?;
                        match carrier["target"].as_str() {
                            Some("previous") => {
                                let last = parts.last_mut().ok_or_else(invalid)?;
                                last["thoughtSignature"] = carrier["signature"].clone();
                                continue;
                            }
                            Some("next") => {
                                pending_signature = Some(carrier["signature"].clone());
                                continue;
                            }
                            Some("thinking") => {
                                json!({"text":block["thinking"],"thought":true,"thoughtSignature":carrier["signature"]})
                            }
                            _ => return Err(invalid()),
                        }
                    } else {
                        // 没有本协议签名的思考不作为可验证历史回放
                        continue;
                    }
                }
                Some("redacted_thinking") => continue,
                Some("tool_use") => {
                    let id = block["id"].as_str().ok_or_else(invalid)?;
                    let name = block["name"].as_str().ok_or_else(invalid)?;
                    calls.insert(id.to_owned(), name.to_owned());
                    if !block["input"].is_object() {
                        return Err(invalid());
                    }
                    json!({"functionCall":{"id":id,"name":name,"args":block["input"]}})
                }
                Some("tool_result") => {
                    let id = block["tool_use_id"].as_str().ok_or_else(invalid)?;
                    let name = calls.get(id).ok_or_else(invalid)?;
                    let output = &block["content"];
                    let mut response =
                        json!({"functionResponse":{"id":id,"name":name,"response":{}}});
                    let key = if block["is_error"] == true {
                        "error"
                    } else {
                        "output"
                    };
                    response["functionResponse"]["response"][key] = output.clone();
                    response
                }
                _ => return Err(invalid()),
            };
            if part.get("thoughtSignature").is_some_and(Value::is_null) {
                part.as_object_mut()
                    .ok_or_else(invalid)?
                    .remove("thoughtSignature");
            }
            if let Some(signature) = pending_signature.take() {
                part["thoughtSignature"] = signature;
            }
            // Google 为外部历史提供的兜底值；真实签名优先，Chat 等无签名合同无需服务端缓存
            // https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/thinking/thought-signatures
            if part.get("functionCall").is_some() && part.get("thoughtSignature").is_none() {
                part["thoughtSignature"] = json!("skip_thought_signature_validator");
            }
            parts.push(part);
        }
        if pending_signature.is_some() {
            return Err(invalid());
        }
        // Chat 工具轮次可带空 content；签名已附着后再删除无内容且无签名的占位
        parts.retain(|part| part["text"] != "" || part.get("thoughtSignature").is_some());
        if !parts.is_empty() {
            contents.push(json!({"role":role,"parts":parts}));
        }
    }
    result.insert("contents".into(), json!(contents));
    let mut generation = Map::new();
    for (from, to) in [
        ("max_tokens", "maxOutputTokens"),
        ("temperature", "temperature"),
        ("top_p", "topP"),
        ("top_k", "topK"),
        ("stop_sequences", "stopSequences"),
    ] {
        if let Some(value) = request.get(from) {
            generation.insert(to.into(), value.clone());
        }
    }
    if let Some(thinking) = request.get("thinking") {
        match thinking["type"].as_str() {
            Some("disabled") => {
                generation.insert("thinkingConfig".into(), json!({"thinkingBudget":0}));
            }
            Some("enabled") => {
                generation.insert(
                    "thinkingConfig".into(),
                    json!({"thinkingBudget":thinking["budget_tokens"],"includeThoughts":true}),
                );
            }
            Some("adaptive" | "auto") => {
                generation.insert(
                    "thinkingConfig".into(),
                    json!({"thinkingBudget":-1,"includeThoughts":true}),
                );
            }
            _ => return Err(invalid()),
        }
    }
    if let Some(format) = request.get("output_config").and_then(|c| c.get("format")) {
        if format["type"] != "json_schema" {
            return Err(invalid());
        }
        generation.insert("responseMimeType".into(), json!("application/json"));
        generation.insert("responseSchema".into(), format["schema"].clone());
    }
    result.insert("generationConfig".into(), Value::Object(generation));
    if let Some(tools) = request.get("tools").and_then(Value::as_array) {
        let declarations = tools
            .iter()
            .map(|tool| {
                if tool
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|t| t != "custom")
                    || !tool["input_schema"].is_object()
                {
                    return Err(invalid());
                }
                let mut declaration =
                    json!({"name":tool["name"],"parametersJsonSchema":tool["input_schema"]});
                if let Some(description) = tool.get("description") {
                    declaration["description"] = description.clone();
                }
                Ok(declaration)
            })
            .collect::<Result<Vec<_>, _>>()?;
        result.insert(
            "tools".into(),
            json!([{"functionDeclarations":declarations}]),
        );
    }
    if let Some(choice) = request.get("tool_choice") {
        let mode = match choice["type"].as_str() {
            Some("auto") => "AUTO",
            Some("any" | "tool") => "ANY",
            Some("none") => "NONE",
            _ => return Err(invalid()),
        };
        let mut config = json!({"mode":mode});
        if choice["type"] == "tool" {
            config["allowedFunctionNames"] = json!([choice["name"]]);
        }
        result.insert("toolConfig".into(), json!({"functionCallingConfig":config}));
    }
    Ok(result)
}

fn plain_parts(value: &Value) -> Result<Vec<Value>, TranslationError> {
    if let Some(text) = value.as_str() {
        return Ok(vec![json!({"text":text})]);
    }
    value
        .as_array()
        .ok_or_else(invalid)?
        .iter()
        .map(|block| {
            if block["type"] != "text" {
                return Err(invalid());
            }
            Ok(json!({"text":block["text"]}))
        })
        .collect()
}

fn media(source: &Value) -> Result<Value, TranslationError> {
    match source["type"].as_str() {
        Some("base64") => {
            Ok(json!({"inlineData":{"mimeType":source["media_type"],"data":source["data"]}}))
        }
        Some("text") => Ok(json!({"text":source["data"]})),
        // URL 文件需有明确 MIME 类型；不替客户端下载未知资源
        Some("url") if source["media_type"].is_string() => {
            Ok(json!({"fileData":{"mimeType":source["media_type"],"fileUri":source["url"]}}))
        }
        _ => Err(invalid()),
    }
}
