//! Canonical event 到 OpenAI Responses SSE/JSON 的单一状态机。

use std::collections::BTreeMap;

use gateway_core::accounting::Usage;
use gateway_core::event::{
    CompactionOutput, ContentItem, ContentKind, EventSequenceValidator, FinishReason, GatewayEvent,
    ProviderEvent, ReasoningDelta, ResponseMeta, TextDelta, ToolCallDelta,
};
use gateway_protocol::openai::sse::{encode_sse_event, encode_sse_event_with_metadata};
use serde_json::{Map, Value, json};

use super::error::ResponseEncodeError;

const OPENAI_PROTOCOL: &str = "openai";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponseRepresentation {
    Undecided,
    Canonical,
    OpenAiWire,
}

#[derive(Debug)]
struct PendingWireEvent {
    event_type: Option<String>,
    data: Value,
    sse_id: Option<String>,
    sse_retry: Option<u64>,
}

/// Provider event 封套到 OpenAI Responses wire 的唯一表达边界。
///
/// Provider 已提供 OpenAI wire 时只下发该 wire；否则由 [`ResponsesCollector`]
/// 表达 canonical facts。同一响应不能在两种模式之间切换。
#[derive(Debug)]
pub struct OpenAiResponsesEncoder {
    canonical: ResponsesCollector,
    representation: ResponseRepresentation,
    upstream_response_id: Option<String>,
    gateway_response_id: Option<String>,
    wire_terminal: Option<Value>,
    pending_wire: Vec<PendingWireEvent>,
    canonical_completed: bool,
}

impl OpenAiResponsesEncoder {
    /// 创建响应 encoder。
    #[must_use]
    pub fn new(created_at: u64) -> Self {
        Self {
            canonical: ResponsesCollector::new(created_at),
            representation: ResponseRepresentation::Undecided,
            upstream_response_id: None,
            gateway_response_id: None,
            wire_terminal: None,
            pending_wire: Vec::new(),
            canonical_completed: false,
        }
    }

    /// 消费一个 Provider event，并返回 SSE frames。
    ///
    /// # Errors
    ///
    /// canonical 顺序无效、响应身份变化或表达模式混用时返回错误。
    pub fn push_sse(&mut self, event: &ProviderEvent) -> Result<Vec<String>, ResponseEncodeError> {
        self.push(event, WireEncoding::Sse)
    }

    /// 消费一个 Provider event，并返回 WebSocket JSON messages。
    ///
    /// # Errors
    ///
    /// 继承 [`Self::push_sse`] 的错误。
    pub fn push_websocket(
        &mut self,
        event: &ProviderEvent,
    ) -> Result<Vec<String>, ResponseEncodeError> {
        self.push(event, WireEncoding::WebSocket)
    }

    /// 返回是否已经观察到 canonical 或 wire 终态。
    #[must_use]
    pub fn is_completed(&self) -> bool {
        (self.wire_terminal.is_some() && self.canonical_completed)
            || self.canonical.final_response.is_some()
    }

    /// 返回 Core 冻结并已写入客户端事件的网关响应 ID。
    #[must_use]
    pub fn gateway_response_id(&self) -> Option<&str> {
        self.gateway_response_id.as_deref()
    }

    /// 校验完整响应并返回非流式 JSON。
    ///
    /// # Errors
    ///
    /// 流不完整或原生 wire 缺少终态 response object 时返回错误。
    pub fn finish(self) -> Result<Value, ResponseEncodeError> {
        match self.representation {
            ResponseRepresentation::OpenAiWire if self.canonical_completed => self
                .wire_terminal
                .ok_or(ResponseEncodeError::MissingWireTerminal),
            ResponseRepresentation::OpenAiWire => Err(ResponseEncodeError::MissingWireTerminal),
            ResponseRepresentation::Undecided | ResponseRepresentation::Canonical => {
                self.canonical.finish()
            }
        }
    }

