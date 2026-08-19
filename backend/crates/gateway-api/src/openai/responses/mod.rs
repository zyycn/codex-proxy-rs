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
    ContinuationIntent, DecodedResponsesRequest, OpenAiRequestHeaders, ResponsesRequestMetadata,
    decode_request_with_headers,
};
pub use response::OpenAiResponsesEncoder;
pub(crate) use websocket::responses_websocket;
pub use websocket::{ResponseCreateFrameError, decode_response_create_with_context};

use gateway_core::event::ProviderResponseHeader;

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

pub(super) fn response_header_is_forwardable(name: &str, connection_options: &[String]) -> bool {
    let name = name.trim().to_ascii_lowercase();
    if connection_options
        .iter()
        .any(|option| option.eq_ignore_ascii_case(&name))
        || name.starts_with("sec-websocket-")
    {
        return false;
    }

    !matches!(
        name.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-connection"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "content-length"
            | "content-type"
            | "content-encoding"
            | "authorization"
            | "x-api-key"
            | "www-authenticate"
            | "authentication-info"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "proxy-authentication-info"
            | "cookie"
            | "cookie2"
            | "set-cookie"
            | "set-cookie2"
            | "chatgpt-account-id"
            | "chatgpt-organization-id"
            | "chatgpt-org-id"
            | "chatgpt-project-id"
            | "openai-organization"
            | "openai-project"
            | "x-openai-organization"
            | "x-openai-project"
            | "x-codex-installation-id"
            | "x-codex-turn-metadata"
    )
}
