//! 请求调度模块（账号选择、回退、恢复、会话亲和性）。

pub mod affinity;
mod attempts;
pub mod errors;
mod recording;
pub mod recovery;
pub(crate) mod service;
pub(crate) mod stream;
pub mod upstream_call;
