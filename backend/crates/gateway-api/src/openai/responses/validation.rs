//! 插件改写后的 OpenAI Responses 投递协议复核

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

use gateway_core::event::{GatewayEvent, ProtocolWireEvent, ProviderEvent};
use gateway_protocol::openai::sse::{parse_sse_events, sse_frame_is_done};
use serde_json::{Map, Value};

const MAX_TRACKED_OUTPUT_ITEMS: usize = 4_096;
const MAX_IDENTIFIER_BYTES: usize = 1_024;

/// 插件输出不能安全投递为 OpenAI Responses 协议
#[derive(Debug, Clone, Copy)]
pub(super) struct ResponseValidationError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalKind {
    Completed,
    Incomplete,
    Failed,
    Done,
}

#[derive(Debug, Clone, Default)]
struct ExpectedResponseFacts {
    response_id: Option<String>,
    terminal: Option<TerminalKind>,
    invalid: bool,
}

/// 仅保存最终协议复核所需的响应 ID 与终态类别，不缓存流正文
#[derive(Clone, Default)]
pub(super) struct ResponseValidationFacts(Arc<Mutex<ExpectedResponseFacts>>);

impl ResponseValidationFacts {
    pub(super) fn observe_event(&self, event: &ProviderEvent) {
        let mut facts = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(wire) = event
            .middleware_origin_wire()
            .or_else(|| event.wire_event())
            .filter(|wire| wire.protocol() == "openai")
        {
            facts.observe_wire(wire);
        }
        for fact in event.canonical_facts() {
            if let GatewayEvent::Started(metadata) = fact {
                facts.observe_response_id(metadata.response_id());
            }
        }
    }

    pub(super) fn observe_buffered(&self, events: &[ProviderEvent], original: &[u8]) {
        for event in events {
            self.observe_event(event);
        }
        let Ok(value) = serde_json::from_slice::<Value>(original) else {
            return;
        };
        let mut facts = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Ok(response_id) = required_identifier(value.get("id")) {
            facts.observe_response_id(response_id);
        }
        if let Some(terminal) = response_terminal(&value) {
            facts.observe_terminal(terminal);
        }
    }

    fn snapshot(&self) -> ExpectedResponseFacts {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(super) fn response_id(&self) -> Option<String> {
        self.snapshot().response_id
    }
}

impl ExpectedResponseFacts {
    fn observe_wire(&mut self, wire: &ProtocolWireEvent) {
        let event_type = wire
            .event_type()
            .or_else(|| wire.data().get("type").and_then(Value::as_str));
        self.observe_wire_value(event_type, wire.data());
        if !wire.has_json_data()
            && let Some(raw) = wire.raw_sse_frame()
            && let Ok(ParsedFrame::Event { event_type, value }) = ParsedFrame::parse(raw)
        {
            self.observe_wire_value(Some(&event_type), &value);
        }
    }

    fn observe_wire_value(&mut self, event_type: Option<&str>, value: &Value) {
        if event_type == Some("response.created")
            && let Some(response_id) = value
                .get("response")
                .and_then(|response| response.get("id"))
                .and_then(Value::as_str)
        {
            self.observe_response_id(response_id);
        }
        if let Some(terminal) = terminal_kind(event_type) {
            self.observe_terminal(terminal);
            let response = value.get("response").unwrap_or(value);
            if let Some(response_id) = response.get("id").and_then(Value::as_str) {
                self.observe_response_id(response_id);
            }
        }
    }

    fn observe_response_id(&mut self, response_id: &str) {
        if response_id.is_empty()
            || response_id.len() > MAX_IDENTIFIER_BYTES
            || self
                .response_id
                .as_deref()
                .is_some_and(|expected| expected != response_id)
        {
            self.invalid = true;
        } else {
            self.response_id
                .get_or_insert_with(|| response_id.to_owned());
        }
    }

