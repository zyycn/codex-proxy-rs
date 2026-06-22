//! Codex 协议类型。

/// Codex Chat 扩展字段。
pub mod chat;
/// Codex 事件语义与用量提取。
pub mod events;
/// Codex Responses / Compact 请求体。
pub mod responses;
/// Codex JSON schema 处理。
pub(crate) mod schema;
/// SSE 事件解析。
pub mod sse;
/// WebSocket 帧类型与纯编解码。
pub mod websocket;
