//! 请求调度模块（账号选择、回退、恢复、会话亲和性）。

mod auth_recovery;
pub mod chat;
pub mod cloudflare;
pub mod errors;
mod exhaustion;
pub mod implicit_resume;
pub mod reasoning_replay;
pub mod responses;
pub mod session_affinity;
pub mod upstream;
mod usage_events;
