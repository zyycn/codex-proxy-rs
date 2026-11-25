//! 生成与计数协议共用的解码事实和 HTTP 执行边界

pub(crate) mod http;
mod keepalive;
mod request;
mod response;
mod validation;

pub(crate) use http::{
    GenerationHttpRequest, HttpResponseAdapter, ResponseAuthorization, execute_prepared_generation,
};
pub use request::{ContinuationIntent, DecodedGenerationRequest, GenerationRequestMetadata};

use axum::http::HeaderMap;
use gateway_core::operation::OperationKind;
use gateway_protocol::anthropic::ANTHROPIC_PROTOCOL;
use gateway_protocol::openai::OPENAI_PROTOCOL;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GenerationEndpoint {
    Responses,
    Messages,
    MessageTokenCount,
    GenAi,
    GenAiTokenCount,
}

impl GenerationEndpoint {
    pub(crate) fn input(self, request: DecodedGenerationRequest) -> GenerationInput {
        GenerationInput {
            endpoint: self,
            model: request.metadata.requested_model,
            stream: request.metadata.stream,
        }
    }

    pub(crate) const fn wire(self) -> &'static str {
        match self {
            Self::Responses => OPENAI_PROTOCOL,
            Self::GenAi | Self::GenAiTokenCount => gateway_protocol::genai::GENAI_PROTOCOL,
            Self::Messages | Self::MessageTokenCount => ANTHROPIC_PROTOCOL,
        }
    }

    pub(crate) const fn path(self) -> &'static str {
        match self {
            Self::Responses => super::router::RESPONSES_PATH,
            Self::GenAi | Self::GenAiTokenCount => super::genai::MODEL_ACTION_PATH,
            Self::Messages => super::router::MESSAGES_PATH,
            Self::MessageTokenCount => super::router::MESSAGE_TOKENS_PATH,
        }
    }

    pub(crate) const fn operation(self) -> OperationKind {
        match self {
            Self::Responses | Self::Messages | Self::GenAi => OperationKind::Generate,
            Self::MessageTokenCount | Self::GenAiTokenCount => OperationKind::CountTokens,
        }
    }
}

pub(crate) struct GenerationInput {
    pub(super) endpoint: GenerationEndpoint,
    pub(super) model: String,
    pub(super) stream: bool,
}

impl GenerationInput {
    fn decode(
        &self,
        body: &axum::body::Bytes,
        headers: &HeaderMap,
        maximum: usize,
    ) -> Result<DecodedGenerationRequest, gateway_core::engine::middleware::MiddlewareError> {
        use gateway_core::engine::middleware::MiddlewareError;
        match self.endpoint {
            GenerationEndpoint::GenAi | GenerationEndpoint::GenAiTokenCount => {
                super::genai::decode(
                    &self.model,
                    self.stream,
                    self.endpoint,
                    body.clone(),
                    headers,
                )
                .map_err(MiddlewareError::Gateway)
            }
            GenerationEndpoint::Responses => {
                super::responses::decode_request_with_headers(body, headers, maximum)
                    .map_err(|_| MiddlewareError::Rejected)
            }
            GenerationEndpoint::Messages | GenerationEndpoint::MessageTokenCount => {
                super::messages::decode_request(body, headers, maximum, self.endpoint)
                    .map_err(|_| MiddlewareError::Rejected)
            }
        }
    }
}
