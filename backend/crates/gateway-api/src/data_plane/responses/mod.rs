//! OpenAI Responses 的透明 wire adapter 与 Core canonical facts 投影

mod error;
mod http;
pub(in crate::data_plane) mod request;
pub(in crate::data_plane) mod response;
pub(in crate::data_plane) mod validation;
pub mod websocket;

pub use crate::client_error::{ProtocolError, ProtocolErrorBody};
pub use error::ResponseEncodeError;
pub(crate) use http::responses;
pub(crate) use request::decode_request_with_body;
pub use request::{OpenAiRequestHeaders, decode_request_with_headers};
pub use response::OpenAiResponsesEncoder;
pub(crate) use websocket::responses_websocket;
pub use websocket::{ResponseCreateFrameError, decode_response_create_with_context};
