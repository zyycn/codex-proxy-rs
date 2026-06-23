//! OpenAI 兼容代理入口。
//!
//! 本模块负责：
//! - OpenAI API 协议转换（Chat Completions / Responses / Models）
//! - 请求调度（账号选择、回退、恢复）
//! - 会话亲和性与 reasoning replay

pub mod auth;
pub mod dispatch;
pub mod openai;
pub mod router;
