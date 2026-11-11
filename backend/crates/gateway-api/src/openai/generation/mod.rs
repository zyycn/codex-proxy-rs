//! 生成与计数协议共用的解码事实和 HTTP 执行边界

pub(crate) mod http;
mod request;
mod response;
mod validation;

pub(crate) use http::{
    GenerationHttpRequest, HttpResponseAdapter, ResponseAuthorization, execute_prepared_generation,
};
pub use request::{ContinuationIntent, DecodedGenerationRequest, GenerationRequestMetadata};

use axum::http::HeaderMap;
use gateway_core::{
    error::{GatewayError, GatewayErrorKind},
    operation::OperationKind,
};
use gateway_protocol::anthropic::ANTHROPIC_PROTOCOL;
use gateway_protocol::openai::OPENAI_PROTOCOL;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GenerationProtocol {
    Responses,
    Messages,
    MessageTokenCount,
}

impl GenerationProtocol {
    pub(crate) fn from_request(request: &DecodedGenerationRequest) -> Self {
        if request.operation().kind() == OperationKind::CountTokens {
            Self::MessageTokenCount
        } else if request.operation().protocol() == ANTHROPIC_PROTOCOL {
            Self::Messages
        } else {
            Self::Responses
        }
    }

    pub(crate) const fn wire(self) -> &'static str {
        match self {
            Self::Responses => OPENAI_PROTOCOL,
            Self::Messages | Self::MessageTokenCount => ANTHROPIC_PROTOCOL,
        }
    }

    pub(crate) const fn endpoint(self) -> &'static str {
        match self {
            Self::Responses => super::router::RESPONSES_PATH,
            Self::Messages => super::router::MESSAGES_PATH,
            Self::MessageTokenCount => super::router::MESSAGE_TOKENS_PATH,
        }
    }

    pub(crate) const fn operation(self) -> OperationKind {
        match self {
            Self::Responses | Self::Messages => OperationKind::Generate,
            Self::MessageTokenCount => OperationKind::CountTokens,
        }
    }

    pub(crate) fn decode(
        self,
        body: &[u8],
        headers: &HeaderMap,
        maximum: usize,
    ) -> Result<DecodedGenerationRequest, GatewayError> {
        match self {
            Self::Responses => super::responses::decode_request_with_headers(
                body, headers, maximum,
            )
            .map_err(|_| {
                GatewayError::new(
                    GatewayErrorKind::InvalidRequest,
                    "Request middleware produced an invalid Responses request.",
                )
            }),
            Self::Messages | Self::MessageTokenCount => {
                super::messages::decode_request(body, headers, maximum, self)
            }
        }
    }
}