    fn push(
        &mut self,
        event: &ProviderEvent,
        encoding: WireEncoding,
    ) -> Result<Vec<String>, ResponseEncodeError> {
        self.observe_identity(event)?;
        let openai_wire = event
            .wire_event()
            .filter(|wire| wire.protocol() == OPENAI_PROTOCOL);
        if openai_wire.is_some() {
            if self.representation == ResponseRepresentation::Canonical {
                return Err(ResponseEncodeError::MixedWireRepresentation);
            }
            self.representation = ResponseRepresentation::OpenAiWire;
        } else if event.has_canonical_facts()
            && self.representation == ResponseRepresentation::Undecided
        {
            self.representation = ResponseRepresentation::Canonical;
        }

        if self.representation == ResponseRepresentation::OpenAiWire {
            let Some(wire) = openai_wire else {
                return Ok(Vec::new());
            };
            self.pending_wire.push(PendingWireEvent {
                event_type: wire.event_type().map(ToOwned::to_owned),
                data: wire.data().clone(),
                sse_id: wire.sse_id().map(ToOwned::to_owned),
                sse_retry: wire.sse_retry(),
            });
            if self.gateway_response_id.is_none() {
                return Ok(Vec::new());
            }
            let pending = std::mem::take(&mut self.pending_wire);
            return pending
                .into_iter()
                .map(|pending| {
                    let PendingWireEvent {
                        event_type,
                        mut data,
                        sse_id,
                        sse_retry,
                    } = pending;
                    self.rewrite_wire_identity(&mut data);
                    let effective_type = event_type
                        .as_deref()
                        .or_else(|| data.get("type").and_then(Value::as_str));
                    if matches!(
                        effective_type,
                        Some("response.completed" | "response.incomplete")
                    ) {
                        self.wire_terminal = data.get("response").cloned();
                    }
                    Ok(match encoding {
                        WireEncoding::Sse => encode_sse_event_with_metadata(
                            event_type.as_deref().unwrap_or_default(),
                            &data.to_string(),
                            sse_id.as_deref(),
                            sse_retry,
                        ),
                        WireEncoding::WebSocket => data.to_string(),
                    })
                })
                .collect();
        }

        let mut canonical_frames = Vec::new();
        for fact in event.canonical_facts() {
            canonical_frames.extend(self.canonical.push(fact)?);
        }

        match encoding {
            WireEncoding::Sse => Ok(canonical_frames),
            WireEncoding::WebSocket => canonical_frames
                .into_iter()
                .map(|frame| {
                    frame
                        .lines()
                        .find_map(|line| line.strip_prefix("data: "))
                        .map(ToOwned::to_owned)
                        .ok_or(ResponseEncodeError::InvalidEventEncoding)
                })
                .collect(),
        }
    }

    fn observe_identity(&mut self, event: &ProviderEvent) -> Result<(), ResponseEncodeError> {
        for fact in event.canonical_facts() {
            let metadata = match fact {
                GatewayEvent::Started(metadata) => metadata,
                GatewayEvent::Completed(metadata) => {
                    self.canonical_completed = true;
                    metadata
                }
                _ => continue,
            };
            let Some(upstream) = metadata.upstream_response_id() else {
                continue;
            };
            if self
                .upstream_response_id
                .as_deref()
                .is_some_and(|current| current != upstream.as_str())
                || self
                    .gateway_response_id
                    .as_deref()
                    .is_some_and(|current| current != metadata.response_id())
            {
                return Err(ResponseEncodeError::WireIdentityChanged);
            }
            self.upstream_response_id = Some(upstream.as_str().to_owned());
            self.gateway_response_id = Some(metadata.response_id().to_owned());
        }
        Ok(())
    }

    fn rewrite_wire_identity(&self, data: &mut Value) {
        let (Some(upstream), Some(gateway)) = (
            self.upstream_response_id.as_deref(),
            self.gateway_response_id.as_deref(),
        ) else {
            return;
        };
        rewrite_response_identity(data, upstream, gateway);
    }
}

#[derive(Debug, Clone, Copy)]
enum WireEncoding {
    Sse,
    WebSocket,
}