    fn observe_terminal(&mut self, terminal: TerminalKind) {
        if terminal == TerminalKind::Done {
            return;
        }
        if self.terminal.is_some_and(|expected| expected != terminal) {
            self.invalid = true;
        } else {
            self.terminal.get_or_insert(terminal);
        }
    }
}

/// 只复核中间件确实改写过的完整 JSON；未经改写的原生快路保持原行为
pub(super) fn validate_buffered_response(
    facts: &ResponseValidationFacts,
    modified: &[u8],
) -> Result<(), ResponseValidationError> {
    let expected = facts.snapshot();
    if expected.invalid {
        return Err(ResponseValidationError);
    }
    let modified =
        serde_json::from_slice::<Value>(modified).map_err(|_| ResponseValidationError)?;
    let response_id = required_identifier(modified.get("id"))?;
    let terminal = response_terminal(&modified).ok_or(ResponseValidationError)?;
    if expected
        .response_id
        .as_deref()
        .is_some_and(|expected| expected != response_id)
        || expected
            .terminal
            .is_some_and(|expected| expected != terminal)
    {
        return Err(ResponseValidationError);
    }
    validate_terminal_response(&modified, response_id, terminal)
}

/// 持有实际已交付给客户端的 Responses 流状态
///
/// 原始前缀只旁路构建状态；直到插件首次改写或丢弃正文才启用严格拒绝，避免给
/// 无策略原生热路增加新的兼容性条件
#[derive(Debug, Default)]
pub(super) struct ResponsesDeliveryValidator {
    active: bool,
    invalid_prefix: bool,
    state: DeliveryState,
}

impl ResponsesDeliveryValidator {
    /// 复核实际交付帧；只有宿主标记为已改写时才激活严格模式
    pub(super) fn validate_frame(
        &mut self,
        delivered: &[u8],
        transformed: bool,
        facts: &ResponseValidationFacts,
    ) -> Result<(), ResponseValidationError> {
        self.validate_parsed(ParsedFrame::parse(delivered), transformed, facts)
    }

    /// WebSocket JSON message 与 SSE 共用同一响应顺序和关联校验
    pub(super) fn validate_websocket_frame(
        &mut self,
        delivered: &[u8],
        transformed: bool,
        facts: &ResponseValidationFacts,
    ) -> Result<(), ResponseValidationError> {
        self.validate_parsed(ParsedFrame::parse_json(delivered), transformed, facts)
    }

    fn validate_parsed(
        &mut self,
        parsed: Result<ParsedFrame, ResponseValidationError>,
        transformed: bool,
        facts: &ResponseValidationFacts,
    ) -> Result<(), ResponseValidationError> {
        if !self.active && !transformed {
            if parsed.and_then(|frame| self.state.apply(frame)).is_err() {
                self.invalid_prefix = true;
            }
            return Ok(());
        }
        self.activate()?;
        let parsed = parsed?;
        let expected = facts.snapshot();
        if expected.invalid {
            return Err(ResponseValidationError);
        }
        match &parsed {
            ParsedFrame::Event { event_type, value } if event_type == "response.created" => {
                let response_id = required_identifier(
                    value
                        .get("response")
                        .and_then(|response| response.get("id")),
                )?;
                if expected
                    .response_id
                    .as_deref()
                    .is_some_and(|expected| expected != response_id)
                {
                    return Err(ResponseValidationError);
                }
            }
            ParsedFrame::Event { event_type, value }
                if terminal_kind(Some(event_type)).is_some() =>
            {
                let terminal = terminal_kind(Some(event_type)).ok_or(ResponseValidationError)?;
                let response = value.get("response").unwrap_or(value);
                let response_id = required_identifier(response.get("id"))?;
                if expected
                    .response_id
                    .as_deref()
                    .is_some_and(|expected| expected != response_id)
                    || expected
                        .terminal
                        .is_some_and(|expected| expected != terminal)
                {
                    return Err(ResponseValidationError);
                }
            }
            ParsedFrame::Event { .. } | ParsedFrame::Done => {}
        }
        self.state.apply(parsed)
    }

    pub(super) fn response_id(&self) -> Option<&str> {
        self.state.response_id.as_deref()
    }

    pub(super) fn finish_delivery(&self) -> Result<(), ResponseValidationError> {
        if !self.active || self.state.done {
            Ok(())
        } else {
            Err(ResponseValidationError)
        }
    }

    pub(super) fn finish_websocket_delivery(&self) -> Result<(), ResponseValidationError> {
        if !self.active || self.state.terminal.is_some() {
            Ok(())
        } else {
            Err(ResponseValidationError)
        }
    }

