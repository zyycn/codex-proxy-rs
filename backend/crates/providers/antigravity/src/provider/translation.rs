//! 跨协议请求使用共享转换器，原生响应转换在 Core 中间件阶段之间执行

use crate::transport::{sent, unsent};
use gateway_core::{
    engine::{
        ModelRequestId,
        execution::ClientTransport,
        provider::{LocalWebSocketPrewarm, NativeResponseTranslator},
    },
    error::{ClientVisibleUpstreamError, ProviderError, ProviderErrorKind},
    event::ProtocolWireEvent,
    operation::{Operation, ProtocolPayload},
    provider_ports::ProviderSessionAffinityKey,
    upstream::UpstreamSendState,
};
use gateway_protocol::{
    anthropic::{ANTHROPIC_PROTOCOL, MessagesToResponsesStream},
    genai::{
        GENAI_PROTOCOL, GenAiToMessagesStream, messages_to_genai_request,
        responses_to_genai_request,
    },
    openai::{OPENAI_PROTOCOL, web_search},
};
use serde_json::{Value, json};
use std::borrow::Cow;

fn is_root_prewarm(payload: &ProtocolPayload) -> bool {
    let body = payload.body();
    payload.protocol() == OPENAI_PROTOCOL
        && body.get("generate").and_then(Value::as_bool) == Some(false)
        && body.get("previous_response_id").is_none_or(Value::is_null)
}

pub(super) fn websocket_prewarm(
    operation: &Operation,
    request_id: &ModelRequestId,
) -> Option<LocalWebSocketPrewarm> {
    let Operation::Generate(generate) = operation else {
        return None;
    };
    let payload = generate.protocol_payload();
    if !is_root_prewarm(payload) {
        return None;
    }
    let model = payload.body().get("model")?.as_str()?;
    let response_id = format!("resp_prewarm_{}", request_id.as_str());
    let timestamp = chrono::Utc::now().timestamp();
    let frame = |kind: &str, sequence: u8, status: &str, usage: Value| {
        let data = json!({
            "type": kind,
            "sequence_number": sequence,
            "response": {
                "id": response_id, "object": "response", "created_at": timestamp, "model": model,
                "status": status, "error": null, "output": [], "usage": usage
            }
        });
        ProtocolWireEvent::json(OPENAI_PROTOCOL, Some(kind.to_owned()), data).ok()
    };
    let created = frame("response.created", 0, "in_progress", Value::Null)?;
    let usage = json!({"input_tokens": 0, "output_tokens": 0, "total_tokens": 0});
    let completed = frame("response.completed", 1, "completed", usage)?;
    Some(LocalWebSocketPrewarm {
        response_id,
        created,
        completed,
    })
}

pub(super) fn validate_request_mode(operation: &Operation) -> Result<(), ProviderError> {
    let Operation::Generate(generate) = operation else {
        return Ok(());
    };
    let payload = generate.protocol_payload();
    if !matches!(payload.protocol(), OPENAI_PROTOCOL | ANTHROPIC_PROTOCOL) {
        return Ok(());
    }
    // 带父响应的预热也需要重放；其余未在 WS 入口完成的预热不能触发推理
    if is_root_prewarm(payload) {
        return Err(
            unsent(ProviderErrorKind::InvalidRequest).with_client_visible_upstream_error(
                ClientVisibleUpstreamError::new(
                    "Antigravity supports prewarm only on a new Responses WebSocket chain.",
                    Some("unsupported_prewarm".to_owned()),
                    Some("invalid_request_error".to_owned()),
                ),
            ),
        );
    }
    match payload.body().get("previous_response_id") {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(id))
            if payload.protocol() == OPENAI_PROTOCOL && !id.trim().is_empty() =>
        {
            Err(ProviderError::responses_replay_required(
                "scope_unavailable",
                UpstreamSendState::NotSent,
            ))
        }
        _ => Err(unsent(ProviderErrorKind::InvalidRequest)),
    }
}

pub(super) fn prepare(
    operation: Operation,
    transport: ClientTransport,
    affinity: Option<&ProviderSessionAffinityKey>,
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
    // attempt 中间件可能改变预热或续接字段
    validate_request_mode(&operation)?;
    let search = if source == OPENAI_PROTOCOL {
        web_search::required_tool(original)
            .map_err(|error| ProviderError::invalid_request(error.message()).with_source(error))?
    } else {
        None
    };
    let mut body = if let Some(tool) = search {
        super::web_search::request(original, tool)?
    } else {
        if source == OPENAI_PROTOCOL {
            responses_to_genai_request(original)
        } else {
            messages_to_genai_request(original)
        }
        .map_err(|e| unsent(ProviderErrorKind::InvalidRequest).with_source(e))?
    };
    if let Some(session) = affinity {
        body.insert(
            "sessionId".into(),
            json!(super::affinity::upstream_session_id(session)),
        );
    }
    let model = original
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let translator = if search.is_some() {
        Translator::Search(Box::new(super::web_search::SearchResponse::new(original)))
    } else {
        Translator::Generate(Box::new(GenerationTranslator {
            genai: GenAiToMessagesStream::new(model),
            responses: (source == OPENAI_PROTOCOL)
                .then(|| MessagesToResponsesStream::new(original)),
        }))
    };
    let mut context = payload.context().clone();
    // HTTP 和 WS 的交付模式在入口已确定，内部探测默认保留首输出观测
    let stream = match transport {
        ClientTransport::HttpJson => false,
        ClientTransport::HttpSse | ClientTransport::WebSocket => true,
        ClientTransport::InternalProbe | ClientTransport::InternalPlugin => original
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(true),
    };
    context.insert("stream".into(), json!(search.is_none() && stream));
    let bytes = serde_json::to_vec(&body).map_err(|_| unsent(ProviderErrorKind::InvalidRequest))?;
    let operation = operation
        .replace_protocol_wire(None, context)
        .and_then(|operation| operation.translate_protocol_wire(GENAI_PROTOCOL, bytes.into()))
        .map_err(|_| unsent(ProviderErrorKind::InvalidRequest))?;
    Ok((operation, Some(translator)))
}

pub(super) enum Translator {
    Generate(Box<GenerationTranslator>),
    Search(Box<super::web_search::SearchResponse>),
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
            Self::Search(_) => OPENAI_PROTOCOL,
            Self::Count(protocol) => protocol,
        }
    }
    fn translate(
        &mut self,
        event: &ProtocolWireEvent,
    ) -> Result<Vec<ProtocolWireEvent>, ProviderError> {
        if let Self::Search(search) = self {
            return search.translate(event);
        }
        let value = match event.raw_json_body() {
            Some(raw) => Cow::Owned(
                serde_json::from_slice(raw).map_err(|_| sent(ProviderErrorKind::Protocol))?,
            ),
            None => Cow::Borrowed(event.data()),
        };
        let Self::Generate(translator) = self else {
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
            .push(&value)
            .map_err(|e| sent(ProviderErrorKind::Protocol).with_source(e))?;
        translator.events(events)
    }
    fn finish(&mut self) -> Result<Vec<ProtocolWireEvent>, ProviderError> {
        if let Self::Search(search) = self {
            return search.finish();
        }
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