fn rewrite_response_identity(value: &mut Value, upstream: &str, gateway: &str) {
    match value {
        Value::Array(values) => {
            for value in values {
                rewrite_response_identity(value, upstream, gateway);
            }
        }
        Value::Object(object) => {
            if let Some(Value::String(response_id)) = object.get_mut("response_id")
                && response_id == upstream
            {
                *response_id = gateway.to_owned();
            }
            if let Some(Value::Object(response)) = object.get_mut("response")
                && let Some(Value::String(response_id)) = response.get_mut("id")
                && response_id == upstream
            {
                *response_id = gateway.to_owned();
            }
            for value in object.values_mut() {
                rewrite_response_identity(value, upstream, gateway);
            }
        }
        _ => {}
    }
}

/// 同一 canonical event 流生成的两种 Responses 表达。
#[derive(Debug, Clone, PartialEq)]
pub struct CollectedResponses {
    response: Value,
    sse_frames: Vec<String>,
}

impl CollectedResponses {
    /// 返回非流式 Responses JSON。
    #[must_use]
    pub const fn response(&self) -> &Value {
        &self.response
    }

    /// 返回按事件边界编码的 SSE frame。
    #[must_use]
    pub fn sse_frames(&self) -> &[String] {
        &self.sse_frames
    }

    /// 拆分两种表达。
    #[must_use]
    pub fn into_parts(self) -> (Value, Vec<String>) {
        (self.response, self.sse_frames)
    }
}

/// OpenAI Responses event collector。
///
/// Collector 只做协议编码；它不拥有 commit、retry 或 Provider 生命周期。
#[derive(Debug)]
pub struct ResponsesCollector {
    validator: EventSequenceValidator,
    created_at: u64,
    started: Option<ResponseMeta>,
    usage: Option<Usage>,
    output_positions: BTreeMap<u32, usize>,
    output: Vec<OutputState>,
    final_response: Option<Value>,
}

impl ResponsesCollector {
    /// 创建 collector。`created_at` 是 logical request 冻结的 Unix 秒时间戳。
    #[must_use]
    pub fn new(created_at: u64) -> Self {
        Self {
            validator: EventSequenceValidator::new(),
            created_at,
            started: None,
            usage: None,
            output_positions: BTreeMap::new(),
            output: Vec::new(),
            final_response: None,
        }
    }

    /// 消费一个 canonical event，并返回本次新产生的 SSE frames。
    ///
    /// # Errors
    ///
    /// 事件乱序、元数据变化、重复 usage 或出现 OpenAI Responses 无法表达的
    /// canonical 语义时返回 [`ResponseEncodeError`]。
    pub fn push(&mut self, event: &GatewayEvent) -> Result<Vec<String>, ResponseEncodeError> {
        if self.final_response.is_some() {
            return Err(ResponseEncodeError::AlreadyCompleted);
        }
        self.validator.observe(event)?;

        match event {
            GatewayEvent::Started(meta) => self.started(meta),
            GatewayEvent::ContentAdded(item) => self.content_added(item),
            GatewayEvent::TextDelta(delta) => self.text_delta(delta),
            GatewayEvent::ReasoningDelta(delta) => self.reasoning_delta(delta),
            GatewayEvent::ToolCallDelta(delta) => self.tool_call_delta(delta),
            GatewayEvent::CompactionOutput(output) => self.compaction_output(output),
            GatewayEvent::Usage(usage) => self.observe_usage(usage),
            GatewayEvent::CalculatedCost(_) | GatewayEvent::ProviderCost(_) => Ok(Vec::new()),
            GatewayEvent::Completed(meta) => self.completed(meta),
            _ => Err(ResponseEncodeError::UnsupportedEvent),
        }
    }