    fn activate(&mut self) -> Result<(), ResponseValidationError> {
        self.active = true;
        if self.invalid_prefix {
            Err(ResponseValidationError)
        } else {
            Ok(())
        }
    }
}

#[derive(Debug)]
enum ParsedFrame {
    Event { event_type: String, value: Value },
    Done,
}

impl ParsedFrame {
    fn parse(bytes: &[u8]) -> Result<Self, ResponseValidationError> {
        let frame = std::str::from_utf8(bytes).map_err(|_| ResponseValidationError)?;
        if sse_frame_is_done(frame) {
            return Ok(Self::Done);
        }
        let mut events = parse_sse_events(frame).map_err(|_| ResponseValidationError)?;
        if events.len() != 1 {
            return Err(ResponseValidationError);
        }
        let event = events.pop().ok_or(ResponseValidationError)?;
        let value =
            serde_json::from_str::<Value>(&event.data).map_err(|_| ResponseValidationError)?;
        let json_type = value.get("type").and_then(Value::as_str);
        if let (Some(sse_type), Some(json_type)) = (event.event.as_deref(), json_type)
            && sse_type != json_type
        {
            return Err(ResponseValidationError);
        }
        let event_type = event
            .event
            .or_else(|| json_type.map(ToOwned::to_owned))
            .filter(|value| !value.is_empty() && value.len() <= MAX_IDENTIFIER_BYTES)
            .ok_or(ResponseValidationError)?;
        if !value.is_object() {
            return Err(ResponseValidationError);
        }
        Ok(Self::Event { event_type, value })
    }

    fn parse_json(bytes: &[u8]) -> Result<Self, ResponseValidationError> {
        let value = serde_json::from_slice::<Value>(bytes).map_err(|_| ResponseValidationError)?;
        let event_type = required_identifier(value.get("type"))?.to_owned();
        if !value.is_object() {
            return Err(ResponseValidationError);
        }
        Ok(Self::Event { event_type, value })
    }
}

fn terminal_kind(event_type: Option<&str>) -> Option<TerminalKind> {
    match event_type {
        Some("response.completed") => Some(TerminalKind::Completed),
        Some("response.incomplete") => Some(TerminalKind::Incomplete),
        Some("response.failed" | "error") => Some(TerminalKind::Failed),
        _ => None,
    }
}

fn response_terminal(response: &Value) -> Option<TerminalKind> {
    match response.get("status").and_then(Value::as_str) {
        Some("completed") => Some(TerminalKind::Completed),
        Some("incomplete") => Some(TerminalKind::Incomplete),
        Some("failed") => Some(TerminalKind::Failed),
        _ => None,
    }
}

#[derive(Debug, Default)]
struct DeliveryState {
    response_id: Option<String>,
    terminal: Option<TerminalKind>,
    done: bool,
    output_items: BTreeMap<u32, OutputItem>,
    content_parts: BTreeSet<(u32, u32)>,
}

impl DeliveryState {
    fn apply(&mut self, frame: ParsedFrame) -> Result<(), ResponseValidationError> {
        if self.done {
            return Err(ResponseValidationError);
        }
        let ParsedFrame::Event { event_type, value } = frame else {
            if self.terminal.is_none() {
                return Err(ResponseValidationError);
            }
            self.done = true;
            return Ok(());
        };
        match event_type.as_str() {
            "response.created" => self.start(&value),
            "response.in_progress" | "response.queued" => self.progress(&value),
            "response.output_item.added" => self.output_item(&value, false),
            "response.output_item.done" => self.output_item(&value, true),
            "response.content_part.added"
            | "response.reasoning_summary_part.added"
            | "response.reasoning_part.added" => self.content_part(&value, false),
            "response.content_part.done"
            | "response.reasoning_summary_part.done"
            | "response.reasoning_part.done" => self.content_part(&value, true),
            "response.output_text.delta"
            | "response.output_text.done"
            | "response.refusal.delta"
            | "response.refusal.done"
            | "response.reasoning_summary_text.delta"
            | "response.reasoning_summary_text.done"
            | "response.reasoning_text.delta"
            | "response.reasoning_text.done" => self.content_event(&value),
            "response.function_call_arguments.delta" | "response.custom_tool_call_input.delta" => {
                self.tool_arguments(&event_type, &value, false)
            }
            "response.function_call_arguments.done" | "response.custom_tool_call_input.done" => {
                self.tool_arguments(&event_type, &value, true)
            }
            "response.completed" | "response.incomplete" | "response.failed" | "error" => {
                self.finish_response(&event_type, &value)
            }
            _ => Ok(()),
        }
    }

