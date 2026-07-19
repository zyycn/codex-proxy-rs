//! 网关协议编解码器。
//!
//! 本 crate 只处理 wire contract，不包含 Provider 认证、路由、重试或传输策略。

/// OpenAI wire contract。
pub mod openai;
