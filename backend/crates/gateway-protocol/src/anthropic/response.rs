//! Messages 与 Responses 完整响应的映射

use serde_json::{Map, Value, json};

use super::{MessageUsage, TranslationError, content, tools::ToolSet, usage};

/// 将完整 Messages JSON 转换为单个 Responses 终态事件
pub fn messages_to_responses_event(
    message: &Value,
    original_request: &Map<String, Value>,
) -> Result<Value, TranslationError> {
    messages_to_responses_response(message, original_request).map(terminal_response_event)
}

pub(super) fn terminal_response_event(response: Value) -> Value {
    let event_type = match response.get("status").and_then(Value::as_str) {
        Some("incomplete") => "response.incomplete",
        Some("failed") => "response.failed",
        _ => "response.completed",
    };
    json!({"type":event_type,"response":response})
}

/// Messages 完整响应转为 Responses，原始请求用于 custom 工具与模型身份恢复
pub fn messages_to_responses_response(
    message: &Value,
    original_request: &Map<String, Value>,
) -> Result<Value, TranslationError> {
    complete_response(
        message,
        original_request.get("model"),
        &ToolSet::identities(original_request),
    )
}

pub(super) fn complete_response(
    message: &Value,
    model: Option<&Value>,
    tools: &ToolSet,
) -> Result<Value, TranslationError> {
    if message.get("type").and_then(Value::as_str) == Some("error") {
        return Ok(response_from_message(message, model, Vec::new()));
    }
    let blocks =
        message
            .get("content")
            .and_then(Value::as_array)
            .ok_or(TranslationError::InvalidEvent(
                "The Messages response content must be an array.",
            ))?;
    let id = message
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("resp_messages");
    let output = blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| {
            messages_block_to_response(block, tools, &format!("{id}_{index}"))
        })
        .collect::<Vec<_>>();
    Ok(response_from_message(message, model, output))
}

pub(super) fn response_from_message(
    message: &Value,
    model: Option<&Value>,
    output: Vec<Value>,
) -> Value {
    if message.get("type").and_then(Value::as_str) == Some("error") {
        return json!({"object":"response","status":"failed","error":messages_error_to_responses(message.get("error").unwrap_or(&Value::Null)),"output":[]});
    }
    let id = message
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("resp_messages");
    let reason = message
        .get("stop_reason")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let incomplete = match reason {
        "max_tokens" | "model_context_window_exceeded" => Some("max_output_tokens"),
        "refusal" => Some("content_filter"),
        _ => None,
    };
    let mut response = json!({"id":id,"object":"response","created_at":0,"status":if incomplete.is_some(){"incomplete"}else{"completed"},"model":model.or_else(||message.get("model")),"output":output,"error":null,"incomplete_details":incomplete.map(|reason|json!({"reason":reason})),"usage":MessageUsage::from_value(message.get("usage").unwrap_or(&Value::Null)).responses()});
    if reason == "pause_turn" {
        response["end_turn"] = json!(false);
    }
    response
}

pub(super) fn messages_block_to_response(
    block: &Value,
    tools: &ToolSet,
    id: &str,
) -> Option<Value> {
    match block.get("type").and_then(Value::as_str)? {
        "text" => Some(
            json!({"id":id,"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":block.get("text").and_then(Value::as_str).unwrap_or_default(),"annotations":block.get("citations").cloned().unwrap_or_else(||json!([]))}]}),
        ),
        "tool_use" => Some(tools.call(block, id)),
        "thinking" | "redacted_thinking" => Some(
            json!({"id":id,"type":"reasoning","summary":if block.get("type").and_then(Value::as_str)==Some("thinking") {vec![json!({"type":"summary_text","text":block.get("thinking").and_then(Value::as_str).unwrap_or_default()})]}else{vec![]},"encrypted_content":content::anthropic_carrier(block)}),
        ),
        // 服务端工具块作为有来源的不透明项保留，可随续接请求恢复
        "server_tool_use"
        | "web_search_tool_result"
        | "web_fetch_tool_result"
        | "code_execution_tool_result"
        | "bash_code_execution_tool_result"
        | "text_editor_code_execution_tool_result"
        | "tool_search_tool_result" => Some(
            json!({"id":id,"type":"reasoning","summary":[],"encrypted_content":content::anthropic_carrier(block)}),
        ),
        _ => None,
    }
}

