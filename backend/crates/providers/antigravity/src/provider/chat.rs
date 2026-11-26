//! Chat 直接装配 GenAI，请求转换和响应状态不接触账号租约

use crate::transport::{sent, unsent};
use gateway_core::{
    engine::{AttemptContext, provider::NativeResponseTranslator},
    error::{ProviderError, ProviderErrorKind},
    event::ProtocolWireEvent,
    operation::Operation,
    provider_ports::ProviderSessionAffinityKey,
};
use gateway_protocol::{
    chat_completions::{CHAT_PROTOCOL, ChatIdentity, genai},
    genai::GENAI_PROTOCOL,
};
use serde_json::{Map, Value};
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) fn prepare(
    operation: Operation,
    context: &AttemptContext,
    affinity: Option<&ProviderSessionAffinityKey>,
) -> Result<(Operation, Option<Translator>), ProviderError> {
    let Operation::Generate(generate) = &operation else {
        return Err(unsent(ProviderErrorKind::Unsupported));
    };
    let payload = generate.protocol_payload();
    if payload.protocol() == GENAI_PROTOCOL {
        return Ok((operation, None));
    }
    if payload.protocol() != CHAT_PROTOCOL || generate.source_protocol() != CHAT_PROTOCOL {
        return Err(unsent(ProviderErrorKind::Unsupported));
    }
    let original = payload.body();
    let streaming = original
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let include_usage = !streaming
        || original
            .get("stream_options")
            .and_then(|options| options.get("include_usage"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let identity = ChatIdentity {
        id: format!("chatcmpl-{}", context.request_id().as_str()),
        created: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        model: original
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    };
    let mut body = genai::convert_request(original).map_err(|error| {
        ProviderError::invalid_request(error.safe_message())
            .with_request_details(error.code(), error.parameter())
            .with_source(error)
    })?;
    if let Some(session) = affinity {
        body.insert(
            "sessionId".to_owned(),
            Value::String(super::affinity::upstream_session_id(session)),
        );
    }
    let translator = Translator {
        encoder: genai::ChatStreamEncoder::with_request(identity.clone(), include_usage, original),
        identity,
        // 完整 JSON 的投影只需工具声明，不重复保存对话正文
        original: original
            .get("tools")
            .map(|tools| Map::from_iter([("tools".into(), tools.clone())]))
            .unwrap_or_default(),
        streaming,
        include_usage,
        complete_json: false,
    };
    let mut wire_context = payload.context().clone();
    // 托管搜索的 Cloud Code 端点只返回 JSON，响应仍直接投影到所需的 Chat 交付形式
    wire_context.insert(
        "stream".to_owned(),
        Value::Bool(streaming && !operation.requires_web_search()),
    );
    let body = serde_json::to_vec(&body).map_err(|_| unsent(ProviderErrorKind::InvalidRequest))?;
    let operation = operation
        .replace_protocol_wire(None, wire_context)
        .and_then(|operation| operation.translate_protocol_wire(GENAI_PROTOCOL, body.into()))
        .map_err(|_| unsent(ProviderErrorKind::InvalidRequest))?;
    Ok((operation, Some(translator)))
}

pub(super) struct Translator {
    identity: ChatIdentity,
    original: Map<String, Value>,
    encoder: genai::ChatStreamEncoder,
    streaming: bool,
    include_usage: bool,
    complete_json: bool,
}

impl NativeResponseTranslator for Translator {
    fn source_protocol(&self) -> &str {
        GENAI_PROTOCOL
    }
    fn target_protocol(&self) -> &str {
        CHAT_PROTOCOL
    }

    fn translate(
        &mut self,
        event: &ProtocolWireEvent,
    ) -> Result<Vec<ProtocolWireEvent>, ProviderError> {
        if self.complete_json {
            return Err(sent(ProviderErrorKind::Protocol));
        }
        if let Some(raw) = event.raw_json_body() {
            let body =
                serde_json::from_slice(raw).map_err(|_| sent(ProviderErrorKind::Protocol))?;
            let value = genai::project_response_with_request(&body, &self.identity, &self.original)
                .map_err(|error| sent(ProviderErrorKind::Protocol).with_source(error))?;
            self.complete_json = true;
            if self.streaming {
                return wire_events(
                    gateway_protocol::chat_completions::completion_chunks(
                        &value,
                        self.include_usage,
                    )
                    .map_err(|error| sent(ProviderErrorKind::Protocol).with_source(error))?,
                );
            }
            return ProtocolWireEvent::raw_json(
                CHAT_PROTOCOL,
                serde_json::to_vec(&value)
                    .map_err(|_| sent(ProviderErrorKind::Protocol))?
                    .into(),
            )
            .map(|wire| vec![wire])
            .map_err(|_| sent(ProviderErrorKind::Protocol));
        }
        if !event.has_json_data() {
            return Ok(Vec::new());
        }
        let chunks = self
            .encoder
            .push_event(event.event_type(), event.data())
            .map_err(|error| sent(ProviderErrorKind::Protocol).with_source(error))?;
        wire_events(chunks)
    }

    fn finish(&mut self) -> Result<Vec<ProtocolWireEvent>, ProviderError> {
        if self.complete_json {
            return Ok(Vec::new());
        }
        wire_events(
            self.encoder
                .finish()
                .map_err(|error| sent(ProviderErrorKind::Protocol).with_source(error))?,
        )
    }
}

fn wire_events(chunks: Vec<Value>) -> Result<Vec<ProtocolWireEvent>, ProviderError> {
    chunks
        .into_iter()
        .map(|chunk| {
            ProtocolWireEvent::json(CHAT_PROTOCOL, None, chunk)
                .map_err(|_| sent(ProviderErrorKind::Protocol))
        })
        .collect()
}
