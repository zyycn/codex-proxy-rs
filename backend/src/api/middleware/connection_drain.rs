//! 进程关闭时统一排空入站 HTTP 流与 WebSocket 任务。

use std::future::Future;

use axum::{
    body::Body,
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use futures::StreamExt;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// 入站连接排空控制器。
#[derive(Clone)]
pub struct ConnectionDrain {
    cancellation: CancellationToken,
    connection_tasks: TaskTracker,
}

impl Default for ConnectionDrain {
    fn default() -> Self {
        Self {
            cancellation: CancellationToken::new(),
            connection_tasks: TaskTracker::new(),
        }
    }
}

impl ConnectionDrain {
    /// 注册需要随进程关闭而终止的长连接任务。
    pub fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) {
        let cancellation = self.cancellation.clone();
        drop(self.connection_tasks.spawn(async move {
            cancellation.run_until_cancelled(future).await;
        }));
    }

    /// 停止所有受管连接，并返回关闭前仍在运行的长连接任务数。
    pub fn begin_shutdown(&self) -> usize {
        let active_websocket_connections = self.connection_tasks.len();
        self.connection_tasks.close();
        self.cancellation.cancel();
        active_websocket_connections
    }

    /// 等待所有受管长连接任务退出。
    pub async fn wait(&self) {
        self.connection_tasks.wait().await;
    }

    /// 是否已进入连接排空阶段。
    pub fn is_shutting_down(&self) -> bool {
        self.cancellation.is_cancelled()
    }
}

/// 将所有 HTTP 响应体接入统一关闭信号。
pub async fn drain_response_body(
    State(connection_drain): State<ConnectionDrain>,
    request: Request,
    next: Next,
) -> Response {
    let response = next.run(request).await;
    let (parts, body) = response.into_parts();
    let shutdown = connection_drain.cancellation.clone().cancelled_owned();
    let body = Body::from_stream(body.into_data_stream().take_until(shutdown));
    Response::from_parts(parts, body)
}
