use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    codex::protocol::error::{AppError, AppResult},
    codex::transport::types::CodexResponsesRequest,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    #[serde(default)]
    pub stream: bool,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub service_tier: Option<String>,
    #[serde(default)]
    pub tools: Option<Vec<Value>>,
    #[serde(default)]
    pub tool_choice: Option<Value>,
    #[serde(default)]
    pub parallel_tool_calls: Option<bool>,
    #[serde(default)]
    pub functions: Option<Vec<Value>>,
    #[serde(default)]
    pub response_format: Option<Value>,
    #[serde(default)]
    pub user: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default)]
    pub content: Option<Value>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<Value>>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub function_call: Option<Value>,
}

pub fn translate_chat_to_codex(req: ChatCompletionRequest) -> AppResult<CodexResponsesRequest> {
    if req.messages.is_empty() {
        return Err(AppError::BadRequest(
            "messages must not be empty".to_string(),
        ));
    }

    let instructions = chat_instructions(&req.messages);
    let mut input = chat_input(req.messages);
    if input.is_empty() {
        input.push(json!({"role": "user", "content": ""}));
    }

    let mut request = CodexResponsesRequest::new_http_sse(req.model, instructions, input);
    request.tools = codex_tools(req.tools, req.functions);
    request.tool_choice = req.tool_choice;
    request.parallel_tool_calls = req.parallel_tool_calls;
    request.text = response_format_text(req.response_format);
    request.service_tier = req.service_tier;
    if let Some(effort) = req.reasoning_effort {
        request.reasoning = Some(json!({"effort": effort, "summary": "auto"}));
    }
    Ok(request)
}

fn chat_instructions(messages: &[ChatMessage]) -> String {
    let instructions = messages
        .iter()
        .filter(|message| message.role == "system" || message.role == "developer")
        .map(|message| extract_text(&message.content))
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    if instructions.is_empty() {
        "You are a helpful assistant.".to_string()
    } else {
        instructions
    }
}

fn chat_input(messages: Vec<ChatMessage>) -> Vec<Value> {
    let mut input = Vec::new();
    for message in messages {
        match message.role.as_str() {
            "system" | "developer" => {}
            "assistant" => push_assistant_message(&mut input, message),
            "tool" => input.push(json!({
                "type": "function_call_output",
                "call_id": message.tool_call_id.unwrap_or_else(|| "unknown".to_string()),
                "output": extract_text(&message.content),
            })),
            "function" => input.push(json!({
                "type": "function_call_output",
                "call_id": format!("fc_{}", message.name.unwrap_or_else(|| "unknown".to_string())),
                "output": extract_text(&message.content),
            })),
            _ => input.push(json!({
                "role": "user",
                "content": extract_content(&message.content),
            })),
        }
    }
    input
}

fn push_assistant_message(input: &mut Vec<Value>, message: ChatMessage) {
    let text = extract_text(&message.content);
    let has_tool_calls = message
        .tool_calls
        .as_ref()
        .is_some_and(|calls| !calls.is_empty());
    if !text.is_empty() || (!has_tool_calls && message.function_call.is_none()) {
        input.push(json!({"role": "assistant", "content": text}));
    }

    if let Some(tool_calls) = message.tool_calls {
        for tool_call in tool_calls {
            let call_id = tool_call
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let function = tool_call.get("function").unwrap_or(&Value::Null);
            let name = function
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let arguments = function
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or("");
            input.push(json!({
                "type": "function_call",
                "call_id": call_id,
                "name": name,
                "arguments": arguments,
            }));
        }
    }

    if let Some(function_call) = message.function_call {
        let name = function_call
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let arguments = function_call
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or("");
        input.push(json!({
            "type": "function_call",
            "call_id": format!("fc_{name}"),
            "name": name,
            "arguments": arguments,
        }));
    }
}

fn extract_text(content: &Option<Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    let Some(parts) = content.as_array() else {
        return String::new();
    };
    parts
        .iter()
        .filter_map(|part| {
            (part.get("type").and_then(Value::as_str) == Some("text"))
                .then(|| part.get("text").and_then(Value::as_str))
                .flatten()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn extract_content(content: &Option<Value>) -> Value {
    let Some(content) = content else {
        return Value::String(String::new());
    };
    if content.as_str().is_some() {
        return content.clone();
    }
    let Some(parts) = content.as_array() else {
        return Value::String(String::new());
    };
    let has_image = parts
        .iter()
        .any(|part| part.get("type").and_then(Value::as_str) == Some("image_url"));
    if !has_image {
        return Value::String(extract_text(&Some(content.clone())));
    }
    let codex_parts = parts
        .iter()
        .filter_map(codex_content_part)
        .collect::<Vec<_>>();
    if codex_parts.is_empty() {
        Value::String(String::new())
    } else {
        Value::Array(codex_parts)
    }
}

fn codex_content_part(part: &Value) -> Option<Value> {
    match part.get("type").and_then(Value::as_str)? {
        "text" => part
            .get("text")
            .and_then(Value::as_str)
            .map(|text| json!({"type": "input_text", "text": text})),
        "image_url" => image_url(part).map(|url| json!({"type": "input_image", "image_url": url})),
        _ => None,
    }
}

fn image_url(part: &Value) -> Option<&str> {
    let image = part.get("image_url")?;
    image
        .as_str()
        .or_else(|| image.get("url").and_then(Value::as_str))
}

fn codex_tools(tools: Option<Vec<Value>>, functions: Option<Vec<Value>>) -> Option<Vec<Value>> {
    if let Some(tools) = tools.filter(|tools| !tools.is_empty()) {
        return Some(tools);
    }
    functions
        .filter(|functions| !functions.is_empty())
        .map(|functions| {
            functions
                .into_iter()
                .map(|function| json!({"type": "function", "function": function}))
                .collect()
        })
}

fn response_format_text(response_format: Option<Value>) -> Option<Value> {
    let format = response_format?;
    let kind = format.get("type").and_then(Value::as_str)?;
    match kind {
        "json_object" => Some(json!({"format": {"type": "json_object"}})),
        "json_schema" => {
            let schema = format.get("json_schema")?;
            let name = schema
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("response");
            let mut codex_format = json!({
                "type": "json_schema",
                "name": name,
                "schema": schema.get("schema").cloned().unwrap_or_else(|| json!({})),
            });
            if let Some(strict) = schema.get("strict").and_then(Value::as_bool) {
                codex_format["strict"] = Value::Bool(strict);
            }
            Some(json!({"format": codex_format}))
        }
        _ => None,
    }
}
