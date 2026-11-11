//! 解析 Codex 请求的会话身份、轮次元数据与多代理模式语义

use serde_json::{Map, Value};

const MULTI_AGENT_MODE_OPEN_TAG: &str = "<multi_agent_mode>";
const MULTI_AGENT_MODE_CLOSE_TAG: &str = "</multi_agent_mode>";
const PROACTIVE_MULTI_AGENT_MODE_PREFIX: &str = "Proactive multi-agent delegation is active.";

/// 各 Codex 端点共用的显式根会话身份
/// 连接边界的 session_id 优先，
/// 正文及官方 metadata 只做回退；不从 turn、window 或请求内容猜测归属
#[must_use]
pub fn codex_session_id(body: &Map<String, Value>, context: &Map<String, Value>) -> Option<String> {
    codex_identity_field(body, context, "session_id")
}

/// 账号绑定使用逻辑会话身份；Responses 的 session-id 可能只是缓存路由键
/// WebSocket 每帧元数据优先于连接上下文，缺少逻辑元数据时才回退到显式 session_id
#[must_use]
pub fn codex_account_session_id(
    body: &Map<String, Value>,
    context: &Map<String, Value>,
) -> Option<String> {
    client_metadata_string(body, "session_id")
        .map(str::to_owned)
        .or_else(|| request_turn_metadata(body).and_then(turn_metadata_session_id))
        .or_else(|| {
            non_empty_string(context.get("turn_metadata")).and_then(turn_metadata_session_id)
        })
        .or_else(|| non_empty_string(body.get("session_id")).map(str::to_owned))
        .or_else(|| codex_session_id(body, context))
}

/// 每条请求的逻辑线程身份，与缓存路由头分开解析
#[must_use]
pub fn codex_account_thread_id(
    body: &Map<String, Value>,
    context: &Map<String, Value>,
) -> Option<String> {
    client_metadata_string(body, "thread_id")
        .map(str::to_owned)
        .or_else(|| request_turn_metadata(body).and_then(turn_metadata_thread_id))
        .or_else(|| {
            non_empty_string(context.get("turn_metadata")).and_then(turn_metadata_thread_id)
        })
        .or_else(|| codex_thread_id(body, context))
}

#[must_use]
pub fn turn_metadata_thread_id(metadata: &str) -> Option<String> {
    let metadata: Value = serde_json::from_str(metadata).ok()?;
    non_empty_string(metadata.get("thread_id")).map(str::to_owned)
}

#[must_use]
pub fn turn_metadata_session_id(metadata: &str) -> Option<String> {
    let metadata: Value = serde_json::from_str(metadata).ok()?;
    non_empty_string(metadata.get("session_id")).map(str::to_owned)
}

/// 官方工具调用的 turn_id，同时用于关联独立 Images 请求
#[must_use]
pub fn codex_turn_id(body: &Map<String, Value>, context: &Map<String, Value>) -> Option<String> {
    client_metadata_string(body, "turn_id")
        .map(str::to_owned)
        .or_else(|| request_turn_metadata(body).and_then(turn_metadata_turn_id))
        .or_else(|| non_empty_string(context.get("turn_metadata")).and_then(turn_metadata_turn_id))
        .or_else(|| codex_identity_field(body, context, "turn_id"))
}

#[must_use]
pub fn turn_metadata_turn_id(metadata: &str) -> Option<String> {
    let metadata: Value = serde_json::from_str(metadata).ok()?;
    non_empty_string(metadata.get("turn_id")).map(str::to_owned)
}

/// 各 Codex 端点共用的显式线程身份，与根会话采用相同的来源优先级
#[must_use]
pub fn codex_thread_id(body: &Map<String, Value>, context: &Map<String, Value>) -> Option<String> {
    codex_identity_field(body, context, "thread_id")
}

fn codex_identity_field(
    body: &Map<String, Value>,
    context: &Map<String, Value>,
    field: &str,
) -> Option<String> {
    non_empty_string(context.get(field))
        .or_else(|| non_empty_string(body.get(field)))
        .or_else(|| client_metadata_string(body, field))
        .map(str::to_owned)
        .or_else(|| {
            let metadata = non_empty_string(context.get("turn_metadata"))
                .or_else(|| request_turn_metadata(body))?;
            let metadata: Value = serde_json::from_str(metadata).ok()?;
            non_empty_string(metadata.get(field)).map(str::to_owned)
        })
        .or_else(|| {
            body.get("context")
                .and_then(|context| non_empty_string(context.get(field)))
                .map(str::to_owned)
        })
}

/// 从 OpenAI Responses 请求中提取的稳定 Codex 请求语义
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexResponsesRequestSemantics {
    /// 按最新配置更新与请求基线解析的推理强度
    pub reasoning_effort: Option<String>,
    /// 对话内持续生效的推理强度更新，供原生续接继承
    pub reasoning_effort_override: Option<String>,
    /// Codex turn metadata 中的请求类型
    pub request_kind: Option<String>,
    /// Codex turn metadata 中的子代理类型
    pub subagent_kind: Option<String>,
    /// 网关侧派生出的推理预设
    pub reasoning_preset: Option<&'static str>,
    /// 请求是否为 Codex 压缩请求
    pub compact: bool,
}

