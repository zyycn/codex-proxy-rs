#![deny(missing_docs)]

//! 适配器层，承载具体存储、HTTP、OAuth 和 WebSocket 实现。

pub mod codex;
pub mod oauth;
pub mod sqlite;
