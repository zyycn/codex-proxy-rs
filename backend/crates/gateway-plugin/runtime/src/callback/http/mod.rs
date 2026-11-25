//! 插件 HTTP 出站、API 分派与中间件正文资源入口

mod dispatch;
pub(crate) mod middleware;
mod outbound;

pub(super) use dispatch::call;
pub(super) use outbound::{HttpCallbacks, http_error};