    /// 消费一个 canonical event，并返回 Responses WebSocket 文本消息。
    ///
    /// WebSocket 与 SSE 共用同一状态机和事件 JSON；这里只移除 SSE framing，
    /// 避免两个下游 transport 各自维护一套协议投影。
    ///
    /// # Errors
    ///
    /// 继承 [`Self::push`] 的 canonical 编码错误；内部 SSE framing 若不满足编码器
    /// 自身的不变量则返回 [`ResponseEncodeError::InvalidEventEncoding`]。
    pub fn push_websocket_events(
        &mut self,
        event: &GatewayEvent,
    ) -> Result<Vec<String>, ResponseEncodeError> {
        self.push(event)?
            .into_iter()
            .map(|frame| {
                frame
                    .lines()
                    .find_map(|line| line.strip_prefix("data: "))
                    .map(ToOwned::to_owned)
                    .ok_or(ResponseEncodeError::InvalidEventEncoding)
            })
            .collect()
    }

    /// 校验终态并返回非流式 JSON。
    ///
    /// # Errors
    ///
    /// Stream 未正常完成时返回事件顺序错误。
    pub fn finish(self) -> Result<Value, ResponseEncodeError> {
        self.validator.finish()?;
        self.final_response
            .ok_or_else(|| gateway_core::event::EventSequenceError::MissingCompleted.into())
    }

