//! 跨协议请求使用共享转换器，原生响应转换在 Core 中间件阶段之间执行

use crate::transport::{sent, unsent};
use gateway_core::{
    engine::provider::NativeResponseTranslator,
    error::{ProviderError, ProviderErrorKind},
    event::ProtocolWireEvent,
    operation::Operation,
};
use gateway_protocol::{
    anthropic::{
        ANTHROPIC_PROTOCOL, MessagesModelCapabilities, MessagesToResponsesStream,
        responses_to_messages_request,
    },
    genai::{GENAI_PROTOCOL, GenAiToMessagesStream, messages_to_genai_request},
    openai::OPENAI_PROTOCOL,
};
use serde_json::{Value, json};
use std::borrow::Cow;

pub(super) fn prepare(
    operation: Operation,
) -> Result<(Operation, Option<Translator>), ProviderError> {
    if matches!(operation, Operation::CountTokens(_)) {
        return prepare_count(operation);
    }
    let Operation::Generate(generate) = &operation else {
        return Err(unsent(ProviderErrorKind::Unsupported));
    };
    let payload = generate.protocol_payload();
    let source = payload.protocol();
    if source == GENAI_PROTOCOL {
        return Ok((operation, None));
    }
    if !matches!(source, OPENAI_PROTOCOL | ANTHROPIC_PROTOCOL) {
        return Err(unsent(ProviderErrorKind::Unsupported));
    }
    let original = payload.body();
    if original
        .get("previous_response_id")
        .is_some_and(|v| !v.is_null())
    {
        return Err(unsent(ProviderErrorKind::InvalidRequest));
    }
    let responses = if source == OPENAI_PROTOCOL {
        Some(MessagesToResponsesStream::new(original))
    } else {
        None
    };
    let mut messages = if source == OPENAI_PROTOCOL {
        Cow::Owned(
            responses_to_messages_request(original, &MessagesModelCapabilities::default())
                .map_err(|e| unsent(ProviderErrorKind::InvalidRequest).with_source(e))?,
        )
    } else {
        Cow::Borrowed(original)
    };
    // Messages 的必填输出上限不能成为 GenAI 的隐式默认值
    if source == OPENAI_PROTOCOL && !original.contains_key("max_output_tokens") {
        messages.to_mut().remove("max_tokens");
    }
    let model = original
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let body = messages_to_genai_request(&messages)
        .map_err(|e| unsent(ProviderErrorKind::InvalidRequest).with_source(e))?;
    let mut context = payload.context().clone();
    // 转换路径统一请求 SSE，JSON 客户端沿已有交付层聚合
    context.insert("stream".into(), json!(true));
    let bytes = serde_json::to_vec(&body).map_err(|_| unsent(ProviderErrorKind::InvalidRequest))?;
    let operation = operation
        .replace_protocol_wire(None, context)
        .and_then(|operation| operation.translate_protocol_wire(GENAI_PROTOCOL, bytes.into()))
        .map_err(|_| unsent(ProviderErrorKind::InvalidRequest))?;
    Ok((
        operation,
        Some(Translator::Generate(Box::new(GenerationTranslator {
            genai: GenAiToMessagesStream::new(model),
            responses,
        }))),
    ))
}

pub(super) enum Translator {
    Generate(Box<GenerationTranslator>),
    Count(&'static str),
}

pub(super) struct GenerationTranslator {
    genai: GenAiToMessagesStream,
    responses: Option<MessagesToResponsesStream>,
}

impl GenerationTranslator {
    fn target_protocol(&self) -> &'static str {
        if self.responses.is_some() {
            OPENAI_PROTOCOL
        } else {
            ANTHROPIC_PROTOCOL
        }
    }

