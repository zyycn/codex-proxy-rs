//! 出口校验按协议维护边界事实，不在流式路径保存生成正文

use gateway_core::event::ProviderEvent;
use gateway_protocol::anthropic::ANTHROPIC_PROTOCOL;
use gateway_protocol::openai::sse::parse_sse_events;
use serde_json::Value;
use std::sync::{Arc, Mutex};

use super::GenerationProtocol;
use crate::openai::responses::validation::{
    ResponseValidationFacts, ResponsesDeliveryValidator, validate_buffered_response,
};

#[derive(Clone)]
pub(super) enum GenerationValidationFacts {
    Responses(ResponseValidationFacts),
    Messages(Arc<Mutex<Option<String>>>),
    Count,
}

impl GenerationValidationFacts {
    pub(super) fn new(protocol: GenerationProtocol) -> Self {
        match protocol {
            GenerationProtocol::Responses => Self::Responses(ResponseValidationFacts::default()),
            GenerationProtocol::Messages => Self::Messages(Arc::default()),
            GenerationProtocol::MessageTokenCount => Self::Count,
        }
    }

    pub(super) fn observe_event(&self, event: &ProviderEvent) {
        match self {
            Self::Responses(facts) => facts.observe_event(event),
            Self::Messages(id) => {
                if let Some(value) = event
                    .wire_event()
                    .filter(|wire| wire.protocol() == ANTHROPIC_PROTOCOL)
                    .map(|wire| wire.data())
                    && let Some(value) = value
                        .pointer("/message/id")
                        .or_else(|| value.get("id"))
                        .and_then(Value::as_str)
                    && let Ok(mut id) = id.lock()
                {
                    *id = Some(value.to_owned());
                }
            }
            Self::Count => {}
        }
    }

    pub(super) fn observe_buffered(&self, events: &[ProviderEvent], bytes: &[u8]) {
        if let Self::Responses(facts) = self {
            facts.observe_buffered(events, bytes);
        } else {
            for event in events {
                self.observe_event(event);
            }
            if let Self::Messages(id) = self
                && let Ok(value) = serde_json::from_slice::<Value>(bytes)
                && let Some(value) = value.get("id").and_then(Value::as_str)
                && let Ok(mut id) = id.lock()
            {
                *id = Some(value.to_owned());
            }
        }
    }

    pub(super) fn response_id(&self) -> Option<String> {
        match self {
            Self::Responses(facts) => facts.response_id(),
            Self::Messages(id) => id.lock().ok().and_then(|id| id.clone()),
            Self::Count => None,
        }
    }

    pub(super) fn validate_buffered(&self, bytes: &[u8]) -> Result<(), ()> {
        match self {
            Self::Responses(facts) => validate_buffered_response(facts, bytes).map_err(|_| ()),
            Self::Messages(_) => {
                let value: Value = serde_json::from_slice(bytes).map_err(|_| ())?;
                let id = value.get("id").and_then(Value::as_str).ok_or(())?;
                if value.get("type").and_then(Value::as_str) != Some("message")
                    || !value.get("content").is_some_and(Value::is_array)
                    || self.response_id().is_some_and(|expected| expected != id)
                {
                    return Err(());
                }
                Ok(())
            }
            Self::Count => serde_json::from_slice::<Value>(bytes)
                .ok()
                .and_then(|value| value.get("input_tokens").and_then(Value::as_u64))
                .map(|_| ())
                .ok_or(()),
        }
    }
}

pub(super) enum GenerationDeliveryValidator {
    Responses(ResponsesDeliveryValidator),
    Messages { id: Option<String>, finished: bool },
}

impl GenerationDeliveryValidator {
    pub(super) fn new(protocol: GenerationProtocol) -> Self {
        match protocol {
            GenerationProtocol::Responses => Self::Responses(ResponsesDeliveryValidator::default()),
            _ => Self::Messages {
                id: None,
                finished: false,
            },
        }
    }

    pub(super) fn response_id(&self) -> Option<&str> {
        match self {
            Self::Responses(validator) => validator.response_id(),
            Self::Messages { id, .. } => id.as_deref(),
        }
    }

    pub(super) fn validate_frame(
        &mut self,
        bytes: &[u8],
        transformed: bool,
        facts: &GenerationValidationFacts,
    ) -> Result<(), ()> {
        match (self, facts) {
            (Self::Responses(validator), GenerationValidationFacts::Responses(facts)) => validator
                .validate_frame(bytes, transformed, facts)
                .map_err(|_| ()),
            (Self::Messages { id, finished }, GenerationValidationFacts::Messages(_)) => {
                if bytes.is_empty() {
                    return if *finished { Ok(()) } else { Err(()) };
                }
                for event in
                    parse_sse_events(std::str::from_utf8(bytes).map_err(|_| ())?).map_err(|_| ())?
                {
                    let value: Value = match serde_json::from_str(&event.data) {
                        Ok(value) => value,
                        Err(_) if !transformed => continue,
                        Err(_) => return Err(()),
                    };
                    let kind = value
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if *finished {
                        return Err(());
                    }
                    match kind {
                        "message_start" => {
                            let next = value
                                .pointer("/message/id")
                                .and_then(Value::as_str)
                                .ok_or(())?;
                            if id.is_some()
                                || (transformed
                                    && facts.response_id().is_some_and(|expected| expected != next))
                            {
                                return Err(());
                            }
                            *id = Some(next.to_owned());
                        }
                        "message_stop" => {
                            if id.is_none() {
                                return Err(());
                            }
                            *finished = true;
                        }
                        "error" => *finished = true,
                        _ => {}
                    }
                }
                Ok(())
            }
            _ => Err(()),
        }
    }

    pub(super) fn finish_delivery(&self) -> Result<(), ()> {
        match self {
            Self::Responses(validator) => validator.finish_delivery().map_err(|_| ()),
            Self::Messages { finished: true, .. } => Ok(()),
            _ => Err(()),
        }
    }
}
