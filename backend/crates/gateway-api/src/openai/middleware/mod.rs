//! OpenAI 请求洋葱链与最终传输交付；不拥有 Provider 选择或计量事实

mod http;
mod request;
mod session;

pub(crate) use http::{ExpectedBody, buffered_response, error_response, into_http_response};
pub(crate) use request::{RequestInput, invoke_request, query_response, request_parts};
pub(crate) use session::{PendingExecution, finalize_session, pending_execution_response};