    fn start(&mut self, value: &Value) -> Result<(), ResponseValidationError> {
        if self.response_id.is_some() || self.terminal.is_some() {
            return Err(ResponseValidationError);
        }
        let response = required_object(value.get("response"))?;
        let response_id = required_identifier(response.get("id"))?;
        if let Some(status) = response.get("status") {
            let status = required_identifier(Some(status))?;
            if !matches!(status, "queued" | "in_progress") {
                return Err(ResponseValidationError);
            }
        }
        self.response_id = Some(response_id.to_owned());
        Ok(())
    }

    fn progress(&self, value: &Value) -> Result<(), ResponseValidationError> {
        self.require_active()?;
        if let Some(response) = value.get("response") {
            let response = required_object(Some(response))?;
            self.validate_response_id(response.get("id"))?;
        }
        Ok(())
    }

    fn output_item(&mut self, value: &Value, done: bool) -> Result<(), ResponseValidationError> {
        self.require_active()?;
        self.validate_optional_event_response_id(value)?;
        let index = required_index(value, "output_index")?;
        let identity = OutputItemIdentity::parse(required_object(value.get("item"))?)?;
        if let Some(existing) = self.output_items.get_mut(&index) {
            if !done || existing.done || existing.identity != identity {
                return Err(ResponseValidationError);
            }
            existing.done = true;
            return Ok(());
        }
        if self.output_items.len() >= MAX_TRACKED_OUTPUT_ITEMS
            || self.output_items.values().any(|existing| {
                !identity.id.is_empty() && existing.identity.id == identity.id
                    || existing.identity.call_id.is_some()
                        && existing.identity.call_id == identity.call_id
            })
        {
            return Err(ResponseValidationError);
        }
        self.output_items.insert(
            index,
            OutputItem {
                identity,
                done,
                arguments_done: false,
            },
        );
        Ok(())
    }

    fn content_part(&mut self, value: &Value, done: bool) -> Result<(), ResponseValidationError> {
        self.require_active()?;
        self.validate_optional_event_response_id(value)?;
        let output_index = required_index(value, "output_index")?;
        if !self.output_items.contains_key(&output_index) {
            return Err(ResponseValidationError);
        }
        let content_index = required_index(value, "content_index")?;
        let key = (output_index, content_index);
        if done {
            if !self.content_parts.remove(&key) {
                return Err(ResponseValidationError);
            }
        } else if self.content_parts.len() >= MAX_TRACKED_OUTPUT_ITEMS
            || !self.content_parts.insert(key)
        {
            return Err(ResponseValidationError);
        }
        Ok(())
    }

    fn content_event(&self, value: &Value) -> Result<(), ResponseValidationError> {
        self.require_active()?;
        self.validate_optional_event_response_id(value)?;
        let key = (
            required_index(value, "output_index")?,
            required_index(value, "content_index")?,
        );
        if !self.content_parts.contains(&key) {
            return Err(ResponseValidationError);
        }
        Ok(())
    }

    fn tool_arguments(
        &mut self,
        event_type: &str,
        value: &Value,
        done: bool,
    ) -> Result<(), ResponseValidationError> {
        self.require_active()?;
        self.validate_optional_event_response_id(value)?;
        let output_index = required_index(value, "output_index")?;
        let item = self
            .output_items
            .get_mut(&output_index)
            .ok_or(ResponseValidationError)?;
        let expected_type = if event_type.starts_with("response.function_call") {
            "function_call"
        } else {
            "custom_tool_call"
        };
        if item.identity.kind != expected_type
            || value.get("item_id").is_some_and(|id| {
                required_identifier(Some(id)).ok() != Some(item.identity.id.as_str())
            })
            || value.get("call_id").is_some_and(|call_id| {
                required_identifier(Some(call_id)).ok() != item.identity.call_id.as_deref()
            })
            || done && item.arguments_done
        {
            return Err(ResponseValidationError);
        }
        item.arguments_done |= done;
        Ok(())
    }

    fn finish_response(
        &mut self,
        event_type: &str,
        value: &Value,
    ) -> Result<(), ResponseValidationError> {
        self.require_active()?;
        if self.terminal.is_some() {
            return Err(ResponseValidationError);
        }
        let terminal = terminal_kind(Some(event_type)).ok_or(ResponseValidationError)?;
        let response = value
            .get("response")
            .or_else(|| (event_type == "error").then_some(value))
            .ok_or(ResponseValidationError)?;
        let expected_id = self.response_id.as_deref().ok_or(ResponseValidationError)?;
        validate_terminal_response(response, expected_id, terminal)?;
        self.terminal = Some(terminal);
        Ok(())
    }

