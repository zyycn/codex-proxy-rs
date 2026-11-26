//! Google 原生生成协议的请求与响应事实

mod request;
mod response;

pub(crate) use request::{request_facts, thinking_level};
pub use response::{
    GenerateContentObservation, GenerationFinish, UsageMetadata, has_finish_signal,
    primary_text_parts,
};

/// Google 原生请求与响应使用同一 wire 标识
pub const GENAI_PROTOCOL: &str = "genai";

mod tokens;
pub use tokens::{CountTokensError, count_tokens_request};
