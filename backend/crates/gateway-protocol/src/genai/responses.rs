//! Responses 输入、函数声明与生成参数直接映射到 GenAI

use super::parts::{TurnParts, content, function_response, invalid};
use crate::anthropic::{
    ToolSet, TranslationError, allowed_tools, restore_anthropic, split_data_url,
};
use crate::openai::web_search;
use serde_json::{Map, Value, json};
use std::{borrow::Cow, collections::BTreeMap};

/// 普通生成只映射函数工具，独立搜索由 Provider 按账号能力编排
pub fn responses_to_genai_request(
    request: &Map<String, Value>,
) -> Result<Map<String, Value>, TranslationError> {
    let mut generation = Map::new();
    if let Some(value) = request
        .get("max_output_tokens")
        .filter(|value| !value.is_null())
    {
        let maximum =
            value
                .as_u64()
                .filter(|value| *value > 0)
                .ok_or(TranslationError::InvalidRequest(
                    "The max_output_tokens field must be a positive integer.",
                ))?;
        generation.insert("maxOutputTokens".into(), json!(maximum));
    }
    for (from, to) in [("temperature", "temperature"), ("top_p", "topP")] {
        if let Some(value) = request.get(from) {
            generation.insert(to.into(), value.clone());
        }
    }
    if let Some(thinking) = request
        .get("reasoning")
        .and_then(|value| value.get("effort"))
        .and_then(Value::as_str)
        .and_then(thinking_config)
    {
        generation.insert("thinkingConfig".into(), thinking);
    }
    if let Some(format) = request.get("text").and_then(|value| value.get("format"))
        && format["type"] == "json_schema"
    {
        generation.insert("responseMimeType".into(), json!("application/json"));
        generation.insert(
            "responseSchema".into(),
            format.get("schema").cloned().unwrap_or_else(|| json!({})),
        );
    }
    let tools = ToolSet::from_responses(request);
    let mut result = conversation(request, &tools)?;
    write_tools(request, &tools, &mut result)?;
    result.insert("generationConfig".into(), Value::Object(generation));
    Ok(result)
}

fn thinking_config(effort: &str) -> Option<Value> {
    // 档位预算不受 Messages 的必填输出上限、强制工具或无思考续轮规则影响
    let budget = match effort.trim().to_ascii_lowercase().as_str() {
        "none" => return Some(json!({"thinkingBudget":0})),
        "minimal" | "low" => 1024,
        "medium" => 8192,
        "high" => 24576,
        "xhigh" => 32768,
        "max" => 128000,
        "auto" => -1,
        _ => return None,
    };
    Some(json!({"thinkingBudget":budget,"includeThoughts":true}))
}

fn content_part(part: &Value) -> Result<Value, TranslationError> {
    let data = match part["type"].as_str() {
        Some("text" | "input_text" | "output_text") => {
            return Ok(json!({"text":part["text"].as_str().ok_or_else(invalid)?}));
        }
        Some("refusal") => {
            return Ok(json!({"text":part["refusal"].as_str().ok_or_else(invalid)?}));
        }
        Some("input_image") => part["image_url"]
            .as_str()
            .or_else(|| part["image_url"]["url"].as_str()),
        Some("input_file") => part["file_data"]
            .as_str()
            .or_else(|| part["file_url"].as_str()),
        _ => return Err(invalid()),
    }
    .ok_or_else(invalid)?;
    if let Some((mime, bytes)) = split_data_url(data) {
        return Ok(json!({"inlineData":{"mimeType":mime,"data":bytes}}));
    }
    if let Some(mime) = part["mime_type"].as_str() {
        return Ok(json!({"fileData":{"mimeType":mime,"fileUri":data}}));
    }
    Err(invalid())
}

fn content_parts(content: &Value) -> Result<Vec<Value>, TranslationError> {
    match content {
        Value::String(text) => Ok(vec![json!({"text":text})]),
        Value::Array(parts) => parts.iter().map(content_part).collect(),
        _ => Err(invalid()),
    }
}

