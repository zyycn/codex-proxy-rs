//! Chat Completions 到既有 Responses 执行链的协议适配。

mod http;
mod request;
mod response;

pub(crate) use http::chat_completions;
pub(super) use response::ChatEncoder;
