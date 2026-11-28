//! 网关协议编解码器
//!
//! 本 crate 只处理 wire contract，不包含 Provider 认证、路由、重试或传输策略

mod capabilities;

/// 跨协议 HTTP 传输头与重试时间
pub mod http;

/// OpenAI wire contract
pub mod openai;

/// Google GenAI 原生生成协议
pub mod genai;

/// Anthropic Messages 原生解析、计量与流聚合
pub mod anthropic;

/// 协议请求中的共享能力事实
pub mod request;

/// CPR 互联只读观测协议
pub mod peer;

/// Chat Completions 直接转换
pub mod chat_completions;

/// 原生协议与 Chat 的系统指令定位及局部替换
pub mod prompt_rewrite;
