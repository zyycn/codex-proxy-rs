//! 网关协议编解码器
//!
//! 本 crate 只处理 wire contract，不包含 Provider 认证、路由、重试或传输策略

/// OpenAI wire contract
pub mod openai;

/// Anthropic Messages 与 Responses 的线协议转换
pub mod anthropic;

/// 协议请求中的共享能力事实
pub mod request;

/// CPR 互联只读观测协议
pub mod peer;
