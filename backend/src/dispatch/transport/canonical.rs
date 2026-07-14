//! Responses SSE 原始帧到规范事件的唯一解码入口。

use bytes::Bytes;
use serde_json::Value;

use crate::upstream::openai::protocol::{
    events::{TokenUsage, extract_usage},
    responses::{
        CollectedResponse, ResponsesSseFailure, StreamCommitPolicy,
        reconvert_responses_sse_event_tuple_values,
    },
    sse::{
        MAX_SSE_EVENT_BUFFER_BYTES, SseError, encode_sse_event, parse_sse_events, sse_frame_end,
        sse_frame_is_done,
    },
};

/// 将 complete transport 的原始 SSE 归一化为类型化终态事实。
pub(in crate::dispatch) fn normalize_complete_response(
    body: &str,
    tuple_schema: Option<&Value>,
) -> Result<CollectedResponse, SseError> {
    let events = parse_sse_events(body)?;
    let mut output_text = String::new();
    let mut output_items = Vec::new();
    let mut terminal_response = None;
    let mut terminal_incomplete = false;
    let mut failed_response = None;

    for event in events {
        let Ok(mut value) = serde_json::from_str::<Value>(&event.data) else {
            continue;
        };
        if let Some(tuple_schema) = tuple_schema {
            value = reconvert_responses_sse_event_tuple_values(
                event.event.as_deref(),
                value,
                tuple_schema,
            );
        }
        match event.event.as_deref() {
            Some("response.output_text.delta") => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    output_text.push_str(delta);
                }
            }
            Some("response.output_item.done") => {
                if let Some(item) = value.get("item") {
                    output_items.push(item.clone());
                }
            }
            Some("response.completed") => {
                terminal_response = value.get("response").cloned();
                terminal_incomplete = false;
            }
            Some("response.incomplete") => {
                terminal_response = value.get("response").cloned();
                terminal_incomplete = true;
            }
            Some(event_name @ ("error" | "response.failed")) if failed_response.is_none() => {
                failed_response = Some(ResponsesSseFailure::from_event(event_name, &value));
            }
            _ => {}
        }
    }

    if let Some(failure) = failed_response {
        return Ok(CollectedResponse::Failed(failure));
    }
    let Some(response) = terminal_response else {
        return Ok(CollectedResponse::MissingCompleted);
    };
    if terminal_incomplete {
        return Ok(CollectedResponse::Incomplete(response));
    }
    if complete_response_is_empty(&response, &output_text, &output_items) {
        return Ok(CollectedResponse::Empty);
    }
    Ok(CollectedResponse::Completed(response))
}

fn complete_response_is_empty(response: &Value, output_text: &str, output_items: &[Value]) -> bool {
    if !output_text.trim().is_empty() || !output_items.is_empty() {
        return false;
    }
    if response.get("status").and_then(Value::as_str) == Some("incomplete") {
        return false;
    }
    response
        .pointer("/usage/output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or_default()
        == 0
}

/// 一次解码后可同时供 HTTP SSE、Responses WebSocket 与生命周期控制器消费的事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalResponseEvent {
    event_type: Option<String>,
    data: Value,
}

impl CanonicalResponseEvent {
    pub(crate) fn event_type(&self) -> Option<&str> {
        self.event_type.as_deref()
    }

    pub(crate) fn data(&self) -> &Value {
        &self.data
    }

    pub(in crate::dispatch) fn response_id(&self) -> Option<&str> {
        self.data
            .pointer("/response/id")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
    }

    pub(in crate::dispatch) fn usage(&self) -> Option<TokenUsage> {
        self.data
            .get("response")
            .and_then(extract_usage)
            .or_else(|| extract_usage(&self.data))
    }

