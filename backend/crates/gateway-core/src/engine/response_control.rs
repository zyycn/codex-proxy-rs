//! 原上游连接的控制消息端口；不解释协议类型、不创建执行或延长连接生命周期

use std::fmt;
use std::sync::{Arc, Mutex, Weak};

use crate::lifecycle::CancellationToken;
use async_trait::async_trait;

/// Provider 实现的连接控制通道，消息内容由上游协议解释
#[async_trait]
pub trait ResponseControlTransport: Send + Sync {
    async fn send(&self, payload: &str) -> Result<(), ResponseControlUnavailable>;

    /// 仅在响应正文已结束后读取，调用方必须先取消此读取再开始下一轮执行
    async fn receive(&self) -> Result<String, ResponseControlUnavailable>;
}

/// 控制消息没有可用的原上游连接，不能通过重新选号或重试恢复
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponseControlUnavailable;

/// 单次根执行绑定的原连接；终态后仍可使用，连接销毁或重新绑定时失效
#[derive(Clone, Default)]
pub struct ResponseControl {
    target: Arc<Mutex<Option<Weak<dyn ResponseControlTransport>>>>,
    connection_cancellation: Option<CancellationToken>,
}

/// 某次已交付响应所属的弱连接快照，重绑后不能把旧消息发送到新连接
#[derive(Clone)]
pub struct ResponseControlBinding(Weak<dyn ResponseControlTransport>);

impl ResponseControlBinding {
    #[must_use]
    pub fn same_binding(&self, other: &Self) -> bool {
        self.0.ptr_eq(&other.0)
    }

    pub async fn send(&self, payload: &str) -> Result<(), ResponseControlUnavailable> {
        self.0
            .upgrade()
            .ok_or(ResponseControlUnavailable)?
            .send(payload)
            .await
    }
}

impl fmt::Debug for ResponseControl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ResponseControl([REDACTED])")
    }
}

impl ResponseControl {
    /// 下游连接跨多轮存续，不能使用单次响应的取消信号代替
    #[must_use]
    pub fn with_connection_cancellation(mut self, cancellation: CancellationToken) -> Self {
        self.connection_cancellation = Some(cancellation);
        self
    }

    #[must_use]
    pub fn connection_cancellation(&self) -> Option<&CancellationToken> {
        self.connection_cancellation.as_ref()
    }

    /// 仅保存弱引用，不延长旧 attempt 的连接生命周期
    #[must_use]
    pub fn binding(&self) -> Option<ResponseControlBinding> {
        self.target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .cloned()
            .map(ResponseControlBinding)
    }

    /// Provider 在请求成功写入上游后绑定；连接 owner 持有强引用并负责撤销
    pub fn bind(&self, transport: &Arc<dyn ResponseControlTransport>) {
        *self
            .target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::downgrade(transport));
    }

    pub fn clear(&self) {
        *self
            .target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    fn transport(&self) -> Option<Arc<dyn ResponseControlTransport>> {
        self.target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(Weak::upgrade)
    }

    pub async fn send(&self, payload: &str) -> Result<(), ResponseControlUnavailable> {
        self.transport()
            .ok_or(ResponseControlUnavailable)?
            .send(payload)
            .await
    }

    /// 未绑定时保持等待，供连接空闲循环与客户端输入共同 select
    pub async fn receive(&self) -> Result<String, ResponseControlUnavailable> {
        match self.transport() {
            Some(transport) => transport.receive().await,
            None => std::future::pending().await,
        }
    }
}
