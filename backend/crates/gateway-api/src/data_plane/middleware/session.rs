//! 非流式响应在洋葱链返回后才提交；既有执行守卫保管唯一终结责任

use futures::future::BoxFuture;
use gateway_core::engine::middleware::{
    MiddlewareBody, MiddlewareError, MiddlewareFrame, MiddlewareHeader, MiddlewareResponse,
};

use gateway_core::engine::execution::ExecutionSession;

pub(crate) fn pending_execution_response(
    protocol: String,
    status: u16,
    headers: Vec<MiddlewareHeader>,
    frame: MiddlewareFrame,
    execution: PendingExecution,
) -> MiddlewareResponse {
    MiddlewareResponse::new(
        protocol,
        status,
        headers,
        Box::new(BufferedExecutionBody {
            frame: Some(frame),
            execution,
            commit_execution: (200..300).contains(&status),
        }),
    )
}

struct BufferedExecutionBody {
    frame: Option<MiddlewareFrame>,
    execution: PendingExecution,
    // 原生结果决定是否提交执行；外层改写状态不能把失败转成 Core 成功
    commit_execution: bool,
}

impl MiddlewareBody for BufferedExecutionBody {
    fn next_frame(&mut self) -> BoxFuture<'_, Result<Option<MiddlewareFrame>, MiddlewareError>> {
        // EOF 只表示单文档已读完；外层仍须验证正文并调用 commit，不能在此取消会话
        Box::pin(async { Ok(self.frame.take()) })
    }

    fn commit_downstream(
        &mut self,
        status: Option<u16>,
    ) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        Box::pin(async move {
            if !self.commit_execution {
                if let (Some(status), Some(session)) = (status, self.execution.session_mut()) {
                    session.record_client_status(status).await?;
                }
                self.execution.cancel_and_finalize().await;
                return Ok(());
            }
            let Some(session) = self.execution.session_mut() else {
                return Ok(());
            };
            if session.is_finalized() {
                if let Some(status) = status {
                    session.record_client_status(status).await?;
                }
            } else {
                session.commit_downstream(status).await?;
                if !session.is_finalized() {
                    return Err(MiddlewareError::InvalidState);
                }
            }
            self.execution.disarm();
            Ok(())
        })
    }

    fn record_client_status(&mut self, status: u16) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        Box::pin(async move {
            if let Some(session) = self.execution.session_mut() {
                session.record_client_status(status).await?;
            }
            Ok(())
        })
    }

    fn is_finalized(&self) -> bool {
        self.execution.is_finalized()
    }

    fn close(mut self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            self.execution.cancel_and_finalize().await;
        })
    }
}

pub(crate) async fn finalize_session(session: Box<dyn ExecutionSession>) {
    if session.is_finalized() {
        return;
    }
    session.cancel();
    session.detach_finalize().await;
}

pub(crate) struct PendingExecution {
    session: Option<Box<dyn ExecutionSession>>,
}

impl PendingExecution {
    pub(crate) fn new(session: Box<dyn ExecutionSession>) -> Self {
        Self {
            session: Some(session),
        }
    }

    pub(crate) fn session_mut(&mut self) -> Option<&mut (dyn ExecutionSession + 'static)> {
        self.session.as_deref_mut()
    }

    pub(crate) fn is_finalized(&self) -> bool {
        self.session
            .as_ref()
            .is_none_or(|session| session.is_finalized())
    }

    pub(crate) async fn cancel_and_finalize(&mut self) {
        if let Some(session) = self.session.take() {
            finalize_session(session).await;
        }
    }

    pub(crate) async fn record_response_status(
        &mut self,
        response: axum::response::Response,
    ) -> axum::response::Response {
        if let Some(session) = self.session.as_mut() {
            let _ = session
                .record_client_status(response.status().as_u16())
                .await;
        }
        response
    }

    pub(crate) fn disarm(&mut self) {
        self.session = None;
    }
}

impl Drop for PendingExecution {
    fn drop(&mut self) {
        let Some(session) = self.session.take() else {
            return;
        };
        session.trace().record(
            "downstream.cancelled",
            serde_json::json!({"reason": "response_guard_dropped"}),
        );
        session.cancel();
        let finalize = session.detach_finalize();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            drop(runtime.spawn(finalize));
        }
    }
}
