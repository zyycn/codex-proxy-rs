//! GenAI 输入计数的原生请求封装

use serde_json::{Map, Value};
use thiserror::Error;

/// 原生计数封套的解析错误
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("generateContentRequest must be a JSON object.")]
pub struct CountTokensError;

fn wrapped_field(request: &Map<String, Value>) -> Option<&'static str> {
    ["generateContentRequest", "generate_content_request"]
        .into_iter()
        .find(|key| request.contains_key(*key))
}

/// 完整生成请求存在时，以它作为计数输入，外层 contents 不参与计数
pub fn count_tokens_request(
    request: &Map<String, Value>,
) -> Result<&Map<String, Value>, CountTokensError> {
    match wrapped_field(request) {
        Some(key) => request[key].as_object().ok_or(CountTokensError),
        None => Ok(request),
    }
}

/// 取出计数正文供上游适配，无需复制历史和工具
pub fn into_count_tokens_request(
    mut request: Map<String, Value>,
) -> Result<Map<String, Value>, CountTokensError> {
    match wrapped_field(&request) {
        Some(key) => match request.remove(key) {
            Some(Value::Object(body)) => Ok(body),
            _ => Err(CountTokensError),
        },
        None => Ok(request),
    }
}
