//! OpenAI Responses 的透明 wire adapter 与 Core canonical facts 投影

mod error;
mod http;
mod request;
mod response;
mod validation;
pub mod websocket;

pub use error::{ProtocolError, ProtocolErrorBody, RequestDecodeError, ResponseEncodeError};
pub(crate) use http::{
    ResponseAuthorization, ResponsesHttpRequest, execute_prepared_responses,
    request_client_context, responses,
};
pub use http::{collect_execution_response, stream_execution_response};
pub(crate) use request::decode_request_with_body;
pub use request::{
    ContinuationIntent, DecodedResponsesRequest, OpenAiRequestHeaders, ResponsesRequestMetadata,
    decode_request_with_headers,
};
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
