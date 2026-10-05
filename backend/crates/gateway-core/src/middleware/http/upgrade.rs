//! 升级保留 HTTP 返回路径；会话任务由最外层传输在提交握手时接管

use std::sync::{Arc, Mutex};

use futures::future::{BoxFuture, Shared};

use crate::{engine::middleware::MiddlewareError, middleware::websocket::Session};

pub type PendingSession =
    Shared<BoxFuture<'static, Result<Arc<dyn Session>, Arc<MiddlewareError>>>>;
pub type SessionTask = BoxFuture<'static, Result<(), MiddlewareError>>;

/// 具体服务器负责握手与 socket；Core 不依赖 HTTP 服务器实现
pub trait WebSocketUpgrade: Send + Sync {
    fn accept(
        &self,
        parts: http::request::Parts,
        protocols: Vec<String>,
    ) -> BoxFuture<'static, Result<(super::Response, PendingSession), MiddlewareError>>;
}

/// 响应被替换或丢弃时任务随之释放，不启动脱离请求的插件调用
#[derive(Clone)]
pub struct Upgraded {
    pub status: http::StatusCode,
    task: Arc<Mutex<Option<SessionTask>>>,
}

impl Upgraded {
    #[must_use]
    pub fn new(status: http::StatusCode, task: SessionTask) -> Self {
        Self {
            status,
            task: Arc::new(Mutex::new(Some(task))),
        }
    }

    pub fn take_task(&self) -> Option<SessionTask> {
        self.task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}
