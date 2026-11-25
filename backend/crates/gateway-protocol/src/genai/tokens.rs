//! GenAI 输入计数的请求封装和跨协议结果投影

use crate::anthropic::TranslationError;
use serde_json::{Map, Value, json};

/// 完整生成请求存在时，以它作为计数输入，外层 contents 不参与计数
pub fn count_tokens_request(
    request: &Map<String, Value>,
) -> Result<&Map<String, Value>, TranslationError> {
    match request
        .get("generateContentRequest")
        .or_else(|| request.get("generate_content_request"))
    {
        Some(value) => value.as_object().ok_or(TranslationError::InvalidRequest(
            "generateContentRequest must be a JSON object.",
        )),
        None => Ok(request),
    }
}

/// 只投影上游明确给出的计数，不把缺失或无效结果当作零
pub fn count_tokens_to_messages(response: &Value) -> Result<Value, TranslationError> {
    let tokens = response.get("totalTokens").and_then(Value::as_u64).ok_or(
        TranslationError::InvalidEvent("The upstream token count is invalid."),
    )?;
    Ok(json!({"input_tokens":tokens}))
}
