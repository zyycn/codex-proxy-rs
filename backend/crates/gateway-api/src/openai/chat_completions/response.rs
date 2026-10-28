//! 将实际 Responses wire 投影为 Chat JSON 和 SSE，不拥有执行、计费或终结生命周期

use std::collections::{BTreeMap, BTreeSet};

use bytes::Bytes;
use gateway_protocol::openai::events::{billable_usage_is_complete, extract_usage};
use gateway_protocol::openai::sse::encode_sse_event;
use serde::Serialize;
use serde_json::{Value, json};

use crate::openai::responses::{ProtocolError, ProtocolErrorBody};

#[derive(Debug, Clone, Serialize)]
pub(in crate::openai) struct ChatIdentity {
    pub id: String,
    pub created: u64,
    pub model: String,
}

#[derive(Serialize)]
struct Envelope<'a> {
    #[serde(flatten)]
    identity: &'a ChatIdentity,
    object: &'static str,
    choices: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    service_tier: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    moderation: Option<Value>,
}

/// 输入已由现有 Responses collector 聚合，投影不再恢复或重建缺失的 output
pub(in crate::openai) fn project_response(
    response: &Value,
    identity: &ChatIdentity,
) -> Result<Value, ProtocolErrorBody> {
    let output = response
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(invalid_response)?;
    validate_output_identities(output)?;
    let mut content = None;
    let mut refusal = None;
    let mut tools = Vec::new();
    for item in output {
        match required_str(item, "type")? {
            "message" => {
                for part in message_parts(item)? {
                    let (kind, text) = part_text(part)?;
                    let target = match kind {
                        PartKind::Text => &mut content,
                        PartKind::Refusal => &mut refusal,
                    };
                    target.get_or_insert_with(String::new).push_str(text);
                }
            }
            "function_call" => {
                let (id, name, arguments) = function_fields(item)?;
                tools.push(json!({
                    "id": id, "type": "function",
                    "function": {"name": name, "arguments": arguments}
                }));
            }
            "reasoning" => {}
            _ => return Err(invalid_response()),
        }
    }
    let finish_reason = finish_reason(response, !tools.is_empty())?;
    let mut message = json!({"role": "assistant", "content": content, "refusal": refusal});
    if !tools.is_empty() {
        message["tool_calls"] = Value::Array(tools);
    }
    serde_json::to_value(Envelope {
        identity,
        object: "chat.completion",
        choices: json!([{"index": 0, "message": message, "finish_reason": finish_reason, "logprobs": null}]),
        usage: chat_usage(response),
        metadata: response.get("metadata"),
        service_tier: response.get("service_tier"),
        moderation: project_moderation(response)?,
    })
    .map_err(|_| invalid_response())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PartKind {
    Text,
    Refusal,
}

impl PartKind {
    const fn field(self) -> &'static str {
        match self {
            Self::Text => "content",
            Self::Refusal => "refusal",
        }
    }
}

struct Part {
    kind: PartKind,
    text: String,
}

struct Tool {
    index: usize,
    call_id: String,
    name: String,
    arguments: String,
}

enum Output {
    Message(BTreeMap<u64, Part>),
    Function(Tool),
    Reasoning,
}

struct OutputItem {
    id: Option<String>,
    output: Output,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Terminal {
    Completed,
    Failed,
}

/// 只保存身份关联与快照去重所需内容；终态帧由外层受管 body 决定何时交付
pub(in crate::openai) struct ChatStreamEncoder {
    identity: ChatIdentity,
    include_usage: bool,
    role_sent: bool,
    output: BTreeMap<u64, OutputItem>,
    terminal: Option<Terminal>,
}

impl ChatStreamEncoder {
    pub(in crate::openai) fn new(identity: ChatIdentity, include_usage: bool) -> Self {
        Self {
            identity,
            include_usage,
            role_sent: false,
            output: BTreeMap::new(),
            terminal: None,
        }
    }

    pub(in crate::openai) fn is_completed(&self) -> bool {
        self.terminal == Some(Terminal::Completed)
    }

    pub(in crate::openai) fn has_wire_failure(&self) -> bool {
        self.terminal == Some(Terminal::Failed)
    }

