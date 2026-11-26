//! 提取 Messages 客户端显式设置的推理等级

use serde_json::{Map, Value};

/// 返回客户端显式设置的 effort，预算与默认档位由转换器单独处理
#[must_use]
pub fn messages_request_effort(request: &Map<String, Value>) -> Option<&str> {
    request
        .get("output_config")
        .and_then(|value| value.get("effort"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}