    /// 一次性收集完整事件序列，同时保留 stateful collector 的相同实现路径。
    ///
    /// # Errors
    ///
    /// 任意事件无法编码或序列不完整时返回错误。
    pub fn collect<'a>(
        created_at: u64,
        events: impl IntoIterator<Item = &'a GatewayEvent>,
    ) -> Result<CollectedResponses, ResponseEncodeError> {
        let mut collector = Self::new(created_at);
        let mut sse_frames = Vec::new();
        for event in events {
            sse_frames.extend(collector.push(event)?);
        }
        let response = collector.finish()?;
        Ok(CollectedResponses {
            response,
            sse_frames,
        })
    }

    fn started(&mut self, meta: &ResponseMeta) -> Result<Vec<String>, ResponseEncodeError> {
        self.started = Some(meta.clone());
        let response = response_base(
            meta,
            self.created_at,
            "in_progress",
            Value::Null,
            Vec::new(),
            Value::Null,
        );
        Ok(vec![sse_frame(
            "response.created",
            json!({
                "type": "response.created",
                "response": response,
            }),
        )])
    }

    fn content_added(&mut self, item: &ContentItem) -> Result<Vec<String>, ResponseEncodeError> {
        let started = self
            .started
            .as_ref()
            .ok_or(gateway_core::event::EventSequenceError::MissingStarted)?;
        let output_index = self.output.len();
        let id = deterministic_output_id(started.response_id(), item.index(), item.kind());
        let state = match item.kind() {
            ContentKind::Text => OutputState::Text {
                id,
                text: String::new(),
            },
            ContentKind::Reasoning => OutputState::Reasoning {
                id,
                text: String::new(),
            },
            ContentKind::ToolCall => OutputState::ToolCall {
                canonical_index: item.index(),
                id,
                call_id: None,
                name: None,
                arguments: String::new(),
                published: false,
            },
            ContentKind::Image => {
                return Err(ResponseEncodeError::UnsupportedContentKind { kind: "image" });
            }
            ContentKind::Audio => {
                return Err(ResponseEncodeError::UnsupportedContentKind { kind: "audio" });
            }
            _ => {
                return Err(ResponseEncodeError::UnsupportedContentKind { kind: "unknown" });
            }
        };
        self.output_positions.insert(item.index(), output_index);
        let frames = state.added_frames(output_index);
        self.output.push(state);
        Ok(frames)
    }

    fn text_delta(&mut self, delta: &TextDelta) -> Result<Vec<String>, ResponseEncodeError> {
        let output_index = self.position(delta.content_index)?;
        let OutputState::Text { id, text, .. } = &mut self.output[output_index] else {
            return Err(
                gateway_core::event::EventSequenceError::InvalidDeltaTarget {
                    index: delta.content_index,
                }
                .into(),
            );
        };
        text.push_str(&delta.text);
        Ok(vec![sse_frame(
            "response.output_text.delta",
            json!({
                "type": "response.output_text.delta",
                "item_id": id,
                "output_index": output_index,
                "content_index": 0,
                "delta": &delta.text,
            }),
        )])
    }

    fn reasoning_delta(
        &mut self,
        delta: &ReasoningDelta,
    ) -> Result<Vec<String>, ResponseEncodeError> {
        let output_index = self.position(delta.content_index)?;
        let OutputState::Reasoning { id, text, .. } = &mut self.output[output_index] else {
            return Err(
                gateway_core::event::EventSequenceError::InvalidDeltaTarget {
                    index: delta.content_index,
                }
                .into(),
            );
        };
        text.push_str(&delta.text);
        Ok(vec![sse_frame(
            "response.reasoning_summary_text.delta",
            json!({
                "type": "response.reasoning_summary_text.delta",
                "item_id": id,
                "output_index": output_index,
                "summary_index": 0,
                "delta": &delta.text,
            }),
        )])
    }

    fn tool_call_delta(
        &mut self,
        delta: &ToolCallDelta,
    ) -> Result<Vec<String>, ResponseEncodeError> {
        let output_index = self.position(delta.content_index)?;
        let OutputState::ToolCall {
            canonical_index,
            id,
            call_id,
            name,
            arguments,
            published,
        } = &mut self.output[output_index]
        else {
            return Err(
                gateway_core::event::EventSequenceError::InvalidDeltaTarget {
                    index: delta.content_index,
                }
                .into(),
            );
        };
        if delta.call_id.is_empty() {
            return Err(ResponseEncodeError::ToolIdentityChanged {
                index: *canonical_index,
            });
        }
        match call_id {
            Some(existing) if existing != &delta.call_id => {
                return Err(ResponseEncodeError::ToolIdentityChanged {
                    index: *canonical_index,
                });
            }
            None => *call_id = Some(delta.call_id.clone()),
            Some(_) => {}
        }
        if let Some(delta_name) = &delta.name {
            if delta_name.is_empty() {
                return Err(ResponseEncodeError::MissingToolName {
                    index: *canonical_index,
                });
            }
            match name {
                Some(existing) if existing != delta_name => {
                    return Err(ResponseEncodeError::ToolIdentityChanged {
                        index: *canonical_index,
                    });
                }
                None => *name = Some(delta_name.clone()),
                Some(_) => {}
            }
        }

        let mut frames = Vec::new();
        if !*published && name.is_some() {
            frames.push(sse_frame(
                "response.output_item.added",
                json!({
                    "type": "response.output_item.added",
                    "output_index": output_index,
                    "item": tool_item(id, call_id.as_deref(), name.as_deref(), "", "in_progress"),
                }),
            ));
            *published = true;
            if !arguments.is_empty() {
                frames.push(tool_arguments_delta_frame(id, output_index, arguments));
            }
        }
        arguments.push_str(&delta.arguments_delta);
        if *published && !delta.arguments_delta.is_empty() {
            frames.push(tool_arguments_delta_frame(
                id,
                output_index,
                &delta.arguments_delta,
            ));
        }
        Ok(frames)
    }

    fn compaction_output(
        &mut self,
        output: &CompactionOutput,
    ) -> Result<Vec<String>, ResponseEncodeError> {
        let output_index = self.output.len();
        let state = OutputState::Compaction {
            encrypted_content: output.summary().as_str().to_owned(),
        };
        let item = state.completed_item()?;
        self.output.push(state);
        Ok(vec![sse_frame(
            "response.output_item.done",
            json!({
                "type": "response.output_item.done",
                "output_index": output_index,
                "item": item,
            }),
        )])
    }

    fn observe_usage(&mut self, usage: &Usage) -> Result<Vec<String>, ResponseEncodeError> {
        if self.usage.is_some() {
            return Err(ResponseEncodeError::DuplicateUsage);
        }
        if usage.input_tokens.is_none()
            || usage.output_tokens.is_none()
            || usage.total_tokens.is_none()
        {
            return Err(ResponseEncodeError::IncompleteUsage);
        }
        self.usage = Some(usage.clone());
        Ok(Vec::new())
    }

    fn completed(&mut self, meta: &ResponseMeta) -> Result<Vec<String>, ResponseEncodeError> {
        let started = self
            .started
            .as_ref()
            .ok_or(gateway_core::event::EventSequenceError::MissingStarted)?;
        if started.response_id() != meta.response_id() || started.model() != meta.model() {
            return Err(ResponseEncodeError::MetadataChanged);
        }

        let mut frames = Vec::new();
        for (output_index, state) in self.output.iter().enumerate() {
            frames.extend(state.done_frames(output_index)?);
        }
        let output = self
            .output
            .iter()
            .map(OutputState::completed_item)
            .collect::<Result<Vec<_>, _>>()?;
        let (status, terminal_event, incomplete_details) = terminal_status(meta.finish_reason());
        let usage = self.usage.as_ref().map_or(Ok(Value::Null), usage_json)?;
        let response = response_base(
            meta,
            self.created_at,
            status,
            incomplete_details,
            output,
            usage,
        );
        frames.push(sse_frame(
            terminal_event,
            json!({
                "type": terminal_event,
                "response": &response,
            }),
        ));
        self.final_response = Some(response);
        Ok(frames)
    }

    fn position(&self, canonical_index: u32) -> Result<usize, ResponseEncodeError> {
        self.output_positions
            .get(&canonical_index)
            .copied()
            .ok_or_else(|| {
                gateway_core::event::EventSequenceError::InvalidDeltaTarget {
                    index: canonical_index,
                }
                .into()
            })
    }
}