fn conversation(
    request: &Map<String, Value>,
    tools: &ToolSet<&Value>,
) -> Result<Map<String, Value>, TranslationError> {
    let mut result = Map::new();
    let mut system = Vec::new();
    if let Some(text) = request
        .get("instructions")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        system.push(json!({"text":text}));
    }
    let mut contents = Vec::new();
    let mut history = Vec::new();
    let input = match request.get("input") {
        None | Some(Value::Null) => &[][..],
        Some(Value::String(text)) => {
            if !text.is_empty() {
                contents.push(json!({"role":"user","parts":[{"text":text}]}));
            }
            &[]
        }
        Some(Value::Array(items)) => items.as_slice(),
        _ => {
            return Err(TranslationError::InvalidRequest(
                "The input field must be a string or an array.",
            ));
        }
    };
    for (pos, item) in input.iter().enumerate() {
        let kind = item["type"].as_str().unwrap_or("message");
        let role = match kind {
            "message" => match item["role"].as_str().unwrap_or("user") {
                "system" | "developer" => {
                    let parts = content_parts(&item["content"])?;
                    if parts.iter().any(|part| !part["text"].is_string()) {
                        return Err(invalid());
                    }
                    system.extend(parts);
                    continue;
                }
                "assistant" => "model",
                "user" => "user",
                _ => return Err(invalid()),
            },
            "reasoning" => {
                // CPR 的历史签名载荷包含完整思考块，其他来源的推理不应打断相邻轮次
                if let Some(block) = restore_anthropic(item)
                    && block["type"] == "thinking"
                    && block["signature"]
                        .as_str()
                        .is_some_and(|v| v.starts_with(crate::anthropic::GENAI_CARRIER))
                {
                    history.push(InputItem {
                        role: "model",
                        position: pos,
                        body: Cow::Owned(block),
                    });
                }
                continue;
            }
            "function_call" | "custom_tool_call" => "model",
            "function_call_output" | "custom_tool_call_output" => "user",
            // 托管工具的历史事件不包含可回放的模型内容
            "web_search_call" => continue,
            _ => return Err(invalid()),
        };
        history.push(InputItem {
            role,
            position: pos,
            body: Cow::Borrowed(item),
        });
    }
    let mut calls = Vec::new();
    for turn in history.chunk_by(|a, b| a.role == b.role) {
        let role = turn[0].role;
        let parts = if role == "model" {
            model_turn(turn, tools, &mut calls)?
        } else {
            user_turn(turn, &mut calls)?
        };
        if !parts.is_empty() {
            contents.push(content(role, parts));
        }
    }
    if !calls.is_empty() {
        contents.push(content("user", user_turn(&[], &mut calls)?));
    }
    result.insert("contents".into(), Value::Array(contents));
    if !system.is_empty() {
        result.insert("systemInstruction".into(), json!({"parts":system}));
    }
    Ok(result)
}

struct InputItem<'a> {
    role: &'static str,
    position: usize,
    body: Cow<'a, Value>,
}

