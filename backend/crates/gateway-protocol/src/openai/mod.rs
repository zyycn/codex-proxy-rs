//! OpenAI wire contract。

/// OpenAI/Codex 事件语义、用量与限流字段编解码。
pub mod events;
/// OpenAI/Codex JSON schema 纯转换。
pub mod schema;
/// Server-Sent Events 帧的解析与编码。
pub mod sse;
