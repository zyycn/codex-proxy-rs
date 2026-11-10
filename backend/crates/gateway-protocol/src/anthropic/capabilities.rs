//! 所选 Messages 模型的明确能力与限制，不根据模型名称推断

use serde_json::Value;

/// 转换器消费的模型目录事实；None 表示上游未声明
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MessagesModelCapabilities {
    pub thinking: Option<bool>,
    pub adaptive_thinking: Option<bool>,
    pub manual_thinking: Option<bool>,
    pub disabled_thinking: Option<bool>,
    pub efforts: Option<Vec<String>>,
    pub max_tokens: Option<u64>,
}

impl MessagesModelCapabilities {
    /// 从 Anthropic Models 条目提取转换所需事实，未知扩展不影响解析
    pub fn from_model(model: &Value) -> Self {
        Self {
            thinking: model
                .pointer("/capabilities/thinking/supported")
                .and_then(Value::as_bool),
            adaptive_thinking: model
                .pointer("/capabilities/thinking/types/adaptive/supported")
                .and_then(Value::as_bool),
            manual_thinking: model
                .pointer("/capabilities/thinking/types/enabled/supported")
                .and_then(Value::as_bool),
            disabled_thinking: model
                .pointer("/capabilities/thinking/types/disabled/supported")
                .and_then(Value::as_bool),
            efforts: model
                .pointer("/capabilities/effort")
                .and_then(Value::as_object)
                .map(|efforts| {
                    ["low", "medium", "high", "xhigh", "max"]
                        .into_iter()
                        .filter(|level| {
                            efforts
                                .get(*level)
                                .and_then(|v| v.get("supported"))
                                .and_then(Value::as_bool)
                                == Some(true)
                        })
                        .map(str::to_owned)
                        .collect()
                }),
            max_tokens: model
                .get("max_tokens")
                .and_then(Value::as_u64)
                .filter(|v| *v > 0),
        }
    }
}