#[derive(Debug)]
enum OutputState {
    Text {
        id: String,
        text: String,
    },
    Reasoning {
        id: String,
        text: String,
    },
    ToolCall {
        canonical_index: u32,
        id: String,
        call_id: Option<String>,
        name: Option<String>,
        arguments: String,
        published: bool,
    },
    Compaction {
        encrypted_content: String,
    },
}

impl OutputState {
    fn added_frames(&self, output_index: usize) -> Vec<String> {
        match self {
            Self::Text { id, .. } => vec![
                sse_frame(
                    "response.output_item.added",
                    json!({
                        "type": "response.output_item.added",
                        "output_index": output_index,
                        "item": message_item(id, "in_progress", Vec::new()),
                    }),
                ),
                sse_frame(
                    "response.content_part.added",
                    json!({
                        "type": "response.content_part.added",
                        "item_id": id,
                        "output_index": output_index,
                        "content_index": 0,
                        "part": output_text_part(""),
                    }),
                ),
            ],
            Self::Reasoning { id, .. } => vec![
                sse_frame(
                    "response.output_item.added",
                    json!({
                        "type": "response.output_item.added",
                        "output_index": output_index,
                        "item": reasoning_item_with_summary(id, Vec::new()),
                    }),
                ),
                sse_frame(
                    "response.reasoning_summary_part.added",
                    json!({
                        "type": "response.reasoning_summary_part.added",
                        "item_id": id,
                        "output_index": output_index,
                        "summary_index": 0,
                        "part": summary_text_part(""),
                    }),
                ),
            ],
            Self::ToolCall { .. } => Vec::new(),
            Self::Compaction { .. } => Vec::new(),
        }
    }

