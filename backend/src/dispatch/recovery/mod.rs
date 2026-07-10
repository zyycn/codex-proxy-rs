//! 调度失败恢复策略。

pub(crate) mod auth;
pub mod cloudflare;
pub(crate) mod exhaustion;
pub mod implicit_resume;
pub mod reasoning_replay;
