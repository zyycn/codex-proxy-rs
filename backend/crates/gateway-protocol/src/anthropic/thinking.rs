//! 按目标模型能力映射推理等级与预算；未知兼容上游沿用 CPA 手动预算

use serde_json::{Map, Value, json};

use super::MessagesModelCapabilities;

pub(super) fn to_responses(request: &Map<String, Value>) -> Value {
    let configured = request
        .get("output_config")
        .and_then(|value| value.get("effort"))
        .and_then(Value::as_str)
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty());
    let thinking = request.get("thinking");
    let effort = match thinking
        .and_then(|value| value.get("type"))
        .and_then(Value::as_str)
    {
        Some("disabled") => "none".to_owned(),
        Some("enabled") => {
            let budget = thinking
                .and_then(|value| value.get("budget_tokens"))
                .and_then(Value::as_i64);
            match budget {
                Some(-1) => "auto",
                Some(0) => "none",
                Some(1..=512) => "minimal",
                Some(513..=1024) => "low",
                Some(1025..=8192) => "medium",
                Some(8193..=24576) => "high",
                Some(24577..) => "xhigh",
                _ => "medium",
            }
            .to_owned()
        }
        Some("adaptive" | "auto") => configured.unwrap_or_else(|| "xhigh".to_owned()),
        _ => configured.unwrap_or_else(|| "medium".to_owned()),
    };
    json!({"effort":effort})
}

pub(super) fn apply_to_messages(
    request: &Map<String, Value>,
    target: &mut Map<String, Value>,
    capabilities: &MessagesModelCapabilities,
    maximum: u64,
) {
    let effort = request
        .get("reasoning")
        .and_then(|v| v.get("effort"))
        .and_then(Value::as_str)
        .map(|v| v.trim().to_ascii_lowercase());
    let forced = matches!(
        target
            .get("tool_choice")
            .and_then(|v| v.get("type"))
            .and_then(Value::as_str),
        Some("any" | "tool")
    );
    let adaptive = capabilities.adaptive_thinking == Some(true);
    if forced && adaptive {
        // 新版 adaptive 模型可能不支持强制工具；沿 CPA 降级为 auto，保留并行限制与推理
        if let Some(choice) = target.get_mut("tool_choice").and_then(Value::as_object_mut) {
            choice.insert("type".into(), json!("auto"));
            choice.shift_remove("name");
        }
    } else if forced || (effort.is_none() && continues_without_thinking(target)) {
        // 默认开启推理的上游要求工具结果回传思考块；无思考的工具轮次保持关闭，直到用户开始新轮次
        if capabilities.thinking != Some(false) && capabilities.disabled_thinking != Some(false) {
            target.insert("thinking".into(), json!({"type":"disabled"}));
        }
        return;
    }
    let Some(effort) = effort else {
        return;
    };
    if capabilities.thinking == Some(false) {
        return;
    }
    if effort == "none" && capabilities.disabled_thinking != Some(false) {
        target.insert("thinking".into(), json!({"type":"disabled"}));
        return;
    }
    if adaptive {
        target.insert("thinking".into(), json!({"type":"adaptive"}));
        if let Some(effort) = adaptive_effort(&effort, capabilities.efforts.as_deref()) {
            target.entry("output_config").or_insert_with(|| json!({}))["effort"] = json!(effort);
        }
        return;
    }
    if capabilities.manual_thinking == Some(false) {
        return;
    }
    let requested: u64 = match effort.as_str() {
        "minimal" => 512,
        "low" => 1024,
        "medium" => 8192,
        "high" => 24576,
        "xhigh" => 32768,
        "max" => 128000,
        "auto" => 8192,
        _ => return,
    };
    // 手动预算至少 1024 且小于输出上限，adaptive 不受此预算下限约束
    let budget = requested.max(1024).min(maximum.saturating_sub(1));
    if budget >= 1024 {
        target.insert(
            "thinking".into(),
            json!({"type":"enabled","budget_tokens":budget}),
        );
    } else if capabilities.disabled_thinking != Some(false) {
        target.insert("thinking".into(), json!({"type":"disabled"}));
    }
}

fn continues_without_thinking(target: &Map<String, Value>) -> bool {
    let Some([.., assistant, user]) = target
        .get("messages")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
    else {
        return false;
    };
    target.contains_key("tools")
        && user["role"] == "user"
        && user["content"].as_array().is_some_and(|blocks| {
            !blocks.is_empty() && blocks.iter().all(|block| block["type"] == "tool_result")
        })
        && assistant["role"] == "assistant"
        && assistant["content"].as_array().is_some_and(|blocks| {
            blocks.iter().any(|block| block["type"] == "tool_use")
                && !blocks.iter().any(|block| {
                    matches!(
                        block["type"].as_str(),
                        Some("thinking" | "redacted_thinking")
                    )
                })
        })
}

fn adaptive_effort<'a>(requested: &str, supported: Option<&'a [String]>) -> Option<&'a str> {
    const LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
    let requested = match requested {
        "none" | "minimal" => "low",
        "auto" => return None,
        other => other,
    };
    let rank = LEVELS.iter().position(|level| *level == requested)?;
    let Some(supported) = supported else {
        return Some(LEVELS[rank]);
    };
    // 目录明确列出的等级优先，超出范围收敛到最近的较低档，全部更高时取最低可用档
    LEVELS[..=rank]
        .iter()
        .rev()
        .chain(LEVELS[rank + 1..].iter())
        .find_map(|level| {
            supported
                .iter()
                .find(|value| value.as_str() == *level)
                .map(String::as_str)
        })
}
