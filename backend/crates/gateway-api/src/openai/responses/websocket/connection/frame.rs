//! 下游连接的帧、写入结果、退出原因与队列预算

use std::{sync::Arc, time::Duration};
use thiserror::Error;
use tokio::sync::OwnedSemaphorePermit;

pub(super) const OUTBOUND_COMMAND_BUFFER: usize = 32;
pub(super) const INBOUND_EVENT_BUFFER: usize = 32;
pub(super) const DOWNSTREAM_WRITE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
pub(super) const DOWNSTREAM_CLOSE_TIMEOUT: Duration = Duration::from_secs(1);
pub(super) const CONNECTION_MAX_AGE: Duration = Duration::from_secs(60 * 60);
pub(super) const DOWNSTREAM_PING_INTERVAL: Duration = Duration::from_secs(25);
pub(super) const DOWNSTREAM_PONG_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const DOWNSTREAM_PING_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// WebSocket pump 的写入与生命周期预算
#[derive(Clone, Copy)]
pub struct ConnectionConfig {
    pub(super) write_timeout: Duration,
    pub(super) max_age: Duration,
}

impl ConnectionConfig {
    /// 生产环境固定预算
    pub const PRODUCTION: Self = Self {
        write_timeout: DOWNSTREAM_WRITE_TIMEOUT,
        max_age: CONNECTION_MAX_AGE,
    };
}

/// 业务层可观察的客户端输入；Ping/Pong 始终由 pump 消费
pub enum ConnectionEvent {
    Text(String),
    Binary,
    Expired,
    Exited(PumpExitReason),
}

/// 接收队列与活动期间暂存的请求共用容量；移动帧不释放它占用的名额
pub(in super::super) struct PendingConnectionEvent {
    pub(in super::super) event: ConnectionEvent,
    pub(super) _permit: Option<OwnedSemaphorePermit>,
}

impl PendingConnectionEvent {
    pub(super) fn exited(reason: PumpExitReason) -> Self {
        Self {
            event: ConnectionEvent::Exited(reason),
            _permit: None,
        }
    }
}

/// 下游写入阶段；名称刻意使用 write，而不是暗示客户端已消费的 delivery
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramePhase {
    Metadata,
    First,
    Data,
    Terminal,
    FirstAndTerminal,
    Error,
    ConnectionLimit,
    Close,
}

impl FramePhase {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::First => "first",
            Self::Data => "data",
            Self::Terminal => "terminal",
            Self::FirstAndTerminal => "first_and_terminal",
            Self::Error => "error",
            Self::ConnectionLimit => "connection_limit",
            Self::Close => "close",
        }
    }

    pub(super) const fn is_milestone(self) -> bool {
        matches!(
            self,
            Self::First
                | Self::Terminal
                | Self::FirstAndTerminal
                | Self::Error
                | Self::ConnectionLimit
                | Self::Close
        )
    }
}

/// 一次下游写入的请求归属与协议阶段
#[derive(Clone)]
pub struct WriteContext {
    pub(super) request_id: Option<Arc<str>>,
    pub(super) phase: FramePhase,
}

impl WriteContext {
    /// 创建归属于某个请求的写入上下文
    #[must_use]
    pub fn request(request_id: &Arc<str>, phase: FramePhase) -> Self {
        Self {
            request_id: Some(Arc::clone(request_id)),
            phase,
        }
    }

    /// 创建连接级写入上下文
    #[must_use]
    pub const fn connection(phase: FramePhase) -> Self {
        Self {
            request_id: None,
            phase,
        }
    }
}

/// WebSocket pump 停止的稳定原因
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PumpExitReason {
    ClientClose,
    PeerEof,
    ReadError,
    WriteError,
    WriteTimeout,
    PongTimeout,
    LifecycleShutdown,
    ConnectionMaxAge,
    CoordinatorDropped,
    InboundOverload,
    ServerClose,
    PumpStopped,
    MiddlewareFailed,
}

impl PumpExitReason {
    pub(in super::super) const fn as_str(self) -> &'static str {
        match self {
            Self::ClientClose => "client_close",
            Self::PeerEof => "peer_eof",
            Self::ReadError => "read_error",
            Self::WriteError => "write_error",
            Self::WriteTimeout => "write_timeout",
            Self::PongTimeout => "pong_timeout",
            Self::LifecycleShutdown => "lifecycle_shutdown",
            Self::ConnectionMaxAge => "connection_max_age",
            Self::CoordinatorDropped => "coordinator_dropped",
            Self::InboundOverload => "inbound_overload",
            Self::ServerClose => "server_close",
            Self::PumpStopped => "pump_stopped",
            Self::MiddlewareFailed => "middleware_failed",
        }
    }
}

/// 下游 WebSocket 写入失败
#[derive(Debug, Error)]
pub enum ConnectionWriteError {
    #[error("downstream WebSocket pump is closed")]
    Closed,
    #[error("downstream WebSocket write timed out after {timeout:?}")]
    Timeout { timeout: Duration },
    #[error("downstream WebSocket transport write failed: {message}")]
    Transport { message: String },
}

/// 只有实际 transport 写入才记为 Written；插件丢弃消息不伪造写入成功
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteOutcome {
    Written,
    Suppressed,
}
