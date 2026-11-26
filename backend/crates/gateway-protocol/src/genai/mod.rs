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

mod messages;
mod responses;
mod stream;
mod tokens;
pub use messages::messages_to_genai_request;
pub use responses::responses_to_genai_request;
pub use stream::GenAiToMessagesStream;
pub use tokens::{count_tokens_request, count_tokens_to_messages};
mod grounding;
pub use grounding::grounded_search_events;