/// 从 Responses body 与非 wire 协议上下文中提取 Codex 语义
#[must_use]
pub fn codex_responses_request_semantics(
    body: &Map<String, Value>,
    context: &Map<String, Value>,
) -> CodexResponsesRequestSemantics {
    codex_responses_request_semantics_with_turn_metadata(
        body,
        context.get("turn_metadata").and_then(Value::as_str),
        None,
    )
}

/// 结合权威 turn metadata 与原生续接配置，从 Responses body 中提取 Codex 语义
#[must_use]
pub fn codex_responses_request_semantics_with_turn_metadata(
    body: &Map<String, Value>,
    turn_metadata: Option<&str>,
    previous_reasoning_effort_override: Option<&str>,
) -> CodexResponsesRequestSemantics {
    // 配置更新不改变顶层基线，只有 previous_response_id 续接才继承上一响应的更新
    let reasoning_effort_override = input_items(body)
        .iter()
        .rev()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("configuration_update"))
        .map(|item| reasoning_effort(item.get("reasoning")))
        .unwrap_or_else(|| {
            previous_reasoning_effort_override
                .filter(|_| non_empty_string(body.get("previous_response_id")).is_some())
                .map(ToOwned::to_owned)
        });
    let reasoning_effort = reasoning_effort_override
        .clone()
        .or_else(|| reasoning_effort(body.get("reasoning")));
    // 权威上下文与显式 turnMetadata 即使为空也不退回其他别名
    let parsed_turn_metadata = turn_metadata
        .or_else(|| body.get("turnMetadata").and_then(Value::as_str))
        .or_else(|| request_turn_metadata(body))
        .and_then(|value| serde_json::from_str::<Value>(value).ok());
    let mut request_kind = parsed_turn_metadata
        .as_ref()
        .and_then(|metadata| non_empty_string(metadata.get("request_kind")))
        .map(ToOwned::to_owned);
    let subagent_kind = parsed_turn_metadata
        .as_ref()
        .and_then(|metadata| non_empty_string(metadata.get("subagent_kind")))
        .map(ToOwned::to_owned);
    let proactive_multi_agent = latest_multi_agent_mode(input_items(body).iter())
        .is_some_and(|mode| mode.starts_with(PROACTIVE_MULTI_AGENT_MODE_PREFIX));
    let compact_trigger = input_items(body)
        .iter()
        .any(|item| non_empty_string(item.get("type")) == Some("compaction_trigger"));
    let reasoning_preset = (subagent_kind.is_none()
        && reasoning_effort.as_deref() == Some("max")
        && proactive_multi_agent)
        .then_some("ultra");
    let compact = request_kind.as_deref() == Some("compaction") || compact_trigger;
    // generate=false 才是预热，客户端分类提示不能把真实推理排除在用量统计外
    if body.get("generate").and_then(Value::as_bool) == Some(false) {
        request_kind = Some("prewarm".to_owned());
    } else if request_kind.as_deref() == Some("prewarm") {
        request_kind = None;
    }

    CodexResponsesRequestSemantics {
        reasoning_effort,
        reasoning_effort_override,
        request_kind,
        subagent_kind,
        reasoning_preset,
        compact,
    }
}

fn reasoning_effort(reasoning: Option<&Value>) -> Option<String> {
    let effort = reasoning?.get("effort")?;
    non_empty_string(Some(effort))
        .map(ToOwned::to_owned)
        .or_else(|| effort.as_u64().map(|value| value.to_string()))
}

fn request_turn_metadata(body: &Map<String, Value>) -> Option<&str> {
    non_empty_string(body.get("turnMetadata"))
        .or_else(|| non_empty_string(body.get("turn_metadata")))
        .or_else(|| non_empty_string(body.get("x-codex-turn-metadata")))
        .or_else(|| client_metadata_string(body, "x-codex-turn-metadata"))
        .or_else(|| client_metadata_string(body, "turnMetadata"))
        .or_else(|| client_metadata_string(body, "turn_metadata"))
}

fn client_metadata_string<'a>(body: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    body.get("client_metadata")
        .and_then(Value::as_object)
        .and_then(|metadata| non_empty_string(metadata.get(key)))
}

fn input_items(body: &Map<String, Value>) -> &[Value] {
    body.get("input")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn latest_multi_agent_mode<'a>(
    input: impl DoubleEndedIterator<Item = &'a Value>,
) -> Option<&'a str> {
    input.rev().find_map(|item| {
        // 显式 system 消息会由 Codex 编码器转为 developer，观测在编码前保持等价
        let system_message = item.get("type").and_then(Value::as_str) == Some("message")
            && item.get("role").and_then(Value::as_str) == Some("system");
        if non_empty_string(item.get("role")) != Some("developer") && !system_message {
            return None;
        }
        item.get("content")?
            .as_array()?
            .iter()
            .rev()
            .filter_map(|content| non_empty_string(content.get("text")))
            .find_map(multi_agent_mode_from_text)
    })
}

fn multi_agent_mode_from_text(text: &str) -> Option<&str> {
    let close = text.rfind(MULTI_AGENT_MODE_CLOSE_TAG)?;
    let before_close = &text[..close];
    let open = before_close.rfind(MULTI_AGENT_MODE_OPEN_TAG)?;
    let body = &before_close[open + MULTI_AGENT_MODE_OPEN_TAG.len()..];
    Some(body.trim())
}

fn non_empty_string(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}
