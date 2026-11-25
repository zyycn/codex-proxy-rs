//! 原生协议出口保留 wire，只在非流式交付时聚合消息正文

use axum::body::Bytes;
use gateway_core::{error::GatewayError, event::ProviderEvent};
use gateway_protocol::anthropic::ANTHROPIC_PROTOCOL;
use gateway_protocol::{
    anthropic::MessagesAccumulator,
    openai::sse::{
        DONE_SSE_FRAME, encode_sse_event_with_metadata, response_failed_sse_event_with_id,
    },
};
use serde_json::{Value, json};

use super::GenerationEndpoint;
use crate::data_plane::{
    error::gateway_error_contract,
    messages,
    responses::{OpenAiResponsesEncoder, ResponseEncodeError},
};

pub(super) enum GenerationEncoder {
    Responses(OpenAiResponsesEncoder),
    GenAi {
        failed: bool,
    },
    Messages {
        response_id: Option<String>,
        completed: bool,
        failed: bool,
    },
}

impl GenerationEncoder {
    pub(super) fn new(protocol: GenerationEndpoint) -> Self {
        match protocol {
            GenerationEndpoint::Responses => Self::Responses(OpenAiResponsesEncoder::new()),
            GenerationEndpoint::GenAi | GenerationEndpoint::GenAiTokenCount => {
                Self::GenAi { failed: false }
            }
            GenerationEndpoint::Messages | GenerationEndpoint::MessageTokenCount => {
                Self::Messages {
                    response_id: None,
                    completed: false,
                    failed: false,
                }
            }
        }
    }

    pub(super) fn push_sse(&mut self, event: &ProviderEvent) -> Vec<Bytes> {
        match self {
            Self::Responses(encoder) => encoder.push_sse(event),
            Self::GenAi { failed } => {
                let Some(wire) = event
                    .wire_event()
                    .filter(|wire| wire.protocol() == gateway_protocol::genai::GENAI_PROTOCOL)
                else {
                    return Vec::new();
                };
                *failed |= wire.data().get("error").is_some_and(Value::is_object);
                if let Some(raw) = wire.raw_sse_frame() {
                    return vec![raw.clone()];
                }
                if !wire.has_json_data() {
                    return Vec::new();
                }
                vec![Bytes::from(encode_sse_event_with_metadata(
                    wire.event_type().unwrap_or_default(),
                    &wire.data().to_string(),
                    wire.sse_id(),
                    wire.sse_retry(),
                ))]
            }
            Self::Messages {
                response_id,
                completed,
                failed,
            } => {
                let Some(wire) = event
                    .wire_event()
                    .filter(|wire| wire.protocol() == ANTHROPIC_PROTOCOL)
                else {
                    return Vec::new();
                };
                if let Some(raw) = wire.raw_json_body() {
                    let events = serde_json::from_slice(raw).ok().and_then(|value| {
                        gateway_protocol::anthropic::message_to_stream_events(value).ok()
                    });
                    let Some(events) = events else {
                        *failed = true;
                        return vec![messages::stream_error(
                            &json!({"error":{"message":"The upstream message is invalid."}}),
                        )];
                    };
                    *response_id = events
                        .first()
                        .and_then(|value| value.pointer("/message/id"))
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    *completed = true;
                    return events
                        .into_iter()
                        .map(|value| {
                            Bytes::from(encode_sse_event_with_metadata(
                                value["type"].as_str().unwrap_or_default(),
                                &value.to_string(),
                                None,
                                None,
                            ))
                        })
                        .collect();
                }
                let kind = wire
                    .event_type()
                    .or_else(|| wire.data().get("type").and_then(Value::as_str));
                match kind {
                    Some("message_start") => {
                        *response_id = wire
                            .data()
                            .pointer("/message/id")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    }
                    Some("message_stop") => *completed = true,
                    Some("error") => *failed = true,
                    _ => {}
                }
                if let Some(raw) = wire.raw_sse_frame() {
                    return vec![raw.clone()];
                }
                vec![Bytes::from(encode_sse_event_with_metadata(
                    kind.unwrap_or_default(),
                    &wire.data().to_string(),
                    wire.sse_id(),
                    wire.sse_retry(),
                ))]
            }
        }
    }

    pub(super) fn is_completed(&self) -> bool {
        match self {
            Self::Responses(encoder) => encoder.is_completed(),
            // 原生 GenAI 以 EOF 结束，finishReason 后仍可收到独立用量帧
            Self::GenAi { .. } => false,
            Self::Messages { completed, .. } => *completed,
        }
    }

