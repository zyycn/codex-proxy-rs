//! OpenAI wire contract。

/// Codex Responses Lite 的 HTTP 请求头。
pub const X_OPENAI_INTERNAL_CODEX_RESPONSES_LITE_HEADER: &str =
    "x-openai-internal-codex-responses-lite";
/// Codex Responses Lite 在 WebSocket `client_metadata` 中的投影键。
pub const WS_REQUEST_HEADER_RESPONSES_LITE_CLIENT_METADATA_KEY: &str =
    "ws_request_header_x_openai_internal_codex_responses_lite";
/// Codex memory consolidation 请求标记。
pub const X_OPENAI_MEMGEN_REQUEST_HEADER: &str = "x-openai-memgen-request";

/// OpenAI/Codex 事件语义、用量与限流字段编解码。
pub mod events;
/// OpenAI/Codex JSON schema 纯转换。
pub mod schema;
/// Server-Sent Events 帧的解析与编码。
pub mod sse;
