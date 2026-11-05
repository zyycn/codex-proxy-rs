//! 将实际 Responses wire 投影为 Chat JSON 和 SSE，不拥有执行、计费或终结生命周期

use std::collections::{BTreeMap, BTreeSet};

use bytes::Bytes;
use gateway_protocol::openai::events::{billable_usage_is_complete, extract_usage};
use gateway_protocol::openai::sse::encode_sse_event;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::client_error::{ProtocolError, ProtocolErrorBody};

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
    let mut annotations = Vec::new();
    let mut content_chars = 0;
    let mut reasoning = Vec::new();
    let mut images = Vec::new();
    let mut logprobs = None;
    for item in output {
        match required_str(item, "type")? {
            "message" => {
                // 两侧索引都按消息中的字符计数，合并多条 assistant 消息时平移引用
                for part in message_parts(item)? {
                    let (kind, text) = part_text(part)?;
                    let target = match kind {
                        PartKind::Text => {
                            append_url_citations(part, content_chars, &mut annotations)?;
                            if let Some(values) = part.get("logprobs").and_then(Value::as_array) {
                                logprobs
                                    .get_or_insert_with(Vec::new)
                                    .extend(values.iter().cloned());
                            }
                            content_chars += text.chars().count() as u64;
                            &mut content
                        }
                        PartKind::Refusal => &mut refusal,
                    };
                    target.get_or_insert_with(String::new).push_str(text);
                }
            }
            "function_call" | "custom_tool_call" => {
                let kind = ToolKind::from_item(item)?;
                let (id, name, data) = tool_fields(item, kind)?;
                tools.push(json!({"id":id, "type":kind.chat_type(),
                    kind.chat_type():{"name":name, kind.data_field():data}}));
            }
            "reasoning" => {
                for (_, _, text) in reasoning_parts(item)? {
                    if !text.is_empty() {
                        reasoning.push(text);
                    }
                }
            }
            "image_generation_call" => {
                if let Some(image) = image_payload(item)? {
                    images.push(image);
                }
            }
            kind if is_auxiliary_output(kind) => {}
            _ => return Err(invalid_response()),
        }
    }
    let finish_reason = finish_reason(response, !tools.is_empty())?;
    let mut message = json!({"role": "assistant", "content": content, "refusal": refusal});
    if !tools.is_empty() {
        message["tool_calls"] = Value::Array(tools);
    }
    if !annotations.is_empty() {
        message["annotations"] = Value::Array(annotations);
    }
    if !reasoning.is_empty() {
        message["reasoning_content"] = reasoning.join("\n\n").into();
    }
    if !images.is_empty() {
        message["images"] = images.into();
    }
    let logprobs = logprobs.map(|content| json!({"content":content, "refusal":null}));
    serde_json::to_value(Envelope {
        identity,
        object: "chat.completion",
        choices: json!([{"index": 0, "message": message, "finish_reason": finish_reason, "logprobs": logprobs}]),
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
    logprobs: Vec<Value>,
    annotations: BTreeMap<u64, Value>,
    spans: Vec<TextSpan>,
}

struct TextSpan {
    start: u64,
    end: u64,
    delivered_start: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolKind {
    Function,
    Custom,
}

impl ToolKind {
    fn from_item(item: &Value) -> Result<Self, ProtocolErrorBody> {
        match required_str(item, "type")? {
            "function_call" => Ok(Self::Function),
            "custom_tool_call" => Ok(Self::Custom),
            _ => Err(invalid_response()),
        }
    }
    const fn chat_type(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Custom => "custom",
        }
    }
    const fn data_field(self) -> &'static str {
        match self {
            Self::Function => "arguments",
            Self::Custom => "input",
        }
    }
}

struct Tool {
    kind: ToolKind,
    index: usize,
    call_id: String,
    name: String,
    arguments: String,
}

enum Output {
    Message(BTreeMap<u64, Part>),
    Tool(Tool),
    Reasoning(BTreeMap<(bool, u64), String>),
    Image {
        index: usize,
        fingerprint: Option<[u8; 32]>,
    },
    Auxiliary(String),
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
    content_chars: u64,
}

