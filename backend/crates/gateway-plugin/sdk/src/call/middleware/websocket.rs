//! 双工消息使用二进制 payload 或惰性句柄，避免强制 JSON/UTF-8 解码
use super::MiddlewareHeader;
use serde::{Deserialize, Serialize};
pub const SEND_METHOD: &str = "host.middleware.send";
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Incoming,
    Outgoing,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Kind {
    Text,
    Binary,
    Ping,
    Pong,
    Close { code: Option<u16> },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Payload {
    Bytes,
    Handle { handle: String },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub kind: Kind,
    pub payload: Payload,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Call {
    pub connection_id: String,
    pub direction: Direction,
    pub headers: Vec<MiddlewareHeader>,
    pub message: Message,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Read {
    pub eof: bool,
}
