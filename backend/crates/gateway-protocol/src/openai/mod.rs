//! 共享的 OpenAI 线协议类型、解析与编码入口

/// OpenAI 线协议载荷与事件的协议标识
pub const OPENAI_PROTOCOL: &str = "openai";

/// Codex Responses Lite 的 HTTP 请求头
pub const X_OPENAI_INTERNAL_CODEX_RESPONSES_LITE_HEADER: &str =
    "x-openai-internal-codex-responses-lite";
/// Codex Responses Lite 在 WebSocket `client_metadata` 中的投影键
pub const WS_REQUEST_HEADER_RESPONSES_LITE_CLIENT_METADATA_KEY: &str =
    "ws_request_header_x_openai_internal_codex_responses_lite";
/// Codex memory consolidation 请求标记
pub const X_OPENAI_MEMGEN_REQUEST_HEADER: &str = "x-openai-memgen-request";

/// OpenAI Responses 请求中携带的 Codex 专属语义
pub mod codex;
/// OpenAI/Codex 事件语义、用量与限流字段编解码
pub mod events;
mod headers;
/// Server-Sent Events 帧的解析与编码
pub mod sse;

pub use codex::{
    CodexResponsesRequestSemantics, codex_account_session_id, codex_account_thread_id,
    codex_responses_request_semantics, codex_responses_request_semantics_with_turn_metadata,
    codex_session_id, codex_thread_id, codex_turn_id, turn_metadata_session_id,
    turn_metadata_thread_id, turn_metadata_turn_id,
};
pub use headers::{
    is_transport_managed_request_header, parse_retry_after_seconds, response_header_is_forwardable,
};
