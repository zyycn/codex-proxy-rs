//! GenAI 原生计数与 Provider 计数端点的正文解包

use crate::transport::{sent, unsent};
use gateway_core::{
    engine::provider::NativeResponseTranslator,
    error::{ProviderError, ProviderErrorKind},
    event::ProtocolWireEvent,
    operation::Operation,
};
use gateway_protocol::genai::{GENAI_PROTOCOL, count_tokens_request};
use serde_json::{Map, Value};

pub(super) fn prepare(
    operation: Operation,
) -> Result<(Operation, Option<Translator>), ProviderError> {
    let Operation::CountTokens(request) = &operation else {
        return Err(unsent(ProviderErrorKind::Unsupported));
    };
    let source = request.payload().protocol();
    if !matches!(source, GENAI_PROTOCOL | "token-count") {
        return Err(unsent(ProviderErrorKind::Unsupported));
    }
    let body: Map<String, Value> = serde_json::from_slice(request.payload().body())
        .map_err(|error| unsent(ProviderErrorKind::InvalidRequest).with_source(error))?;
    let body = count_tokens_request(&body)
        .map_err(|error| unsent(ProviderErrorKind::InvalidRequest).with_source(error))?;
    let bytes = serde_json::to_vec(body).map_err(|_| unsent(ProviderErrorKind::InvalidRequest))?;
    let translated = source == "token-count";
    let operation = if translated {
        operation.translate_protocol_wire(GENAI_PROTOCOL, bytes.into())
    } else {
        let context = request.payload().context().clone();
        operation.replace_protocol_wire(Some(bytes.into()), context)
    }
    .map_err(|_| unsent(ProviderErrorKind::InvalidRequest))?;
    Ok((operation, translated.then_some(Translator)))
}

pub(super) struct Translator;

impl NativeResponseTranslator for Translator {
    fn source_protocol(&self) -> &str {
        GENAI_PROTOCOL
    }
    fn target_protocol(&self) -> &str {
        "token-count"
    }
    fn translate(
        &mut self,
        event: &ProtocolWireEvent,
    ) -> Result<Vec<ProtocolWireEvent>, ProviderError> {
        let wire = match event.raw_json_body() {
            Some(raw) => ProtocolWireEvent::raw_json("token-count", raw.clone()),
            None => ProtocolWireEvent::json("token-count", None, event.data().clone()),
        };
        wire.map(|wire| vec![wire])
            .map_err(|_| sent(ProviderErrorKind::Protocol))
    }
}