impl ChatStreamEncoder {
    pub(in crate::openai) fn new(identity: ChatIdentity, include_usage: bool) -> Self {
        Self {
            identity,
            include_usage,
            role_sent: false,
            output: BTreeMap::new(),
            terminal: None,
            content_chars: 0,
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
                self.text_event(data, kind, delta, false, data, &mut frames)?;
            }
            Some("response.output_text.done" | "response.refusal.done") => {
                self.require_open()?;
                let (kind, field) = if event_type == Some("response.refusal.done") {
                    (PartKind::Refusal, "refusal")
                } else {
                    (PartKind::Text, "text")
                };
                self.text_event(
                    data,
                    kind,
                    required_str(data, field)?,
                    true,
                    data,
                    &mut frames,
                )?;
            }
            Some("response.content_part.added" | "response.content_part.done") => {
                self.require_open()?;
                let part = data.get("part").ok_or_else(invalid_response)?;
                let (kind, text) = part_text(part)?;
                self.text_event(data, kind, text, true, part, &mut frames)?;
            }
            Some("response.output_text.annotation.added") => {
                self.require_open()?;
                let part = self.event_part(data, PartKind::Text)?;
                insert_annotation(
                    part,
                    required_index(data, "annotation_index")?,
                    data.get("annotation").ok_or_else(invalid_response)?,
                )?;
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
                "response.function_call_arguments.delta"
                | "response.function_call_arguments.done"
                | "response.custom_tool_call_input.delta"
                | "response.custom_tool_call_input.done",
            ) => {
                self.require_open()?;
                let index = required_index(data, "output_index")?;
                let item = self.output.get_mut(&index).ok_or_else(invalid_response)?;
                bind_id(item, optional_id(data, "item_id")?)?;
                let Output::Tool(tool) = &mut item.output else {
                    return Err(invalid_response());
                };
                let kind = tool.kind;
                let custom_event = event_type
                    .is_some_and(|event| event.starts_with("response.custom_tool_call_input."));
                if custom_event != (kind == ToolKind::Custom) {
                    return Err(invalid_response());
                }
                let delta = if event_type.is_some_and(|event| event.ends_with(".delta")) {
                    let delta = required_str(data, "delta")?;
                    tool.arguments.push_str(delta);
                    delta
                } else {
                    append_snapshot(&mut tool.arguments, required_str(data, kind.data_field())?)?
                };
                let tool_index = tool.index;
                if !delta.is_empty() {
                    frames.push(self.delta(json!({"tool_calls": [{"index": tool_index, kind.chat_type(): {kind.data_field(): delta}}]}))?);
                }
            }
            Some(
                "response.reasoning_summary_text.delta"
                | "response.reasoning_summary_text.done"
                | "response.reasoning_text.delta"
                | "response.reasoning_text.done",
            ) => {
                self.require_open()?;
                let summary =
                    event_type.is_some_and(|event| event.starts_with("response.reasoning_summary"));
                let snapshot = event_type.is_some_and(|event| event.ends_with(".done"));
                self.reasoning_text(
                    required_index(data, "output_index")?,
                    optional_id(data, "item_id")?,
                    (
                        summary,
                        required_index(
                            data,
                            if summary {
                                "summary_index"
                            } else {
                                "content_index"
                            },
                        )?,
                    ),
                    required_str(data, if snapshot { "text" } else { "delta" })?,
                    snapshot,
                    &mut frames,
                )?;
            }
            Some("response.image_generation_call.partial_image") => {
                self.require_open()?;
                self.item_snapshot(required_index(data, "output_index")?, &json!({
                    "id": required_nonempty_str(data, "item_id")?, "type":"image_generation_call",
                    "result": required_str(data, "partial_image_b64")?,
                    "output_format": data.get("output_format").unwrap_or(&json!("png")),
                }), false, &mut frames)?;
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
                let annotations = self.stream_annotations()?;
                if !annotations.is_empty() {
                    frames.push(self.delta(json!({"annotations":annotations}))?);
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
        // done-only 流也要提供完整 Chat 消息身份，不含可交付内容的辅助事件不触发角色帧
        if !self.role_sent && !frames.is_empty() && !self.has_wire_failure() {
            frames.insert(0, self.delta(json!({"role": "assistant"}))?);
            self.role_sent = true;
        }
        Ok(frames)
    }

    fn text_event(
        &mut self,
        data: &Value,
        kind: PartKind,
        text: &str,
        snapshot: bool,
        metadata: &Value,
        frames: &mut Vec<Bytes>,
    ) -> Result<(), ProtocolErrorBody> {
        let mut content_chars = self.content_chars;
        let part = self.event_part(data, kind)?;
        let delta = if snapshot {
            append_snapshot(&mut part.text, text)?
        } else {
            part.text.push_str(text);
            text
        };
        record_text_span(part, &mut content_chars, delta);
        merge_annotations(part, metadata)?;
        let logprobs = append_logprobs(part, metadata, snapshot)?;
        self.content_chars = content_chars;
        if !delta.is_empty() {
            frames.push(self.delta(json!({kind.field():delta}))?);
        }
        if !logprobs.is_empty() {
            frames.push(logprobs_frame(
                &self.identity,
                self.include_usage,
                kind,
                logprobs,
            )?);
        }
        Ok(())
    }

    fn stream_annotations(&self) -> Result<Vec<Value>, ProtocolErrorBody> {
        let mut result = Vec::new();
        for item in self.output.values() {
            if let Output::Message(parts) = &item.output {
                for part in parts.values() {
                    let mut citations = Vec::new();
                    append_url_citations(
                        &json!({"annotations":part.annotations.values().collect::<Vec<_>>()}),
                        0,
                        &mut citations,
                    )?;
                    for mut citation in citations {
                        for field in ["start_index", "end_index"] {
                            let position = required_index(&citation["url_citation"], field)?;
                            citation["url_citation"][field] =
                                delivered_position(part, position, field == "end_index")?.into();
                        }
                        result.push(citation);
                    }
                }
            }
        }
        Ok(result)
    }

    fn reasoning_text(
        &mut self,
        index: u64,
        id: Option<&str>,
        part: (bool, u64),
        text: &str,
        snapshot: bool,
        frames: &mut Vec<Bytes>,
    ) -> Result<(), ProtocolErrorBody> {
        let has_text = self.output.values().any(|item| {
            matches!(&item.output,
            Output::Reasoning(parts) if parts.values().any(|text| !text.is_empty()))
        });
        let item = self.output.entry(index).or_insert_with(|| OutputItem {
            id: None,
            output: Output::Reasoning(BTreeMap::new()),
        });
        bind_id(item, id)?;
        let Output::Reasoning(parts) = &mut item.output else {
            return Err(invalid_response());
        };
        let previous = parts.entry(part).or_default();
        let separator = previous.is_empty() && has_text;
        let delta = if snapshot {
            append_snapshot(previous, text)?
        } else {
            previous.push_str(text);
            text
        };
        if !delta.is_empty() {
            let delta = if separator {
                format!("\n\n{delta}")
            } else {
                delta.to_owned()
            };
            frames.push(self.delta(json!({"reasoning_content":delta}))?);
        }
        Ok(())
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
            .filter(|item| matches!(item.output, Output::Tool(_)))
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
        let call_id = if matches!(kind, "function_call" | "custom_tool_call") {
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
                "reasoning" => Output::Reasoning(BTreeMap::new()),
                "image_generation_call" => Output::Image {
                    index: self
                        .output
                        .values()
                        .filter(|item| matches!(item.output, Output::Image { .. }))
                        .count(),
                    fingerprint: None,
                },
                kind if is_auxiliary_output(kind) => Output::Auxiliary(kind.to_owned()),
                "function_call" | "custom_tool_call" => {
                    let kind = ToolKind::from_item(snapshot)?;
                    let (call_id, name, _) = tool_fields(snapshot, kind)?;
                    let tool_index = self.tool_count();
                    frames.push(self.delta(json!({"tool_calls": [{"index": tool_index, "id": call_id, "type": kind.chat_type(), kind.chat_type(): {"name": name, kind.data_field(): ""}}]}))?);
                    Output::Tool(Tool {
                        kind,
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
                    let snapshot = part;
                    let part = part_entry(parts, index as u64, kind)?;
                    let suffix = append_snapshot(&mut part.text, text)?;
                    record_text_span(part, &mut self.content_chars, suffix);
                    merge_annotations(part, snapshot)?;
                    let logprobs = append_logprobs(part, snapshot, true)?;
                    if !logprobs.is_empty() {
                        frames.push(logprobs_frame(
                            &self.identity,
                            self.include_usage,
                            kind,
                            logprobs,
                        )?);
                    }
                    if !suffix.is_empty() {
                        frames.push(delta_frame(
                            &self.identity,
                            self.include_usage,
                            json!({kind.field(): suffix}),
                        )?);
                    }
                }
            }
            (Output::Tool(tool), "function_call" | "custom_tool_call") => {
                let kind = ToolKind::from_item(snapshot)?;
                if kind != tool.kind {
                    return Err(invalid_response());
                }
                let (id, name, arguments) = tool_fields(snapshot, kind)?;
                if tool.call_id != id || tool.name != name {
                    return Err(invalid_response());
                }
                let suffix = append_snapshot(&mut tool.arguments, arguments)?;
                if !suffix.is_empty() {
                    frames.push(delta_frame(&self.identity, self.include_usage, json!({"tool_calls": [{"index": tool.index, kind.chat_type(): {kind.data_field(): suffix}}]}))?);
                }
            }
            (Output::Reasoning(_), "reasoning") => {
                for (summary, part_index, text) in reasoning_parts(snapshot)? {
                    self.reasoning_text(index, id, (summary, part_index), text, true, frames)?;
                }
            }
            (Output::Image { index, fingerprint }, "image_generation_call") => {
                if let Some(mut image) = image_payload(snapshot)? {
                    let hash: [u8; 32] =
                        Sha256::digest(required_str(&image["image_url"], "url")?.as_bytes()).into();
                    if *fingerprint != Some(hash) {
                        *fingerprint = Some(hash);
                        image["index"] = (*index).into();
                        frames.push(delta_frame(
                            &self.identity,
                            self.include_usage,
                            json!({"images":[image]}),
                        )?);
                    }
                }
            }
            (Output::Auxiliary(previous), kind) if previous == kind => {}
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
            let same_call = matches!(&item.output, Output::Tool(tool) if call_id == Some(tool.call_id.as_str()));
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
        logprobs: Vec::new(),
        annotations: BTreeMap::new(),
        spans: Vec::new(),
    });
    if part.kind != kind {
        return Err(invalid_response());
    }
    Ok(part)
}

fn record_text_span(part: &mut Part, delivered: &mut u64, delta: &str) {
    if part.kind != PartKind::Text || delta.is_empty() {
        return;
    }
    let length = delta.chars().count() as u64;
    let start = part.spans.last().map_or(0, |span| span.end);
    if let Some(last) = part.spans.last_mut()
        && last.delivered_start + last.end - last.start == *delivered
    {
        last.end += length;
    } else {
        part.spans.push(TextSpan {
            start,
            end: start + length,
            delivered_start: *delivered,
        });
    }
    *delivered += length;
}

fn delivered_position(part: &Part, position: u64, end: bool) -> Result<u64, ProtocolErrorBody> {
    let matches = |span: &&TextSpan| {
        if end && position > 0 {
            span.start < position && position <= span.end
        } else {
            span.start <= position && position < span.end
        }
    };
    let span = part
        .spans
        .iter()
        .find(matches)
        .or_else(|| part.spans.last().filter(|span| position == span.end))
        .ok_or_else(invalid_response)?;
    Ok(span.delivered_start + position - span.start)
}

fn merge_annotations(part: &mut Part, value: &Value) -> Result<(), ProtocolErrorBody> {
    if let Some(values) = value.get("annotations").filter(|value| !value.is_null()) {
        for (index, annotation) in values
            .as_array()
            .ok_or_else(invalid_response)?
            .iter()
            .enumerate()
        {
            insert_annotation(part, index as u64, annotation)?;
        }
    }
    Ok(())
}

fn insert_annotation(part: &mut Part, index: u64, value: &Value) -> Result<(), ProtocolErrorBody> {
    if let Some(previous) = part.annotations.get(&index) {
        if previous != value {
            return Err(invalid_response());
        }
    } else {
        part.annotations.insert(index, value.clone());
    }
    Ok(())
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

fn append_url_citations(
    part: &Value,
    message_offset: u64,
    annotations: &mut Vec<Value>,
) -> Result<(), ProtocolErrorBody> {
    let Some(values) = part.get("annotations").filter(|value| !value.is_null()) else {
        return Ok(());
    };
    for value in values.as_array().ok_or_else(invalid_response)? {
        if value["type"] != "url_citation" {
            continue;
        }
        let start = required_index(value, "start_index")?;
        let end = required_index(value, "end_index")?;
        if start > end {
            return Err(invalid_response());
        }
        annotations.push(json!({"type": "url_citation", "url_citation": {
            "url": required_str(value, "url")?,
            "title": required_str(value, "title")?,
            "start_index": message_offset.checked_add(start).ok_or_else(invalid_response)?,
            "end_index": message_offset.checked_add(end).ok_or_else(invalid_response)?,
        }}));
    }
    Ok(())
}

fn is_auxiliary_output(kind: &str) -> bool {
    // 服务端工具记录和压缩密文不要求 Chat 客户端执行动作，也不投影为可读内容
    matches!(
        kind,
        "web_search_call"
            | "file_search_call"
            | "code_interpreter_call"
            | "mcp_call"
            | "mcp_list_tools"
            | "compaction"
            | "compaction_summary"
            | "context_compaction"
    )
}

fn reasoning_parts(item: &Value) -> Result<Vec<(bool, u64, &str)>, ProtocolErrorBody> {
    let mut result = Vec::new();
    // 只交付上游明确公开的推理文本，不解读或暴露 encrypted_content
    for (field, kind) in [("summary", "summary_text"), ("content", "reasoning_text")] {
        if let Some(parts) = item.get(field).filter(|value| !value.is_null()) {
            for (index, part) in parts
                .as_array()
                .ok_or_else(invalid_response)?
                .iter()
                .enumerate()
            {
                if part["type"] == kind {
                    result.push((
                        field == "summary",
                        index as u64,
                        required_str(part, "text")?,
                    ));
                }
            }
        }
    }
    Ok(result)
}

fn image_payload(item: &Value) -> Result<Option<Value>, ProtocolErrorBody> {
    let Some(data) = item.get("result").filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let data = data.as_str().ok_or_else(invalid_response)?;
    if data.is_empty() {
        return Ok(None);
    }
    let format = item
        .get("output_format")
        .and_then(Value::as_str)
        .unwrap_or("png");
    let mime = match format {
        "png" => "image/png",
        "jpeg" | "jpg" => "image/jpeg",
        "webp" => "image/webp",
        _ => return Err(invalid_response()),
    };
    Ok(Some(
        json!({"type":"image_url", "image_url":{"url":format!("data:{mime};base64,{data}")}}),
    ))
}

fn append_logprobs(
    part: &mut Part,
    value: &Value,
    snapshot: bool,
) -> Result<Vec<Value>, ProtocolErrorBody> {
    let Some(values) = value.get("logprobs").filter(|value| !value.is_null()) else {
        return Ok(Vec::new());
    };
    let values = values.as_array().ok_or_else(invalid_response)?;
    let delta = if snapshot {
        values
            .strip_prefix(part.logprobs.as_slice())
            .ok_or_else(invalid_response)?
    } else {
        values
    };
    let delta = delta.to_vec();
    part.logprobs.extend(delta.iter().cloned());
    Ok(delta)
}

fn logprobs_frame(
    identity: &ChatIdentity,
    include_usage: bool,
    kind: PartKind,
    values: Vec<Value>,
) -> Result<Bytes, ProtocolErrorBody> {
    chunk_frame(
        identity,
        json!([{"index":0, "delta":{}, "finish_reason":null,
        "logprobs":{kind.field():values}}]),
        include_usage.then_some(Value::Null),
        None,
    )
}

fn tool_fields(item: &Value, kind: ToolKind) -> Result<(&str, &str, &str), ProtocolErrorBody> {
    Ok((
        required_nonempty_str(item, "call_id")?,
        required_nonempty_str(item, "name")?,
        required_str(item, kind.data_field())?,
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
        if matches!(
            item.get("type").and_then(Value::as_str),
            Some("function_call" | "custom_tool_call")
        ) && !call_ids.insert(required_nonempty_str(item, "call_id")?)
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
    for (source, target) in [
        ("input_tokens_details", "prompt_tokens_details"),
        ("output_tokens_details", "completion_tokens_details"),
    ] {
        if let Some(details) = response
            .get("usage")
            .and_then(|usage| usage.get(source))
            .filter(|value| value.is_object())
        {
            result[target] = details.clone();
        }
    }
    if let Some(details) = result
        .get_mut("prompt_tokens_details")
        .and_then(Value::as_object_mut)
        && details.contains_key("cached_tokens")
    {
        details.insert("cached_tokens".into(), usage.cached_input_tokens.into());
    }
    if let Some(details) = result
        .get_mut("completion_tokens_details")
        .and_then(Value::as_object_mut)
    {
        if details
            .get("reasoning_tokens")
            .and_then(Value::as_u64)
            .is_some_and(|tokens| tokens <= usage.output_tokens)
        {
            details.insert(
                "reasoning_tokens".into(),
                usage.reasoning_output_tokens.into(),
            );
        } else {
            details.remove("reasoning_tokens");
        }
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
