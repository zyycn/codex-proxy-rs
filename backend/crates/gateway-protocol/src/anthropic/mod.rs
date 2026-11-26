//! Messages 原生解析、用量事实和流聚合

mod capabilities;
mod error;
mod request;
mod session;
mod stream;
mod thinking;
mod usage;

pub use capabilities::MessagesModelCapabilities;
pub use error::MessageError;
pub use request::{validate_count_request, validate_request};
pub use session::claude_session_id;
pub use stream::{MessagesAccumulator, message_to_stream_events};
pub use thinking::messages_request_effort;
pub use usage::MessageUsage;

/// Messages 载荷的协议标识
pub const ANTHROPIC_PROTOCOL: &str = "anthropic";
