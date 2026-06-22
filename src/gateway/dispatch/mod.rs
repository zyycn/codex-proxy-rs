//! 请求调度模块（账号选择、回退、恢复、会话亲和性）。

pub mod chat;
pub mod cloudflare;
pub mod implicit_resume;
pub mod reasoning_replay;
pub mod responses;
pub mod session_affinity;
pub mod upstream_errors;
pub mod upstream_requests;
