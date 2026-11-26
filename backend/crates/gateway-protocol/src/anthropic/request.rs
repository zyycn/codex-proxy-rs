//! 校验原生 Messages 请求与计数合同

use super::MessageError;
use serde_json::{Map, Value};

/// 校验 Messages 生成请求的必需字段
pub fn validate_request(request: &Map<String, Value>) -> Result<(), MessageError> {
    validate_count_request(request)?;
    if request.get("max_tokens").and_then(Value::as_u64).is_none() {
        return Err(MessageError::InvalidRequest(
            "The max_tokens field must be a non-negative integer.",
        ));
    }
    Ok(())
}

/// 计数请求没有生成预算要求
pub fn validate_count_request(request: &Map<String, Value>) -> Result<(), MessageError> {
    if !request
        .get("model")
        .and_then(Value::as_str)
        .is_some_and(|v| !v.trim().is_empty())
    {
        return Err(MessageError::InvalidRequest(
            "The model field must be a non-empty string.",
        ));
    }
    let messages =
        request
            .get("messages")
            .and_then(Value::as_array)
            .ok_or(MessageError::InvalidRequest(
                "The messages field must be an array.",
            ))?;
    if messages.is_empty() {
        return Err(MessageError::InvalidRequest(
            "The messages field must not be empty.",
        ));
    }
    for message in messages {
        if !message
            .get("role")
            .and_then(Value::as_str)
            .is_some_and(|v| matches!(v, "user" | "assistant" | "system"))
        {
            return Err(MessageError::InvalidRequest(
                "Each message role must be user, assistant, or system.",
            ));
        }
        if message
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(|blocks| {
                blocks
                    .iter()
                    .any(|block| block.get("type").and_then(Value::as_str).is_none())
            })
        {
            return Err(MessageError::InvalidRequest(
                "Each content block must contain a type string.",
            ));
        }
        if !message
            .get("content")
            .is_some_and(|v| v.is_string() || v.is_array())
        {
            return Err(MessageError::InvalidRequest(
                "Each message content must be a string or an array.",
            ));
        }
    }
    if request.get("stream").is_some_and(|v| !v.is_boolean()) {
        return Err(MessageError::InvalidRequest(
            "The stream field must be a boolean.",
        ));
    }
    if request.get("tools").is_some_and(|v| !v.is_array()) {
        return Err(MessageError::InvalidRequest(
            "The tools field must be an array.",
        ));
    }
    Ok(())
}
