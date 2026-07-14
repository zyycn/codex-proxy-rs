//! 请求调度模块（账号选择、回退、恢复、会话亲和性）。

pub mod affinity;
pub(crate) mod controllers;
pub mod errors;
pub(crate) mod failure;
pub(crate) mod lifecycle;
pub(crate) mod routing;
pub(crate) mod service;
pub(crate) mod stream;
pub(crate) mod transport;