    fn events(&mut self, events: Vec<Value>) -> Result<Vec<ProtocolWireEvent>, ProviderError> {
        let mut wire = Vec::new();
        for event in events {
            let events = if let Some(responses) = &mut self.responses {
                responses
                    .push(&event)
                    .map_err(|e| sent(ProviderErrorKind::Protocol).with_source(e))?
            } else {
                vec![event]
            };
            for event in events {
                wire.push(
                    ProtocolWireEvent::json(
                        self.target_protocol(),
                        event["type"].as_str().map(str::to_owned),
                        event,
                    )
                    .map_err(|_| sent(ProviderErrorKind::Protocol))?,
                );
            }
        }
        Ok(wire)
    }
}

impl NativeResponseTranslator for Translator {
    fn source_protocol(&self) -> &str {
        GENAI_PROTOCOL
    }
    fn target_protocol(&self) -> &str {
        match self {
            Self::Generate(translator) => translator.target_protocol(),
            Self::Count(protocol) => protocol,
        }
    }
    fn translate(
        &mut self,
        event: &ProtocolWireEvent,
    ) -> Result<Vec<ProtocolWireEvent>, ProviderError> {
        let Self::Generate(translator) = self else {
            let value = match event.raw_json_body() {
                Some(raw) => Cow::Owned(
                    serde_json::from_slice(raw).map_err(|_| sent(ProviderErrorKind::Protocol))?,
                ),
                None => Cow::Borrowed(event.data()),
            };
            let body = if self.target_protocol() == ANTHROPIC_PROTOCOL {
                gateway_protocol::genai::count_tokens_to_messages(&value)
                    .map_err(|e| sent(ProviderErrorKind::Protocol).with_source(e))?
            } else {
                value.into_owned()
            };
            return Ok(vec![
                ProtocolWireEvent::json(self.target_protocol(), None, body)
                    .map_err(|_| sent(ProviderErrorKind::Protocol))?,
            ]);
        };
        let events = translator
            .genai
            .push(event.data())
            .map_err(|e| sent(ProviderErrorKind::Protocol).with_source(e))?;
        translator.events(events)
    }
    fn finish(&mut self) -> Result<Vec<ProtocolWireEvent>, ProviderError> {
        let Self::Generate(translator) = self else {
            return Ok(Vec::new());
        };
        let events = translator
            .genai
            .finish()
            .map_err(|e| sent(ProviderErrorKind::Protocol).with_source(e))?;
        translator.events(events)
    }
}

fn prepare_count(operation: Operation) -> Result<(Operation, Option<Translator>), ProviderError> {
    let Operation::CountTokens(request) = &operation else {
        return Err(unsent(ProviderErrorKind::Unsupported));
    };
    let source = request.payload().protocol();
    let body: serde_json::Map<String, Value> = serde_json::from_slice(request.payload().body())
        .map_err(|e| unsent(ProviderErrorKind::InvalidRequest).with_source(e))?;
    let body = match source {
        ANTHROPIC_PROTOCOL => {
            gateway_protocol::anthropic::validate_count_request(&body)
                .map_err(|e| unsent(ProviderErrorKind::InvalidRequest).with_source(e))?;
            messages_to_genai_request(&body).map(Cow::Owned)
        }
        GENAI_PROTOCOL | "token-count" => {
            gateway_protocol::genai::count_tokens_request(&body).map(Cow::Borrowed)
        }
        _ => return Err(unsent(ProviderErrorKind::Unsupported)),
    }
    .map_err(|e| unsent(ProviderErrorKind::InvalidRequest).with_source(e))?;
    let translator = match source {
        ANTHROPIC_PROTOCOL => Some(Translator::Count(ANTHROPIC_PROTOCOL)),
        "token-count" => Some(Translator::Count("token-count")),
        _ => None,
    };
    let bytes = serde_json::to_vec(&body).map_err(|_| unsent(ProviderErrorKind::InvalidRequest))?;
    let operation = if source == GENAI_PROTOCOL {
        let context = request.payload().context().clone();
        operation.replace_protocol_wire(Some(bytes.into()), context)
    } else {
        operation.translate_protocol_wire(GENAI_PROTOCOL, bytes.into())
    }
    .map_err(|_| unsent(ProviderErrorKind::InvalidRequest))?;
    Ok((operation, translator))
}