    fn require_active(&self) -> Result<(), ResponseValidationError> {
        if self.response_id.is_some() && self.terminal.is_none() {
            Ok(())
        } else {
            Err(ResponseValidationError)
        }
    }

    fn validate_response_id(&self, value: Option<&Value>) -> Result<(), ResponseValidationError> {
        if required_identifier(value)?
            == self.response_id.as_deref().ok_or(ResponseValidationError)?
        {
            Ok(())
        } else {
            Err(ResponseValidationError)
        }
    }

    fn validate_optional_event_response_id(
        &self,
        value: &Value,
    ) -> Result<(), ResponseValidationError> {
        if let Some(response_id) = value.get("response_id") {
            self.validate_response_id(Some(response_id))?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct OutputItem {
    identity: OutputItemIdentity,
    done: bool,
    arguments_done: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct OutputItemIdentity {
    id: String,
    kind: String,
    call_id: Option<String>,
}

impl OutputItemIdentity {
    fn parse(item: &Map<String, Value>) -> Result<Self, ResponseValidationError> {
        let kind = required_identifier(item.get("type"))?;
        let known_item = matches!(
            kind,
            "message"
                | "reasoning"
                | "function_call"
                | "custom_tool_call"
                | "image_generation_call"
                | "computer_call"
                | "web_search_call"
        );
        let id = item
            .get("id")
            .map(|value| required_identifier(Some(value)))
            .transpose()?
            .unwrap_or("");
        if known_item && id.is_empty() {
            return Err(ResponseValidationError);
        }
        let call_id = item
            .get("call_id")
            .map(|value| required_identifier(Some(value)).map(ToOwned::to_owned))
            .transpose()?;
        if matches!(kind, "function_call" | "custom_tool_call")
            && (call_id.is_none() || required_identifier(item.get("name")).is_err())
        {
            return Err(ResponseValidationError);
        }
        Ok(Self {
            id: id.to_owned(),
            kind: kind.to_owned(),
            call_id,
        })
    }
}

fn validate_terminal_response(
    response: &Value,
    expected_id: &str,
    terminal: TerminalKind,
) -> Result<(), ResponseValidationError> {
    let response = required_object(Some(response))?;
    if required_identifier(response.get("id"))? != expected_id {
        return Err(ResponseValidationError);
    }
    let expected_status = match terminal {
        TerminalKind::Completed => "completed",
        TerminalKind::Incomplete => "incomplete",
        TerminalKind::Failed => "failed",
        TerminalKind::Done => return Err(ResponseValidationError),
    };
    if required_identifier(response.get("status"))? != expected_status {
        return Err(ResponseValidationError);
    }
    if terminal == TerminalKind::Failed {
        if !response.get("error").is_some_and(Value::is_object) {
            return Err(ResponseValidationError);
        }
        return Ok(());
    }
    let output = response
        .get("output")
        .and_then(Value::as_array)
        .ok_or(ResponseValidationError)?;
    if output.len() > MAX_TRACKED_OUTPUT_ITEMS {
        return Err(ResponseValidationError);
    }
    let mut ids = BTreeSet::new();
    let mut call_ids = BTreeSet::new();
    for item in output {
        let identity = OutputItemIdentity::parse(required_object(Some(item))?)?;
        if (!identity.id.is_empty() && !ids.insert(identity.id))
            || identity
                .call_id
                .is_some_and(|call_id| !call_ids.insert(call_id))
        {
            return Err(ResponseValidationError);
        }
    }
    Ok(())
}

fn required_object(value: Option<&Value>) -> Result<&Map<String, Value>, ResponseValidationError> {
    value
        .and_then(Value::as_object)
        .ok_or(ResponseValidationError)
}

fn required_identifier(value: Option<&Value>) -> Result<&str, ResponseValidationError> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= MAX_IDENTIFIER_BYTES)
        .ok_or(ResponseValidationError)
}

fn required_index(value: &Value, name: &str) -> Result<u32, ResponseValidationError> {
    value
        .get(name)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(ResponseValidationError)
}