    fn done_frames(&self, output_index: usize) -> Result<Vec<String>, ResponseEncodeError> {
        match self {
            Self::Text { id, text, .. } => Ok(vec![
                sse_frame(
                    "response.output_text.done",
                    json!({
                        "type": "response.output_text.done",
                        "item_id": id,
                        "output_index": output_index,
                        "content_index": 0,
                        "text": text,
                    }),
                ),
                sse_frame(
                    "response.content_part.done",
                    json!({
                        "type": "response.content_part.done",
                        "item_id": id,
                        "output_index": output_index,
                        "content_index": 0,
                        "part": output_text_part(text),
                    }),
                ),
                sse_frame(
                    "response.output_item.done",
                    json!({
                        "type": "response.output_item.done",
                        "output_index": output_index,
                        "item": text_item(id, text, "completed"),
                    }),
                ),
            ]),
            Self::Reasoning { id, text, .. } => Ok(vec![
                sse_frame(
                    "response.reasoning_summary_text.done",
                    json!({
                        "type": "response.reasoning_summary_text.done",
                        "item_id": id,
                        "output_index": output_index,
                        "summary_index": 0,
                        "text": text,
                    }),
                ),
                sse_frame(
                    "response.reasoning_summary_part.done",
                    json!({
                        "type": "response.reasoning_summary_part.done",
                        "item_id": id,
                        "output_index": output_index,
                        "summary_index": 0,
                        "part": summary_text_part(text),
                    }),
                ),
                sse_frame(
                    "response.output_item.done",
                    json!({
                        "type": "response.output_item.done",
                        "output_index": output_index,
                        "item": reasoning_item(id, text),
                    }),
                ),
            ]),
            Self::ToolCall {
                canonical_index,
                id,
                call_id,
                name,
                arguments,
                published,
            } => {
                let name = name
                    .as_deref()
                    .ok_or(ResponseEncodeError::MissingToolName {
                        index: *canonical_index,
                    })?;
                let call_id =
                    call_id
                        .as_deref()
                        .ok_or(ResponseEncodeError::ToolIdentityChanged {
                            index: *canonical_index,
                        })?;
                let mut frames = Vec::new();
                if !published {
                    frames.push(sse_frame(
                        "response.output_item.added",
                        json!({
                            "type": "response.output_item.added",
                            "output_index": output_index,
                            "item": tool_item(id, Some(call_id), Some(name), "", "in_progress"),
                        }),
                    ));
                    if !arguments.is_empty() {
                        frames.push(tool_arguments_delta_frame(id, output_index, arguments));
                    }
                }
                frames.push(sse_frame(
                    "response.function_call_arguments.done",
                    json!({
                        "type": "response.function_call_arguments.done",
                        "item_id": id,
                        "output_index": output_index,
                        "arguments": arguments,
                    }),
                ));
                frames.push(sse_frame(
                    "response.output_item.done",
                    json!({
                        "type": "response.output_item.done",
                        "output_index": output_index,
                        "item": tool_item(
                            id,
                            Some(call_id),
                            Some(name),
                            arguments,
                            "completed",
                        ),
                    }),
                ));
                Ok(frames)
            }
            Self::Compaction { .. } => Ok(Vec::new()),
        }
    }

    fn completed_item(&self) -> Result<Value, ResponseEncodeError> {
        match self {
            Self::Text { id, text, .. } => Ok(text_item(id, text, "completed")),
            Self::Reasoning { id, text, .. } => Ok(reasoning_item(id, text)),
            Self::ToolCall {
                canonical_index,
                id,
                call_id,
                name,
                arguments,
                ..
            } => Ok(tool_item(
                id,
                Some(
                    call_id
                        .as_deref()
                        .ok_or(ResponseEncodeError::ToolIdentityChanged {
                            index: *canonical_index,
                        })?,
                ),
                Some(
                    name.as_deref()
                        .ok_or(ResponseEncodeError::MissingToolName {
                            index: *canonical_index,
                        })?,
                ),
                arguments,
                "completed",
            )),
            Self::Compaction { encrypted_content } => Ok(compaction_item(encrypted_content)),
        }
    }
}

fn terminal_status(finish_reason: Option<FinishReason>) -> (&'static str, &'static str, Value) {
    match finish_reason {
        Some(FinishReason::Length) => (
            "incomplete",
            "response.incomplete",
            json!({ "reason": "max_output_tokens" }),
        ),
        Some(FinishReason::ContentFilter) => (
            "incomplete",
            "response.incomplete",
            json!({ "reason": "content_filter" }),
        ),
        _ => ("completed", "response.completed", Value::Null),
    }
}

fn response_base(
    meta: &ResponseMeta,
    created_at: u64,
    status: &str,
    incomplete_details: Value,
    output: Vec<Value>,
    usage: Value,
) -> Value {
    json!({
        "id": meta.response_id(),
        "object": "response",
        "created_at": created_at,
        "status": status,
        "error": null,
        "incomplete_details": incomplete_details,
        "model": meta.model(),
        "output": output,
        "usage": usage,
    })
}

