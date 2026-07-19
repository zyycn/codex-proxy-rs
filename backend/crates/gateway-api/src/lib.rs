//! 客户端协议 adapter。
//!
//! 本 crate 只负责把外部 wire contract 转换为 [`gateway_core`] 的稳定
//! operation/event contract。它不执行路由、Provider 调用、重试或持久化。

#![forbid(unsafe_code)]

pub mod admin;
pub mod openai;