    pub(super) fn has_wire_failure(&self) -> bool {
        match self {
            Self::Responses(encoder) => encoder.has_wire_failure(),
            Self::Messages { failed, .. } | Self::GenAi { failed } => *failed,
        }
    }

    pub(super) fn error_frame(&self, error: &GatewayError) -> Bytes {
        let (status, error_type, code) = gateway_error_contract(error.kind());
        match self {
            Self::Responses(encoder) => Bytes::from(response_failed_sse_event_with_id(
                encoder.response_id(),
                error.client_error_type().unwrap_or(error_type),
                crate::client_error::client_error_code(error.client_error_code().unwrap_or(code)),
                error.client_message(),
            )),
            Self::GenAi { .. } => {
                crate::data_plane::genai::error_frame(status, error.client_message())
            }
            Self::Messages { .. } => {
                let value = messages::error_payload(
                    &json!({"error":{"message":error.client_message()}}),
                    status,
                );
                messages::stream_error(&value)
            }
        }
    }
}

impl GenerationEndpoint {
    pub(super) const fn stream_error_encoder(self) -> Option<fn(&Value) -> Bytes> {
        match self {
            Self::Responses => None,
            Self::GenAi | Self::GenAiTokenCount => Some(crate::data_plane::genai::stream_error),
            Self::Messages | Self::MessageTokenCount => Some(messages::stream_error),
        }
    }

    pub(super) async fn project_error_response(
        self,
        response: axum::response::Response,
    ) -> axum::response::Response {
        match self {
            Self::Responses => response,
            Self::GenAi | Self::GenAiTokenCount => {
                crate::data_plane::genai::project_error_response(response).await
            }
            Self::Messages | Self::MessageTokenCount => {
                messages::project_error_response(response).await
            }
        }
    }

    pub(super) fn terminal_bytes(self) -> Bytes {
        match self {
            Self::Responses => Bytes::from_static(DONE_SSE_FRAME.as_bytes()),
            // Messages 的 message_stop 已在 wire 中交付，空帧只确认宿主交付终结
            Self::Messages | Self::MessageTokenCount | Self::GenAi | Self::GenAiTokenCount => {
                Bytes::new()
            }
        }
    }

    pub(super) fn collect(self, events: &[ProviderEvent]) -> Result<Vec<u8>, ResponseEncodeError> {
        if self == Self::Responses {
            return serde_json::to_vec(&crate::data_plane::responses::response::collect_response(
                events,
            )?)
            .map_err(|_| ResponseEncodeError::Serialization);
        }
        if matches!(self, Self::GenAi | Self::GenAiTokenCount) {
            let mut wires = events
                .iter()
                .filter_map(ProviderEvent::wire_event)
                .filter(|wire| wire.protocol() == self.wire());
            let wire = wires
                .next()
                .ok_or(ResponseEncodeError::MissingWireTerminal)?;
            if wires.next().is_some() {
                return Err(ResponseEncodeError::InvalidOutputItems);
            }
            if let Some(raw) = wire.raw_json_body() {
                if self == Self::GenAiTokenCount {
                    super::validation::GenerationValidationFacts::new(self)
                        .validate_buffered(raw)
                        .map_err(|_| ResponseEncodeError::InvalidOutputItems)?;
                }
                return Ok(raw.to_vec());
            }
            if self == Self::GenAiTokenCount
                && wire
                    .data()
                    .get("totalTokens")
                    .and_then(Value::as_u64)
                    .is_none()
            {
                return Err(ResponseEncodeError::InvalidOutputItems);
            }
            return serde_json::to_vec(wire.data()).map_err(|_| ResponseEncodeError::Serialization);
        }
        let mut accumulator = MessagesAccumulator::default();
        let mut response = None;
        for wire in events
            .iter()
            .filter_map(ProviderEvent::wire_event)
            .filter(|wire| wire.protocol() == self.wire())
        {
            if let Some(raw) = wire.raw_json_body() {
                return Ok(raw.to_vec());
            }
            let value = wire.data();
            if self == Self::MessageTokenCount {
                if value.get("input_tokens").and_then(Value::as_u64).is_some() {
                    response = Some(value.clone());
                }
            } else if value.get("type").and_then(Value::as_str) == Some("message") {
                response = Some(value.clone());
            } else if wire.has_json_data()
                && let Some(message) = accumulator
                    .push(value)
                    .map_err(|_| ResponseEncodeError::InvalidOutputItems)?
            {
                response = Some(message);
            }
        }
        serde_json::to_vec(&response.ok_or(ResponseEncodeError::MissingWireTerminal)?)
            .map_err(|_| ResponseEncodeError::Serialization)
    }
}