fn usage_json(usage: &Usage) -> Result<Value, ResponseEncodeError> {
    let input_tokens = usage
        .input_tokens
        .ok_or(ResponseEncodeError::IncompleteUsage)?;
    let output_tokens = usage
        .output_tokens
        .ok_or(ResponseEncodeError::IncompleteUsage)?;
    let total_tokens = usage
        .total_tokens
        .ok_or(ResponseEncodeError::IncompleteUsage)?;
    let mut object = Map::new();
    object.insert("input_tokens".to_owned(), json!(input_tokens));
    object.insert("output_tokens".to_owned(), json!(output_tokens));
    object.insert("total_tokens".to_owned(), json!(total_tokens));
    let mut input_details = Map::new();
    if let Some(cached_tokens) = usage.cached_tokens {
        input_details.insert("cached_tokens".to_owned(), json!(cached_tokens));
    }
    if let Some(cache_write_tokens) = usage.cache_write_tokens {
        input_details.insert("cache_write_tokens".to_owned(), json!(cache_write_tokens));
    }
    if !input_details.is_empty() {
        object.insert(
            "input_tokens_details".to_owned(),
            Value::Object(input_details),
        );
    }
    if let Some(reasoning_tokens) = usage.reasoning_tokens {
        object.insert(
            "output_tokens_details".to_owned(),
            json!({ "reasoning_tokens": reasoning_tokens }),
        );
    }
    Ok(Value::Object(object))
}

fn text_item(id: &str, text: &str, status: &str) -> Value {
    message_item(id, status, vec![output_text_part(text)])
}

fn message_item(id: &str, status: &str, content: Vec<Value>) -> Value {
    json!({
        "id": id,
        "type": "message",
        "status": status,
        "role": "assistant",
        "content": content,
    })
}

fn output_text_part(text: &str) -> Value {
    json!({
        "type": "output_text",
        "text": text,
        "annotations": [],
    })
}

fn reasoning_item(id: &str, text: &str) -> Value {
    reasoning_item_with_summary(id, vec![summary_text_part(text)])
}

fn reasoning_item_with_summary(id: &str, summary: Vec<Value>) -> Value {
    json!({
        "id": id,
        "type": "reasoning",
        "summary": summary,
    })
}

fn compaction_item(encrypted_content: &str) -> Value {
    json!({
        "type": "compaction",
        "encrypted_content": encrypted_content,
    })
}

fn summary_text_part(text: &str) -> Value {
    json!({
        "type": "summary_text",
        "text": text,
    })
}

fn tool_item(
    id: &str,
    call_id: Option<&str>,
    name: Option<&str>,
    arguments: &str,
    status: &str,
) -> Value {
    json!({
        "id": id,
        "type": "function_call",
        "status": status,
        "call_id": call_id,
        "name": name,
        "arguments": arguments,
    })
}

fn tool_arguments_delta_frame(id: &str, output_index: usize, delta: &str) -> String {
    sse_frame(
        "response.function_call_arguments.delta",
        json!({
            "type": "response.function_call_arguments.delta",
            "item_id": id,
            "output_index": output_index,
            "delta": delta,
        }),
    )
}

fn sse_frame(event: &str, payload: Value) -> String {
    encode_sse_event(event, &payload.to_string())
}

fn deterministic_output_id(response_id: &str, index: u32, kind: ContentKind) -> String {
    let prefix = match kind {
        ContentKind::Text => "msg",
        ContentKind::Reasoning => "rs",
        ContentKind::ToolCall => "fc",
        ContentKind::Image => "img",
        ContentKind::Audio => "audio",
        _ => "item",
    };
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in response_id
        .bytes()
        .chain(index.to_be_bytes())
        .chain(kind_tag(kind))
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{prefix}_{hash:016x}")
}

fn kind_tag(kind: ContentKind) -> [u8; 1] {
    let tag = match kind {
        ContentKind::Text => 1,
        ContentKind::Reasoning => 2,
        ContentKind::ToolCall => 3,
        ContentKind::Image => 4,
        ContentKind::Audio => 5,
        _ => u8::MAX,
    };
    [tag]
}
