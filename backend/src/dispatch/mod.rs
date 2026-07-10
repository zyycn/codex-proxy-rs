//! 请求调度模块（账号选择、回退、恢复、会话亲和性）。

pub mod affinity;
pub mod cloudflare;
pub mod errors;
mod exhaustion;
pub mod implicit_resume;
pub mod reasoning_replay;
pub mod responses;
pub mod upstream;