    pub(in crate::openai) fn push_event(
        &mut self,
        event_type: Option<&str>,
        data: &Value,
    ) -> Result<Vec<Bytes>, ProtocolErrorBody> {
        let event_type = event_type.or_else(|| data.get("type").and_then(Value::as_str));
        if self.has_wire_failure() {
            return Ok(Vec::new());
        }
        let mut frames = Vec::new();
        match event_type {
            Some("response.created") => {
                self.require_open()?;
                if !self.role_sent {
                    frames.push(self.delta(json!({"role": "assistant"}))?);
                    self.role_sent = true;
                }
            }
            Some("response.output_text.delta" | "response.refusal.delta") => {
                self.require_open()?;
                let kind = if event_type == Some("response.refusal.delta") {
                    PartKind::Refusal
                } else {
                    PartKind::Text
                };
                let delta = required_str(data, "delta")?;
                let part = self.event_part(data, kind)?;
                part.text.push_str(delta);
                if !delta.is_empty() {
                    frames.push(self.delta(json!({kind.field(): delta}))?);
                }
            }
            Some("response.output_text.done" | "response.refusal.done") => {
                self.require_open()?;
                let (kind, field) = if event_type == Some("response.refusal.done") {
                    (PartKind::Refusal, "refusal")
                } else {
                    (PartKind::Text, "text")
                };
                let text = required_str(data, field)?;
                let part = self.event_part(data, kind)?;
                let suffix = append_snapshot(&mut part.text, text)?;
                if !suffix.is_empty() {
                    frames.push(self.delta(json!({kind.field(): suffix}))?);
                }
            }
            Some("response.content_part.added" | "response.content_part.done") => {
                self.require_open()?;
                let part = data.get("part").ok_or_else(invalid_response)?;
                let (kind, text) = part_text(part)?;
                let part = self.event_part(data, kind)?;
                let suffix = append_snapshot(&mut part.text, text)?;
                if !suffix.is_empty() {
                    frames.push(self.delta(json!({kind.field(): suffix}))?);
                }
            }
            Some("response.output_item.added" | "response.output_item.done") => {
                self.require_open()?;
                self.item_snapshot(
                    required_index(data, "output_index")?,
                    data.get("item").ok_or_else(invalid_response)?,
                    false,
                    &mut frames,
                )?;
            }
            Some(
                "response.function_call_arguments.delta" | "response.function_call_arguments.done",
            ) => {
                self.require_open()?;
                let index = required_index(data, "output_index")?;
                let item = self.output.get_mut(&index).ok_or_else(invalid_response)?;
                bind_id(item, optional_id(data, "item_id")?)?;
                let Output::Function(tool) = &mut item.output else {
                    return Err(invalid_response());
                };
                let delta = if event_type == Some("response.function_call_arguments.delta") {
                    let delta = required_str(data, "delta")?;
                    tool.arguments.push_str(delta);
                    delta
                } else {
                    append_snapshot(&mut tool.arguments, required_str(data, "arguments")?)?
                };
                let tool_index = tool.index;
                if !delta.is_empty() {
                    frames.push(self.delta(json!({"tool_calls": [{"index": tool_index, "function": {"arguments": delta}}]}))?);
                }
            }
            Some("response.completed" | "response.incomplete") => {
                self.require_open()?;
                let response = data.get("response").ok_or_else(invalid_response)?;
                let status = required_str(response, "status")?;
                if (event_type == Some("response.completed") && status != "completed")
                    || (event_type == Some("response.incomplete") && status != "incomplete")
                {
                    return Err(invalid_response());
                }
                if let Some(output) = response.get("output") {
                    let output = output.as_array().ok_or_else(invalid_response)?;
                    validate_output_identities(output)?;
                    for (index, item) in output.iter().enumerate() {
                        self.item_snapshot(index as u64, item, true, &mut frames)?;
                    }
                } else if self.output.is_empty() {
                    return Err(invalid_response());
                }
                let finish_reason = finish_reason(response, self.tool_count() > 0)?;
                frames.push(self.chunk(
                    json!([{"index": 0, "delta": {}, "finish_reason": finish_reason, "logprobs": null}]),
                    self.include_usage.then_some(Value::Null),
                    Some(response),
                )?);
                if self.include_usage
                    && let Some(usage) = chat_usage(response)
                {
                    frames.push(self.chunk(json!([]), Some(usage), None)?);
                }
                self.terminal = Some(Terminal::Completed);
            }
            Some("response.failed" | "error") => {
                self.require_open()?;
                let error = data
                    .pointer("/response/error")
                    .or_else(|| data.get("error"))
                    .unwrap_or(data);
                required_str(error, "message")?;
                // 错误恢复信号和扩展由既有出口拥有，Chat 仅转换外层包装
                let mut error = error.clone();
                if error
                    .get("type")
                    .and_then(Value::as_str)
                    .is_none_or(|kind| kind == "error")
                {
                    error["type"] = json!("server_error");
                }
                frames.push(sse_frame(&json!({"error": error})));
                self.terminal = Some(Terminal::Failed);
            }
            // 辅助事件可随上游演进；实际 output item 和 content part 仍逐项检查可表达性
            _ => {}
        }
        // done-only 流也要提供完整 Chat 消息身份，辅助事件或私有推理不触发角色帧
        if !self.role_sent && !frames.is_empty() && !self.has_wire_failure() {
            frames.insert(0, self.delta(json!({"role": "assistant"}))?);
            self.role_sent = true;
        }
        Ok(frames)
    }

