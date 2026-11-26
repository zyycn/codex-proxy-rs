//! Chat 原生请求事实及按目标协议隔离的直接转换

mod error;
mod request;
mod response;

pub mod genai;
pub mod messages;
pub mod responses;

pub use error::ChatError;
pub use request::{custom_tool_schema, parse_request, validate_request};
pub(crate) use request::{request_effort, request_facts};
pub use response::{ChatAccumulator, ChatIdentity, completion_chunks};

/// Chat Completions 线协议标识
pub const CHAT_PROTOCOL: &str = "chat_completions";