/// Responses 完整响应转为 Messages，推理载荷保留其来源供后续轮次回传
pub fn responses_to_messages_response(
    response: &Value,
    original_request: &Map<String, Value>,
) -> Result<Value, TranslationError> {
    complete_message(response, original_request.get("model"))
}

pub(super) fn complete_message(
    response: &Value,
    model: Option<&Value>,
) -> Result<Value, TranslationError> {
    if response.get("error").is_some_and(|v| !v.is_null())
        || response.get("status").and_then(Value::as_str) == Some("failed")
    {
        return Ok(
            json!({"type":"error","error":responses_error_to_messages(response.get("error").unwrap_or(&Value::Null))}),
        );
    }
    let output =
        response
            .get("output")
            .and_then(Value::as_array)
            .ok_or(TranslationError::InvalidEvent(
                "The Responses output must be an array.",
            ))?;
    let content = output
        .iter()
        .flat_map(responses_item_to_messages)
        .collect::<Vec<_>>();
    let has_tool = content
        .iter()
        .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"));
    let stop = responses_stop_reason(response, has_tool);
    Ok(
        json!({"id":response.get("id").and_then(Value::as_str).unwrap_or("msg_responses"),"type":"message","role":"assistant","model":model.or_else(||response.get("model")),"content":content,"stop_reason":stop,"stop_sequence":null,"usage":usage::responses_usage(response.get("usage").unwrap_or(&Value::Null))}),
    )
}

pub(super) fn responses_stop_reason(response: &Value, has_tool: bool) -> &'static str {
    if response.get("status").and_then(Value::as_str) == Some("incomplete") {
        match response
            .pointer("/incomplete_details/reason")
            .and_then(Value::as_str)
        {
            Some("content_filter") => "refusal",
            _ => "max_tokens",
        }
    } else if response.get("end_turn").and_then(Value::as_bool) == Some(false) {
        "pause_turn"
    } else if has_tool {
        "tool_use"
    } else {
        "end_turn"
    }
}

pub(super) fn responses_error_to_messages(error: &Value) -> Value {
    let source = error
        .get("type")
        .or_else(|| error.get("code"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let kind = match source {
        "invalid_request_error" | "invalid_prompt" => "invalid_request_error",
        "authentication_error" => "authentication_error",
        "permission_error" => "permission_error",
        "not_found_error" => "not_found_error",
        "rate_limit_error" | "rate_limit_exceeded" => "rate_limit_error",
        "overloaded_error" => "overloaded_error",
        _ => "api_error",
    };
    json!({"type":kind,"message":error.get("message").and_then(Value::as_str).unwrap_or("The upstream response failed.")})
}

fn messages_error_to_responses(error: &Value) -> Value {
    let code = match error
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "rate_limit_error" => "rate_limit_exceeded",
        "invalid_request_error" => "invalid_prompt",
        _ => "server_error",
    };
    json!({"code":code,"message":error.get("message").and_then(Value::as_str).unwrap_or("The upstream response failed.")})
}

pub(super) fn responses_item_to_messages(item: &Value) -> Vec<Value> {
    match item.get("type").and_then(Value::as_str).unwrap_or_default() {
        "message" => item
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(content::to_messages_content)
            .collect(),
        "function_call" => vec![
            json!({"type":"tool_use","id":item.get("call_id").or_else(||item.get("id")),"name":item.get("name"),"input":item.get("arguments").and_then(Value::as_str).and_then(|v|serde_json::from_str::<Value>(v).ok()).filter(Value::is_object).unwrap_or_else(||json!({}))}),
        ],
        "custom_tool_call" => vec![
            json!({"type":"tool_use","id":item.get("call_id").or_else(||item.get("id")),"name":item.get("name"),"input":{"input":item.get("input").and_then(Value::as_str).unwrap_or_default()}}),
        ],
        "reasoning" => vec![
            json!({"type":"thinking","thinking":content::reasoning_text(item),"signature":content::openai_carrier(item)}),
        ],
        _ => Vec::new(),
    }
}
