//! 升级保留 HTTP 返回路径；会话任务由最外层传输在提交握手时接管

use std::sync::{Arc, Mutex};

use futures::future::{BoxFuture, Shared};

use crate::{engine::middleware::MiddlewareError, engine::middleware::websocket::Session};

pub type PendingSession =
    Shared<BoxFuture<'static, Result<Arc<dyn Session>, Arc<MiddlewareError>>>>;
pub type SessionTask = BoxFuture<'static, Result<(), SessionError>>;

/// 升级后的终态由传输 owner 消费，协议适配器只提供安全分类与诊断归属
#[derive(Debug)]
pub enum SessionError {
    /// 预期取消，不作为异常终态输出
    Cancelled,
    /// 下层已记录同一失败，传输 owner 不重复输出
    Reported,
    Failed {
        kind: &'static str,
        plugin_instance_id: Option<String>,
    },
}

impl From<MiddlewareError> for SessionError {
    fn from(error: MiddlewareError) -> Self {
        use crate::error::GatewayErrorKind;
        if matches!(&error, MiddlewareError::Gateway(error) if error.kind() == GatewayErrorKind::Cancelled)
            || matches!(
                error,
                MiddlewareError::Engine(crate::engine::EngineError::Cancelled)
            )
        {
            return Self::Cancelled;
        }
        Self::Failed {
            kind: if error.is_rejected() {
                "rejected"
            } else {
                "middleware"
            },
            plugin_instance_id: None,
        }
    }
}

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
