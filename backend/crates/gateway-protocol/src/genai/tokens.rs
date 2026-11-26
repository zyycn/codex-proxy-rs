//! GenAI 输入计数的原生请求封装

use serde_json::{Map, Value};
use thiserror::Error;

/// 原生计数封套的解析错误
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("generateContentRequest must be a JSON object.")]
pub struct CountTokensError;

/// 完整生成请求存在时，以它作为计数输入，外层 contents 不参与计数
pub fn count_tokens_request(
    request: &Map<String, Value>,
) -> Result<&Map<String, Value>, CountTokensError> {
    match request
        .get("generateContentRequest")
        .or_else(|| request.get("generate_content_request"))
    {
        Some(value) => value.as_object().ok_or(CountTokensError),
        None => Ok(request),
    }
}
