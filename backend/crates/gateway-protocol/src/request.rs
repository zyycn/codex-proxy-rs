//! 从协议正文提取调度需要的纯请求事实

use serde_json::{Map, Value};

/// 请求正文表达的能力要求
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RequestFacts {
    /// 显式输出预算
    pub max_output_tokens: Option<u64>,
    /// 工具声明
    pub tools: bool,
    /// 图片输入
    pub vision: bool,
    /// 推理配置
    pub reasoning: bool,
    /// JSON Schema 输出
    pub json_schema: bool,
    /// 原生服务端续接
    pub native_continuation: bool,
}

/// 按协议提取能力事实，未知协议保留空事实供其 owner 补齐
#[must_use]
pub fn request_facts(protocol: &str, body: &Map<String, Value>) -> RequestFacts {
    let mut facts = RequestFacts {
        tools: body
            .get("tools")
            .and_then(Value::as_array)
            .is_some_and(|v| !v.is_empty()),
        ..RequestFacts::default()
    };
    match protocol {
        "openai" => {
            facts.max_output_tokens = body
                .get("max_output_tokens")
                .and_then(Value::as_u64)
                .filter(|n| *n > 0);
            facts.vision = contains_type(body.get("input"), "input_image");
            facts.reasoning = body.get("reasoning").is_some_and(Value::is_object);
            facts.json_schema = body
                .get("text")
                .and_then(|v| v.get("format"))
                .and_then(|v| v.get("type"))
                .and_then(Value::as_str)
                == Some("json_schema");
            facts.native_continuation = body
                .get("previous_response_id")
                .and_then(Value::as_str)
                .is_some();
        }
        "anthropic" => {
            facts.max_output_tokens = body.get("max_tokens").and_then(Value::as_u64);
            facts.vision = contains_type(body.get("messages"), "image");
            facts.reasoning = body
                .get("thinking")
                .and_then(|v| v.get("type"))
                .and_then(Value::as_str)
                .is_some_and(|v| matches!(v, "enabled" | "adaptive"));
            facts.json_schema = body
                .get("output_config")
                .and_then(|v| v.get("format"))
                .and_then(|v| v.get("type"))
                .and_then(Value::as_str)
                == Some("json_schema");
        }
        _ => {}
    }
    facts
}

fn contains_type(value: Option<&Value>, expected: &str) -> bool {
    match value {
        Some(Value::Array(values)) => values.iter().any(|v| contains_type(Some(v), expected)),
        Some(Value::Object(object)) => {
            object.get("type").and_then(Value::as_str) == Some(expected)
                || object.values().any(|v| contains_type(Some(v), expected))
        }
        _ => false,
    }
}
