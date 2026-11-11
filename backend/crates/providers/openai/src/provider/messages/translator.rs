//! 双向转换器由每条响应独占，纯协议状态不接触账号、重试或费用

use super::*;
use gateway_core::engine::provider::NativeResponseTranslator;
use gateway_protocol::anthropic::{MessagesToResponsesStream, ResponsesToMessagesStream};

pub(super) struct MessagesResponseTranslator {
    stream: MessagesToResponsesStream,
}

impl MessagesResponseTranslator {
    pub(super) fn new(original: &Map<String, Value>) -> Self {
        Self {
            stream: MessagesToResponsesStream::new(original),
        }
    }
}

impl NativeResponseTranslator for MessagesResponseTranslator {
    fn source_protocol(&self) -> &str {
        ANTHROPIC_PROTOCOL
    }
    fn target_protocol(&self) -> &str {
        OPENAI_PROTOCOL
    }
    fn translate(
        &mut self,
        event: &ProtocolWireEvent,
    ) -> Result<Vec<ProtocolWireEvent>, ProviderError> {
        if let Some(raw) = event.raw_json_body() {
            let message: Value = serde_json::from_slice(raw).map_err(|_| protocol_error())?;
            let event = self
                .stream
                .complete_json_event(&message)
                .map_err(|_| protocol_error())?;
            return wire_events(OPENAI_PROTOCOL, vec![event]);
        }
        if !event.has_json_data() {
            return Ok(Vec::new());
        }
        let events = self
            .stream
            .push(event.data())
            .map_err(|_| protocol_error())?;
        if event.data().get("type").and_then(Value::as_str) == Some("message_stop") {
            self.stream.finish().map_err(|_| protocol_error())?;
        }
        wire_events(OPENAI_PROTOCOL, events)
    }
}

pub(super) struct ResponsesResponseTranslator {
    stream: ResponsesToMessagesStream,
}

impl ResponsesResponseTranslator {
    pub(super) fn new(original: &Map<String, Value>) -> Self {
        Self {
            stream: ResponsesToMessagesStream::new(original),
        }
    }
}

impl NativeResponseTranslator for ResponsesResponseTranslator {
    fn source_protocol(&self) -> &str {
        OPENAI_PROTOCOL
    }
    fn target_protocol(&self) -> &str {
        ANTHROPIC_PROTOCOL
    }
    fn translate(
        &mut self,
        event: &ProtocolWireEvent,
    ) -> Result<Vec<ProtocolWireEvent>, ProviderError> {
        if let Some(raw) = event.raw_json_body() {
            let response = serde_json::from_slice(raw).map_err(|_| protocol_error())?;
            let message = self
                .stream
                .complete_json(&response)
                .map_err(|_| protocol_error())?;
            let raw = serde_json::to_vec(&message).map_err(|_| protocol_error())?;
            return Ok(vec![
                ProtocolWireEvent::raw_json(ANTHROPIC_PROTOCOL, raw.into())
                    .map_err(|_| protocol_error())?,
            ]);
        }
        if !event.has_json_data() {
            return Ok(Vec::new());
        }
        let events = self
            .stream
            .push(event.data())
            .map_err(|_| protocol_error())?;
        if matches!(
            event.data().get("type").and_then(Value::as_str),
            Some("response.completed" | "response.incomplete")
        ) {
            self.stream.finish().map_err(|_| protocol_error())?;
        }
        wire_events(ANTHROPIC_PROTOCOL, events)
    }
}

fn wire_events(
    protocol: &str,
    events: Vec<Value>,
) -> Result<Vec<ProtocolWireEvent>, ProviderError> {
    events
        .into_iter()
        .map(|value| {
            ProtocolWireEvent::json(
                protocol,
                value.get("type").and_then(Value::as_str).map(str::to_owned),
                value,
            )
            .map_err(|_| protocol_error())
        })
        .collect()
}
