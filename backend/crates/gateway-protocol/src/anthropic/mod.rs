//! Messages 编解码、用量事实和 Responses 双向转换

mod capabilities;
mod content;
mod error;
mod request;
mod response;
mod session;
mod stream;
mod thinking;
mod tools;
mod usage;

pub use capabilities::MessagesModelCapabilities;
pub use error::TranslationError;
pub use request::{
    messages_to_responses_count_request, messages_to_responses_request,
    responses_to_messages_request, strip_foreign_reasoning_for_responses,
    strip_foreign_thinking_for_messages, validate_count_request, validate_request,
};
pub use response::{
    messages_to_responses_event, messages_to_responses_response, responses_to_messages_response,
};
pub use session::claude_session_id;
pub use stream::{
    MessagesAccumulator, MessagesToResponsesStream, ResponsesToMessagesStream,
    message_to_stream_events,
};
pub use thinking::messages_request_effort;
pub use usage::MessageUsage;

/// Messages 载荷的协议标识
pub const ANTHROPIC_PROTOCOL: &str = "anthropic";