    pub(in crate::dispatch) fn has_first_output(&self) -> bool {
        match self.event_type() {
            Some(
                "response.output_text.delta"
                | "response.reasoning_summary_text.delta"
                | "response.reasoning_text.delta"
                | "response.function_call_arguments.delta"
                | "response.custom_tool_call_input.delta",
            ) => self
                .data
                .get("delta")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty()),
            Some("response.output_text.done") => self
                .data
                .get("text")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty()),
            Some("response.function_call_arguments.done") => self
                .data
                .get("arguments")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty()),
            Some("response.output_item.done") => {
                self.data.get("item").is_some_and(Value::is_object)
            }
            _ => false,
        }
    }

    pub(in crate::dispatch) fn terminal(&self) -> Option<CanonicalStreamTerminal> {
        match self.event_type() {
            Some("response.completed") => self
                .data
                .get("response")
                .filter(|response| response.is_object())
                .cloned()
                .map(CanonicalStreamTerminal::Completed),
            Some("response.incomplete") => self
                .data
                .get("response")
                .filter(|response| response.is_object())
                .cloned()
                .map(CanonicalStreamTerminal::Incomplete),
            Some(event @ ("response.failed" | "error")) => Some(CanonicalStreamTerminal::Failed(
                ResponsesSseFailure::from_event(event, &self.data),
            )),
            _ => None,
        }
    }

    pub(in crate::dispatch) fn proxy_failure(data: Value) -> Self {
        Self {
            event_type: Some("response.failed".to_string()),
            data,
        }
    }
}

/// 上游明确发出的业务终态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::dispatch) enum CanonicalStreamTerminal {
    Completed(Value),
    Incomplete(Value),
    Failed(ResponsesSseFailure),
}

/// 已解码事件与对应下游字节的单个 fanout 单元。
#[derive(Debug, Clone)]
pub(crate) struct CanonicalResponseChunk {
    bytes: Bytes,
    events: Vec<CanonicalResponseEvent>,
}

impl CanonicalResponseChunk {
    pub(in crate::dispatch) fn new(bytes: Bytes, events: Vec<CanonicalResponseEvent>) -> Self {
        Self { bytes, events }
    }

    pub(in crate::dispatch) fn bytes(&self) -> &Bytes {
        &self.bytes
    }

    pub(in crate::dispatch) fn into_parts(self) -> (Bytes, Vec<CanonicalResponseEvent>) {
        (self.bytes, self.events)
    }

    pub(crate) fn events(&self) -> &[CanonicalResponseEvent] {
        &self.events
    }
}

/// 单次输入产生的所有下游 fanout 单元。
#[derive(Debug)]
pub(in crate::dispatch) struct CanonicalStreamBatch {
    pub chunks: Vec<CanonicalResponseChunk>,
}

impl CanonicalStreamBatch {
    pub(in crate::dispatch) fn append(&mut self, mut other: Self) {
        self.chunks.append(&mut other.chunks);
    }

    /// 只把真实输出前出现的失败视为可在 attempt 阶段处理的失败。
    /// 同一上游 chunk 可能同时包含输出与终态，事件顺序不能被网络分块抹掉。
    pub(in crate::dispatch) fn first_failure(&self) -> Option<ResponsesSseFailure> {
        for event in self.chunks.iter().flat_map(|chunk| chunk.events()) {
            if event.has_first_output() {
                return None;
            }
            if let Some(terminal) = event.terminal() {
                return match terminal {
                    CanonicalStreamTerminal::Failed(failure) => Some(failure),
                    CanonicalStreamTerminal::Completed(_)
                    | CanonicalStreamTerminal::Incomplete(_) => None,
                };
            }
        }
        None
    }
}

/// 原始 SSE 在整个 attempt/live 生命周期中唯一持有的增量 decoder。
pub(in crate::dispatch) struct CanonicalStreamDecoder {
    tuple_schema: Option<Value>,
    pending: Vec<u8>,
    forwardable_frame_seen: bool,
    first_output_seen: bool,
    terminal: Option<CanonicalStreamTerminal>,
    transport_done_seen: bool,
    completed_usage: Option<TokenUsage>,
    fallback_usage: Option<TokenUsage>,
    last_response_id: Option<String>,
}

impl CanonicalStreamDecoder {
    pub(in crate::dispatch) fn new(tuple_schema: Option<Value>) -> Self {
        Self {
            tuple_schema,
            pending: Vec::new(),
            forwardable_frame_seen: false,
            first_output_seen: false,
            terminal: None,
            transport_done_seen: false,
            completed_usage: None,
            fallback_usage: None,
            last_response_id: None,
        }
    }