    fn require_open(&self) -> Result<(), ProtocolErrorBody> {
        if self.terminal.is_some() {
            Err(invalid_response())
        } else {
            Ok(())
        }
    }

    fn tool_count(&self) -> usize {
        self.output
            .values()
            .filter(|item| matches!(item.output, Output::Function(_)))
            .count()
    }

    fn event_part(&mut self, data: &Value, kind: PartKind) -> Result<&mut Part, ProtocolErrorBody> {
        let index = required_index(data, "output_index")?;
        let content_index = required_index(data, "content_index")?;
        let id = optional_id(data, "item_id")?;
        let item = self.output.entry(index).or_insert_with(|| OutputItem {
            id: None,
            output: Output::Message(BTreeMap::new()),
        });
        bind_id(item, id)?;
        message_part(item, content_index, kind)
    }

    fn item_snapshot(
        &mut self,
        index: u64,
        snapshot: &Value,
        terminal: bool,
        frames: &mut Vec<Bytes>,
    ) -> Result<(), ProtocolErrorBody> {
        let id = optional_id(snapshot, "id")?;
        let kind = required_str(snapshot, "type")?;
        let call_id = if kind == "function_call" {
            Some(required_nonempty_str(snapshot, "call_id")?)
        } else {
            None
        };
        let known_index = self.find_item(id, call_id)?;
        let index = match known_index {
            Some(known) if terminal || known == index => known,
            Some(_) => return Err(invalid_response()),
            None => index,
        };
        if !self.output.contains_key(&index) {
            let output = match kind {
                "message" => Output::Message(BTreeMap::new()),
                "reasoning" => Output::Reasoning,
                "function_call" => {
                    let (call_id, name, _) = function_fields(snapshot)?;
                    let tool_index = self.tool_count();
                    frames.push(self.delta(json!({"tool_calls": [{"index": tool_index, "id": call_id, "type": "function", "function": {"name": name, "arguments": ""}}]}))?);
                    Output::Function(Tool {
                        index: tool_index,
                        call_id: call_id.to_owned(),
                        name: name.to_owned(),
                        arguments: String::new(),
                    })
                }
                _ => return Err(invalid_response()),
            };
            self.output.insert(index, OutputItem { id: None, output });
        }
        let item = self.output.get_mut(&index).ok_or_else(invalid_response)?;
        bind_id(item, id)?;
        match (&mut item.output, kind) {
            (Output::Message(parts), "message") => {
                for (index, part) in message_parts(snapshot)?.iter().enumerate() {
                    let (kind, text) = part_text(part)?;
                    let part = part_entry(parts, index as u64, kind)?;
                    let suffix = append_snapshot(&mut part.text, text)?;
                    if !suffix.is_empty() {
                        frames.push(delta_frame(
                            &self.identity,
                            self.include_usage,
                            json!({kind.field(): suffix}),
                        )?);
                    }
                }
            }
            (Output::Function(tool), "function_call") => {
                let (id, name, arguments) = function_fields(snapshot)?;
                if tool.call_id != id || tool.name != name {
                    return Err(invalid_response());
                }
                let suffix = append_snapshot(&mut tool.arguments, arguments)?;
                if !suffix.is_empty() {
                    frames.push(delta_frame(&self.identity, self.include_usage, json!({"tool_calls": [{"index": tool.index, "function": {"arguments": suffix}}]}))?);
                }
            }
            (Output::Reasoning, "reasoning") => {}
            _ => return Err(invalid_response()),
        }
        Ok(())
    }

