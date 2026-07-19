//! OpenAI Responses 的请求 decoder 与 canonical event encoder。
//!
//! 这里的“透明”指已声明语义逐项映射，而不是原始 JSON/SSE passthrough。
//! 未实现语义会返回稳定的 typed error，不能静默删除。

mod error;
mod http;
mod request;
mod response;
mod websocket;

pub use error::{ProtocolErrorBody, RequestDecodeError, ResponseEncodeError};
pub use http::{collect_execution_response, responses, stream_execution_response};
pub use request::{
    ContinuationIntent, DecodedResponsesRequest, PROVIDER_OPTIONS_VERSION,
    ResponsesRequestMetadata, decode_request,
};
pub use response::{CollectedResponses, ResponsesCollector};
pub use websocket::{
    ResponseCreateFrameError, ResponsesWebSocketAdapter, decode_response_create,
    responses_websocket,
};