    pub(in crate::dispatch) fn push(
        &mut self,
        chunk: Bytes,
    ) -> Result<CanonicalStreamBatch, SseError> {
        self.pending.extend_from_slice(&chunk);
        let mut transformed_chunks = Vec::new();
        let mut consumed = 0usize;

        while let Some(frame_len) = sse_frame_end(&self.pending[consumed..]) {
            let end = consumed.saturating_add(frame_len);
            let frame = self.pending[consumed..end].to_vec();
            if let Some((transformed, events)) = self.normalize_frame(&frame)? {
                transformed_chunks.push(CanonicalResponseChunk::new(transformed, events));
            }
            consumed = end;
            if self.terminal.is_some() || self.transport_done_seen {
                consumed = self.pending.len();
                break;
            }
        }

        if consumed != 0 {
            self.pending.drain(..consumed);
        }
        if self.pending.len() > MAX_SSE_EVENT_BUFFER_BYTES {
            return Err(SseError::BufferExceeded {
                max_bytes: MAX_SSE_EVENT_BUFFER_BYTES,
            });
        }

        Ok(CanonicalStreamBatch {
            chunks: transformed_chunks,
        })
    }

    pub(in crate::dispatch) fn finish(&mut self) -> Result<CanonicalStreamBatch, SseError> {
        if self.pending.is_empty() {
            return Ok(CanonicalStreamBatch { chunks: Vec::new() });
        }
        let frame = std::mem::take(&mut self.pending);
        let Some((transformed, events)) = self.normalize_frame(&frame)? else {
            return Ok(CanonicalStreamBatch { chunks: Vec::new() });
        };
        Ok(CanonicalStreamBatch {
            chunks: vec![CanonicalResponseChunk::new(transformed, events)],
        })
    }

    pub(in crate::dispatch) fn take_terminal(&mut self) -> Option<CanonicalStreamTerminal> {
        self.terminal.take()
    }

    pub(in crate::dispatch) fn take_transport_done(&mut self) -> bool {
        std::mem::take(&mut self.transport_done_seen)
    }

    pub(in crate::dispatch) fn transport_done_seen(&self) -> bool {
        self.transport_done_seen
    }

    pub(in crate::dispatch) fn first_output_seen(&self) -> bool {
        self.first_output_seen
    }

    pub(in crate::dispatch) fn commit_boundary_reached(&self, policy: StreamCommitPolicy) -> bool {
        match policy {
            StreamCommitPolicy::FirstForwardableEvent => self.forwardable_frame_seen,
            StreamCommitPolicy::UntilOutputOrTerminal => {
                self.first_output_seen || self.terminal.is_some()
            }
        }
    }

    pub(in crate::dispatch) fn usage(&self) -> Option<TokenUsage> {
        self.completed_usage.or(self.fallback_usage)
    }

    pub(in crate::dispatch) fn last_response_id(&self) -> Option<&str> {
        self.last_response_id.as_deref()
    }

    fn normalize_frame(
        &mut self,
        frame: &[u8],
    ) -> Result<Option<(Bytes, Vec<CanonicalResponseEvent>)>, SseError> {
        let frame_text =
            std::str::from_utf8(frame).map_err(|error| SseError::ParseError(error.to_string()))?;
        if sse_frame_is_done(frame_text) {
            self.transport_done_seen = true;
            return Ok(None);
        }
        let events = parse_sse_events(frame_text)?;
        let mut canonical_events = Vec::new();
        let mut transformed = String::new();

        for event in events {
            let Ok(mut data) = serde_json::from_str::<Value>(&event.data) else {
                continue;
            };
            let event_type = event.event.or_else(|| {
                data.get("type")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            });
            if let Some(tuple_schema) = self.tuple_schema.as_ref() {
                data = reconvert_responses_sse_event_tuple_values(
                    event_type.as_deref(),
                    data,
                    tuple_schema,
                );
                transformed.push_str(&encode_sse_event(
                    event_type.as_deref().unwrap_or_default(),
                    &data.to_string(),
                ));
            }
            let canonical = CanonicalResponseEvent { event_type, data };
            self.observe(&canonical);
            canonical_events.push(canonical);
        }

        self.forwardable_frame_seen = true;

        Ok(Some((
            if self.tuple_schema.is_some() && !transformed.is_empty() {
                Bytes::from(transformed)
            } else {
                Bytes::copy_from_slice(frame)
            },
            canonical_events,
        )))
    }

    fn observe(&mut self, event: &CanonicalResponseEvent) {
        self.first_output_seen |= event.has_first_output();
        if let Some(response_id) = event.response_id() {
            self.last_response_id = Some(response_id.to_string());
        }
        if let Some(usage) = event.usage() {
            if event.event_type() == Some("response.completed") {
                self.completed_usage = Some(usage);
            } else {
                self.fallback_usage = Some(usage);
            }
        }
        if self.terminal.is_none() {
            self.terminal = event.terminal();
        }
    }
}
