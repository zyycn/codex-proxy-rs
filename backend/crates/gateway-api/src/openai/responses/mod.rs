//! OpenAI Responses 的透明 wire adapter 与 Core canonical facts 投影

mod error;
mod http;
pub(in crate::openai) mod keepalive;
pub(in crate::openai) mod request;
pub(in crate::openai) mod response;
pub(in crate::openai) mod validation;
pub mod websocket;

pub use crate::client_error::{ProtocolError, ProtocolErrorBody};
pub use crate::openai::generation::http::{collect_execution_response, stream_execution_response};
pub use crate::openai::generation::{
    ContinuationIntent, DecodedGenerationRequest, GenerationRequestMetadata,
};
pub(crate) use crate::request_context::request_client_context;
pub use error::{RequestDecodeError, ResponseEncodeError};
pub(crate) use http::responses;
pub(in crate::openai) use request::decompress_request_body;
pub use request::{OpenAiRequestHeaders, decode_request_with_headers};
pub(crate) use request::{decode_request_with_body, passthrough_request_headers};
pub use response::OpenAiResponsesEncoder;
pub(crate) use websocket::responses_websocket;
pub use websocket::{ResponseCreateFrameError, decode_response_create_with_context};

use gateway_core::event::ProviderResponseHeader;
pub(super) use gateway_protocol::openai::response_header_is_forwardable;

pub(super) fn response_connection_options(headers: &[ProviderResponseHeader]) -> Vec<String> {
    headers
        .iter()
        .filter(|header| header.name().trim().eq_ignore_ascii_case("connection"))
        .filter_map(|header| std::str::from_utf8(header.value()).ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}
