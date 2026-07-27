use serde_json::{Map, Value};

const MULTI_AGENT_MODE_OPEN_TAG: &str = "<multi_agent_mode>";
const MULTI_AGENT_MODE_CLOSE_TAG: &str = "</multi_agent_mode>";
const PROACTIVE_MULTI_AGENT_MODE_PREFIX: &str = "Proactive multi-agent delegation is active.";

/// 从 OpenAI Responses 请求中提取的稳定 Codex 请求语义。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexResponsesRequestSemantics {
    /// 客户端原始选择的推理强度。
    pub reasoning_effort: Option<String>,
    /// Codex turn metadata 中的请求类型。
    pub request_kind: Option<String>,
    /// Codex turn metadata 中的子代理类型。
    pub subagent_kind: Option<String>,
    /// 网关侧派生出的推理预设。
    pub reasoning_preset: Option<&'static str>,
    /// 请求是否为 Codex 压缩请求。
    pub compact: bool,
}

/// 从 Responses body 与非 wire 协议上下文中提取 Codex 语义。
#[must_use]
pub fn codex_responses_request_semantics(
    body: &Map<String, Value>,
    context: &Map<String, Value>,
) -> CodexResponsesRequestSemantics {
    codex_responses_request_semantics_with_turn_metadata(
        body,
        non_empty_string(context.get("turn_metadata")),
    )
}

/// 调用方已解析权威 turn metadata 时，从 Responses body 中提取 Codex 语义。
#[must_use]
pub fn codex_responses_request_semantics_with_turn_metadata(
    body: &Map<String, Value>,
    turn_metadata: Option<&str>,
) -> CodexResponsesRequestSemantics {
    let reasoning_effort = body
        .get("reasoning")
        .and_then(Value::as_object)
        .and_then(|reasoning| non_empty_string(reasoning.get("effort")))
        .map(ToOwned::to_owned);
    let parsed_turn_metadata = turn_metadata
        .or_else(|| request_turn_metadata(body))
        .and_then(|value| serde_json::from_str::<Value>(value).ok());
    let request_kind = parsed_turn_metadata
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

    CodexResponsesRequestSemantics {
        reasoning_effort,
        request_kind,
        subagent_kind,
        reasoning_preset,
        compact,
    }
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
        if non_empty_string(item.get("role")) != Some("developer") {
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