    fn find_item(
        &self,
        id: Option<&str>,
        call_id: Option<&str>,
    ) -> Result<Option<u64>, ProtocolErrorBody> {
        let mut found = None;
        for (&index, item) in &self.output {
            let same_id = id.is_some() && item.id.as_deref() == id;
            let same_call = matches!(&item.output, Output::Function(tool) if call_id == Some(tool.call_id.as_str()));
            if same_id || same_call {
                if found.is_some_and(|previous| previous != index) {
                    return Err(invalid_response());
                }
                found = Some(index);
            }
        }
        Ok(found)
    }

    fn delta(&self, delta: Value) -> Result<Bytes, ProtocolErrorBody> {
        delta_frame(&self.identity, self.include_usage, delta)
    }

    fn chunk(
        &self,
        choices: Value,
        usage: Option<Value>,
        response: Option<&Value>,
    ) -> Result<Bytes, ProtocolErrorBody> {
        chunk_frame(&self.identity, choices, usage, response)
    }
}

fn message_part(
    item: &mut OutputItem,
    index: u64,
    kind: PartKind,
) -> Result<&mut Part, ProtocolErrorBody> {
    let Output::Message(parts) = &mut item.output else {
        return Err(invalid_response());
    };
    part_entry(parts, index, kind)
}

fn part_entry(
    parts: &mut BTreeMap<u64, Part>,
    index: u64,
    kind: PartKind,
) -> Result<&mut Part, ProtocolErrorBody> {
    let part = parts.entry(index).or_insert_with(|| Part {
        kind,
        text: String::new(),
    });
    if part.kind != kind {
        return Err(invalid_response());
    }
    Ok(part)
}

fn bind_id(item: &mut OutputItem, id: Option<&str>) -> Result<(), ProtocolErrorBody> {
    if let Some(id) = id {
        if item.id.as_deref().is_some_and(|previous| previous != id) {
            return Err(invalid_response());
        }
        if item.id.is_none() {
            item.id = Some(id.to_owned());
        }
    }
    Ok(())
}

fn append_snapshot<'a>(text: &mut String, snapshot: &'a str) -> Result<&'a str, ProtocolErrorBody> {
    let suffix = snapshot
        .strip_prefix(text.as_str())
        .ok_or_else(invalid_response)?;
    text.push_str(suffix);
    Ok(suffix)
}

fn message_parts(item: &Value) -> Result<&[Value], ProtocolErrorBody> {
    if required_str(item, "role")? != "assistant" {
        return Err(invalid_response());
    }
    item.get("content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(invalid_response)
}

fn part_text(part: &Value) -> Result<(PartKind, &str), ProtocolErrorBody> {
    match required_str(part, "type")? {
        "output_text" => Ok((PartKind::Text, required_str(part, "text")?)),
        "refusal" => Ok((PartKind::Refusal, required_str(part, "refusal")?)),
        _ => Err(invalid_response()),
    }
}

fn function_fields(item: &Value) -> Result<(&str, &str, &str), ProtocolErrorBody> {
    Ok((
        required_nonempty_str(item, "call_id")?,
        required_nonempty_str(item, "name")?,
        required_str(item, "arguments")?,
    ))
}

fn validate_output_identities(output: &[Value]) -> Result<(), ProtocolErrorBody> {
    let mut item_ids = BTreeSet::new();
    let mut call_ids = BTreeSet::new();
    for item in output {
        if let Some(id) = optional_id(item, "id")?
            && !item_ids.insert(id)
        {
            return Err(invalid_response());
        }
        if item.get("type").and_then(Value::as_str) == Some("function_call")
            && !call_ids.insert(required_nonempty_str(item, "call_id")?)
        {
            return Err(invalid_response());
        }
    }
    Ok(())
}

fn finish_reason(response: &Value, has_tools: bool) -> Result<&'static str, ProtocolErrorBody> {
    if response.get("error").is_some_and(|value| !value.is_null()) {
        return Err(invalid_response());
    }
    match required_str(response, "status")? {
        "completed" => Ok(if has_tools { "tool_calls" } else { "stop" }),
        "incomplete" => match response
            .pointer("/incomplete_details/reason")
            .and_then(Value::as_str)
        {
            Some("max_output_tokens") => Ok("length"),
            Some("content_filter") => Ok("content_filter"),
            _ => Err(invalid_response()),
        },
        _ => Err(invalid_response()),
    }
}

