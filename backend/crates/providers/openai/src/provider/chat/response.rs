//! 单次响应直接映射为 Chat wire，原生观测与资源释放归现有执行链

use super::*;
use gateway_core::engine::provider::NativeResponseTranslator;
use gateway_protocol::{
    anthropic::ANTHROPIC_PROTOCOL,
    chat_completions::{CHAT_PROTOCOL, ChatError},
};
use serde_json::Map;

enum Encoder {
    Responses(responses::ChatStreamEncoder),
    Messages(messages::ChatStreamEncoder),
}

pub(super) struct ChatResponseTranslator {
    identity: ChatIdentity,
    request: Map<String, Value>,
    encoder: Encoder,
    complete_json: bool,
    streaming: bool,
    include_usage: bool,
}

impl ChatResponseTranslator {
    pub(super) fn new(
        target: ApiKeyProtocol,
        identity: ChatIdentity,
        request: &Map<String, Value>,
    ) -> Self {
        let include_usage = request.get("stream").and_then(Value::as_bool) != Some(true)
            || request
                .get("stream_options")
                .and_then(|value| value.get("include_usage"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
        let encoder = match target {
            ApiKeyProtocol::Responses => Encoder::Responses(responses::ChatStreamEncoder::new(
                identity.clone(),
                include_usage,
            )),
            ApiKeyProtocol::Messages => Encoder::Messages(
                messages::ChatStreamEncoder::with_request(identity.clone(), include_usage, request),
            ),
        };
        Self {
            identity,
            // 响应只需要工具声明，不为转换状态保留整份对话正文
            request: request
                .get("tools")
                .map(|tools| Map::from_iter([("tools".into(), tools.clone())]))
                .unwrap_or_default(),
            encoder,
            complete_json: false,
            streaming: request.get("stream").and_then(Value::as_bool) == Some(true),
            include_usage,
        }
    }
}

impl NativeResponseTranslator for ChatResponseTranslator {
    fn source_protocol(&self) -> &str {
        match self.encoder {
            Encoder::Responses(_) => OPENAI_PROTOCOL,
            Encoder::Messages(_) => ANTHROPIC_PROTOCOL,
        }
    }

    fn target_protocol(&self) -> &str {
        CHAT_PROTOCOL
    }

    fn translate(
        &mut self,
        event: &ProtocolWireEvent,
    ) -> Result<Vec<ProtocolWireEvent>, ProviderError> {
        if self.complete_json {
            return Err(response_error());
        }
        if let Some(raw) = event.raw_json_body() {
            let body = serde_json::from_slice(raw).map_err(|_| response_error())?;
            let response = match self.encoder {
                Encoder::Responses(_) => responses::project_response(&body, &self.identity),
                Encoder::Messages(_) => {
                    messages::project_response_with_request(&body, &self.identity, &self.request)
                }
            }
            .map_err(map_error)?;
            self.complete_json = true;
            if self.streaming {
                return wire_events(
                    gateway_protocol::chat_completions::completion_chunks(
                        &response,
                        self.include_usage,
                    )
                    .map_err(map_error)?,
                );
            }
            return ProtocolWireEvent::raw_json(
                CHAT_PROTOCOL,
                serde_json::to_vec(&response)
                    .map_err(|_| response_error())?
                    .into(),
            )
            .map(|wire| vec![wire])
            .map_err(|_| response_error());
        }
        if !event.has_json_data() {
            return Ok(Vec::new());
        }
        let chunks = match &mut self.encoder {
            Encoder::Responses(encoder) => encoder.push_event(event.event_type(), event.data()),
            Encoder::Messages(encoder) => encoder.push_event(event.event_type(), event.data()),
        }
        .map_err(map_error)?;
        wire_events(chunks)
    }

    fn finish(&mut self) -> Result<Vec<ProtocolWireEvent>, ProviderError> {
        if self.complete_json {
            return Ok(Vec::new());
        }
        let chunks = match &mut self.encoder {
            Encoder::Responses(encoder) => encoder.finish(),
            Encoder::Messages(encoder) => encoder.finish(),
        }
        .map_err(map_error)?;
        wire_events(chunks)
    }
}

fn wire_events(chunks: Vec<Value>) -> Result<Vec<ProtocolWireEvent>, ProviderError> {
    chunks
        .into_iter()
        .map(|chunk| {
            ProtocolWireEvent::json(CHAT_PROTOCOL, None, chunk).map_err(|_| response_error())
        })
        .collect()
}

fn map_error(error: ChatError) -> ProviderError {
    response_error().with_source(error)
}
