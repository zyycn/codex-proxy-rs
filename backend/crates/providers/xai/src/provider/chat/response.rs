//! Grok 原始事件直接生成 Chat，再还原目标工具参数中的字符串输入

use super::*;
use gateway_core::{engine::provider::NativeResponseTranslator, event::ProtocolWireEvent};
use gateway_protocol::chat_completions::responses::ChatStreamEncoder;

pub(super) struct Translator {
    encoder: ChatStreamEncoder,
    custom: tools::CustomTools,
}

impl Translator {
    pub(super) fn new(identity: ChatIdentity, request: &Map<String, Value>) -> Self {
        let include_usage = request.get("stream").and_then(Value::as_bool) != Some(true)
            || request
                .get("stream_options")
                .and_then(|value| value.get("include_usage"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
        Self {
            encoder: ChatStreamEncoder::new(identity, include_usage),
            custom: tools::CustomTools::new(request),
        }
    }

    fn wire(&mut self, chunks: Vec<Value>) -> Result<Vec<ProtocolWireEvent>, ProviderError> {
        let chunks = self.custom.project(chunks)?;
        chunks
            .into_iter()
            .map(|chunk| {
                ProtocolWireEvent::json(CHAT_PROTOCOL, None, chunk).map_err(|_| protocol_error())
            })
            .collect()
    }
}

impl NativeResponseTranslator for Translator {
    fn source_protocol(&self) -> &str {
        OPENAI_PROTOCOL
    }
    fn target_protocol(&self) -> &str {
        CHAT_PROTOCOL
    }
    fn translate(
        &mut self,
        event: &ProtocolWireEvent,
    ) -> Result<Vec<ProtocolWireEvent>, ProviderError> {
        if !event.has_json_data() {
            return Ok(Vec::new());
        }
        let chunks = self
            .encoder
            .push_event(event.event_type(), event.data())
            .map_err(|error| protocol_error().with_source(error))?;
        self.wire(chunks)
    }
    fn finish(&mut self) -> Result<Vec<ProtocolWireEvent>, ProviderError> {
        let chunks = self
            .encoder
            .finish()
            .map_err(|error| protocol_error().with_source(error))?;
        self.wire(chunks)
    }
}

fn protocol_error() -> ProviderError {
    provider_error(ProviderErrorKind::Protocol, UpstreamSendState::Sent)
}
