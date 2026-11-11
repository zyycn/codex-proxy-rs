//! 从协议正文提取能力要求与请求观测事实

use serde_json::{Map, Value};

use crate::anthropic::{ANTHROPIC_PROTOCOL, messages_request_effort};
use crate::openai::{OPENAI_PROTOCOL, codex_responses_request_semantics_with_turn_metadata};

/// 与执行 Provider 无关的逻辑请求观测
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestObservation {
    /// 当前请求与已核实续接配置共同确定的推理强度
    pub reasoning_effort: Option<String>,
    /// 协议声明派生的推理预设
    pub reasoning_preset: Option<String>,
    /// 协议声明的请求类型，可信内部分类由调用方覆盖
    pub request_kind: Option<String>,
    /// 协议声明的子代理类型
    pub subagent_kind: Option<String>,
    /// 当前请求是否声明压缩
    pub compact: bool,
}

/// 旧配置只由状态所属 Provider 提供，是否继承仍由当前协议的续接语义决定
#[must_use]
pub fn request_observation(
    protocol: &str,
    body: &Map<String, Value>,
    context: &Map<String, Value>,
    previous_reasoning_effort_override: Option<&str>,
) -> RequestObservation {
    match protocol {
        OPENAI_PROTOCOL => {
            let semantics = codex_responses_request_semantics_with_turn_metadata(
                body,
                context.get("turn_metadata").and_then(Value::as_str),
                previous_reasoning_effort_override,
            );
            RequestObservation {
                reasoning_effort: semantics.reasoning_effort,
                reasoning_preset: semantics.reasoning_preset.map(str::to_owned),
                request_kind: semantics.request_kind,
                subagent_kind: semantics.subagent_kind,
                compact: semantics.compact,
            }
        }
        ANTHROPIC_PROTOCOL => RequestObservation {
            reasoning_effort: messages_request_effort(body).map(str::to_owned),
            ..RequestObservation::default()
        },
        _ => RequestObservation::default(),
    }
}

/// 请求正文表达的能力要求
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RequestFacts {
    /// 显式输出预算
    pub max_output_tokens: Option<u64>,
    /// 工具声明
    pub tools: bool,
    /// 图片输入
    pub vision: bool,
    /// 图片生成工具声明
    pub image_generation: bool,
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
        OPENAI_PROTOCOL => {
            facts.max_output_tokens = body
                .get("max_output_tokens")
                .and_then(Value::as_u64)
                .filter(|n| *n > 0);
            facts.vision = contains_type(body.get("input"), "input_image");
            facts.image_generation = requests_image_generation(body);
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
        ANTHROPIC_PROTOCOL => {
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

/// 原生工具声明授权上游生成图片，Codex 的被动 namespace 与图片输入不属于该权限
fn requests_image_generation(body: &Map<String, Value>) -> bool {
    contains_image_tool(body.get("tools"))
        || body
            .get("input")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item.get("type").and_then(Value::as_str) == Some("additional_tools")
                        && contains_image_tool(item.get("tools"))
                })
            })
        || body.get("tool_choice").is_some_and(|choice| {
            is_image_tool(choice)
                || (choice.get("type").and_then(Value::as_str) == Some("allowed_tools")
                    && contains_image_tool(choice.get("tools")))
        })
}

fn contains_image_tool(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_array)
        .is_some_and(|tools| tools.iter().any(is_image_tool))
}

fn is_image_tool(value: &Value) -> bool {
    value.get("type").and_then(Value::as_str) == Some("image_generation")
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
