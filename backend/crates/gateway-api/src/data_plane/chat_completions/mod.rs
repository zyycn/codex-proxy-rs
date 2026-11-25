//! Chat Completions 与 Responses 之间的 API 协议适配

mod delivery;
mod http;
mod request;
mod response;

pub(super) use http::chat_completions;