fn model_turn(
    turn: &[InputItem<'_>],
    tools: &ToolSet<&Value>,
    calls: &mut Vec<(String, String)>,
) -> Result<Vec<Value>, TranslationError> {
    let mut parts = TurnParts::default();
    for entry in turn {
        let item = entry.body.as_ref();
        match item["type"].as_str().unwrap_or("message") {
            "thinking" => parts.thinking(item)?,
            "message" => {
                for part in content_parts(&item["content"])? {
                    parts.push(part);
                }
            }
            _ => {
                let name = tools.wire_name(
                    item["name"].as_str().ok_or_else(invalid)?,
                    item["namespace"].as_str(),
                );
                let id = item["call_id"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("call_{}", entry.position));
                let args = if item["type"] == "custom_tool_call" {
                    json!({"input":item["input"].as_str().ok_or_else(invalid)?})
                } else {
                    serde_json::from_str::<Value>(item["arguments"].as_str().unwrap_or("{}"))
                        .map_err(|_| invalid())?
                };
                if !args.is_object() {
                    return Err(invalid());
                }
                parts.push(json!({"functionCall":{"id":id,"name":name,"args":args}}));
                calls.push((id, name.to_owned()));
            }
        }
    }
    parts.finish()
}

fn user_turn(
    turn: &[InputItem<'_>],
    calls: &mut Vec<(String, String)>,
) -> Result<Vec<Value>, TranslationError> {
    let mut outputs = BTreeMap::new();
    for entry in turn {
        let item = entry.body.as_ref();
        if matches!(
            item["type"].as_str(),
            Some("function_call_output" | "custom_tool_call_output")
        ) {
            outputs.insert(
                item["call_id"].as_str().unwrap_or_default(),
                (entry.position, item),
            );
        }
    }
    // 同轮按调用顺序闭合，重复返回只取最后一项，迟到的结果保留为普通用户内容
    let mut parts = Vec::new();
    for (id, name) in calls.drain(..) {
        let response = if let Some((_, item)) = outputs.remove(id.as_str()) {
            function_response(
                &id,
                &name,
                item["status"] == "incomplete",
                &item["output"],
                content_part,
            )?
        } else {
            function_response(
                &id,
                &name,
                true,
                &json!("Tool call was interrupted before any output was recorded."),
                content_part,
            )?
        };
        parts.push(response);
    }
    for entry in turn {
        let item = entry.body.as_ref();
        if item["type"].as_str().unwrap_or("message") == "message" {
            parts.extend(
                content_parts(&item["content"])?
                    .into_iter()
                    .filter(|part| part["text"] != ""),
            );
        } else if outputs
            .get(item["call_id"].as_str().unwrap_or_default())
            .is_some_and(|(last, _)| *last == entry.position)
        {
            let output = &item["output"];
            let blocks = if output.is_array() || output.is_string() {
                content_parts(output)?
            } else {
                vec![json!({"text":output.to_string()})]
            };
            let before = parts.len();
            parts.extend(
                blocks
                    .into_iter()
                    .filter(|p| p["text"].as_str().is_none_or(|s| !s.trim().is_empty())),
            );
            if parts.len() == before {
                parts.push(json!({"text":"Tool result was empty."}));
            }
        }
    }
    Ok(parts)
}

fn write_tools(
    request: &Map<String, Value>,
    tools: &ToolSet<&Value>,
    result: &mut Map<String, Value>,
) -> Result<(), TranslationError> {
    let choice = request.get("tool_choice").unwrap_or(&Value::Null);
    let allowed = allowed_tools(choice)?;
    let mut declarations = Vec::new();
    for (name, tool) in tools.declarations(allowed) {
        let schema = tool.schema();
        if !schema.is_object() {
            return Err(invalid());
        }
        let mut declaration = json!({"name":name,"parametersJsonSchema":schema});
        if let Some(description) = tool.definition.get("description") {
            declaration["description"] = description.clone();
        }
        declarations.push(declaration);
    }
    let kind = if allowed.is_some() {
        match choice["mode"].as_str() {
            Some(mode @ ("required" | "auto")) => Some(mode),
            _ => return Err(invalid()),
        }
    } else {
        choice.as_str().or_else(|| choice["type"].as_str())
    };
    if declarations.is_empty() {
        if matches!(kind, Some("required" | "function" | "custom"))
            || allowed.is_some_and(|a| !a.iter().all(web_search::is_search))
        {
            return Err(TranslationError::InvalidRequest(
                "No declared function can satisfy the tool choice.",
            ));
        }
        return Ok(());
    }
    let config = match kind {
        Some("function" | "custom") => {
            let (name, _) = tools
                .declarations(Some(std::slice::from_ref(choice)))
                .next()
                .ok_or(TranslationError::InvalidRequest(
                    "The selected function must be declared.",
                ))?;
            Some(json!({"mode":"ANY","allowedFunctionNames":[name]}))
        }
        Some("required") => Some(json!({"mode":"ANY"})),
        Some("none") => Some(json!({"mode":"NONE"})),
        Some("auto") => Some(json!({"mode":"AUTO"})),
        _ => None,
    };
    if let Some(config) = config {
        result.insert("toolConfig".into(), json!({"functionCallingConfig":config}));
    }
    result.insert(
        "tools".into(),
        json!([{"functionDeclarations":declarations}]),
    );
    Ok(())
}
