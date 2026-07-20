//! OpenAI Responses 的透明 wire adapter 与 Core canonical facts 投影。

mod error;
mod http;
mod request;
mod response;
mod websocket;

pub use error::{ProtocolErrorBody, RequestDecodeError, ResponseEncodeError};
pub use http::{collect_execution_response, stream_execution_response};
pub(crate) use http::{responses, review_responses};
pub use request::{
    ContinuationIntent, DecodedResponsesRequest, PROVIDER_OPTIONS_VERSION,
    ResponsesRequestMetadata, decode_request,
};
pub use response::{CollectedResponses, OpenAiResponsesEncoder, ResponsesCollector};
pub(crate) use websocket::responses_websocket;
pub use websocket::{ResponseCreateFrameError, decode_response_create};

pub(super) fn safe_response_header_name(name: &str) -> Option<&'static str> {
    match name {
        "x-request-id" => Some("x-request-id"),
        "openai-model" => Some("openai-model"),
        "x-models-etag" => Some("x-models-etag"),
        "x-reasoning-included" => Some("x-reasoning-included"),
        "openai-processing-ms" => Some("openai-processing-ms"),
        _ => None,
    }
}