fn chat_usage(response: &Value) -> Option<Value> {
    let usage = extract_usage(response)?;
    if !billable_usage_is_complete(response, usage) {
        return None;
    }
    let mut result = json!({"prompt_tokens": usage.input_tokens, "completion_tokens": usage.output_tokens, "total_tokens": usage.total_tokens});
    if response
        .pointer("/usage/input_tokens_details/cached_tokens")
        .is_some()
    {
        result["prompt_tokens_details"] = json!({"cached_tokens": usage.cached_input_tokens});
    }
    if response
        .pointer("/usage/output_tokens_details/reasoning_tokens")
        .and_then(Value::as_u64)
        .is_some_and(|tokens| tokens <= usage.output_tokens)
    {
        result["completion_tokens_details"] =
            json!({"reasoning_tokens": usage.reasoning_output_tokens});
    }
    Some(result)
}

fn project_moderation(response: &Value) -> Result<Option<Value>, ProtocolErrorBody> {
    let Some(moderation) = response.get("moderation").filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let mut result = json!({});
    for side in ["input", "output"] {
        let value = moderation.get(side).ok_or_else(invalid_response)?;
        result[side] = match required_str(value, "type")? {
            "moderation_result" => {
                json!({"type": "moderation_results", "model": required_str(value, "model")?, "results": [value]})
            }
            "error" => value.clone(),
            _ => return Err(invalid_response()),
        };
    }
    Ok(Some(result))
}

fn delta_frame(
    identity: &ChatIdentity,
    include_usage: bool,
    delta: Value,
) -> Result<Bytes, ProtocolErrorBody> {
    chunk_frame(
        identity,
        json!([{"index": 0, "delta": delta, "finish_reason": null, "logprobs": null}]),
        include_usage.then_some(Value::Null),
        None,
    )
}

fn chunk_frame(
    identity: &ChatIdentity,
    choices: Value,
    usage: Option<Value>,
    response: Option<&Value>,
) -> Result<Bytes, ProtocolErrorBody> {
    let value = serde_json::to_value(Envelope {
        identity,
        object: "chat.completion.chunk",
        choices,
        usage,
        metadata: None,
        service_tier: response.and_then(|value| value.get("service_tier")),
        moderation: response.map(project_moderation).transpose()?.flatten(),
    })
    .map_err(|_| invalid_response())?;
    Ok(sse_frame(&value))
}

fn sse_frame(value: &Value) -> Bytes {
    Bytes::from(encode_sse_event("", &value.to_string()))
}

fn required_str<'a>(value: &'a Value, field: &str) -> Result<&'a str, ProtocolErrorBody> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(invalid_response)
}

fn required_nonempty_str<'a>(value: &'a Value, field: &str) -> Result<&'a str, ProtocolErrorBody> {
    let value = required_str(value, field)?;
    if value.is_empty() {
        return Err(invalid_response());
    }
    Ok(value)
}

fn optional_id<'a>(value: &'a Value, field: &str) -> Result<Option<&'a str>, ProtocolErrorBody> {
    value
        .get(field)
        .map(|_| required_nonempty_str(value, field))
        .transpose()
}

fn required_index(value: &Value, field: &str) -> Result<u64, ProtocolErrorBody> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(invalid_response)
}

fn invalid_response() -> ProtocolErrorBody {
    ProtocolErrorBody {
        error: ProtocolError {
            kind: "server_error",
            code: "invalid_upstream_response",
            message: "The upstream response cannot be represented as a Chat completion.".to_owned(),
            param: None,
        },
    }
}
