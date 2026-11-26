//! Responses 复用内容映射，生成参数直接转入 GenAI，避免继承 Messages 的默认值与限制

use crate::anthropic::{TranslationError, responses_to_messages_content};
use serde_json::{Map, Value, json};

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
    let content = responses_to_messages_content(request, false)?;
    let mut result = super::messages::content_to_genai(&content)?;
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
