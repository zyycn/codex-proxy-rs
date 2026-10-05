//! 已选账号的受管 WebSocket；连接归属与跨轮续接由宿主管理

use tokio::sync::{Mutex, OnceCell};

use super::{
    HostClient, HostReply, SessionError,
    http::{callback, empty_callback, invalid},
    read::PendingRead,
};
use crate::{
    ErrorCode, PluginFault,
    call::{
        host::HttpResponse,
        upstream_adapter::{
            UpstreamWebSocketMessage, UpstreamWebSocketRead, UpstreamWebSocketRequest,
            WebSocketMessageKind,
        },
    },
};

/// 握手拒绝仍保留 HTTP 状态及正文，供适配器解析上游错误
pub enum UpstreamWebSocketUpgrade {
    Connected {
        headers: Vec<(String, String)>,
        connection: UpstreamWebSocket,
    },
    Rejected {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
}

/// 本次调用的连接入口；读写可同时进行，有先后依赖的发送须按顺序等待
///
/// 结束调用时宿主关闭连接；只有成功终态携带连接内续接状态时才保留连接
/// 因此丢弃此对象不会提前关闭宿主尚待确认的续接连接
pub struct UpstreamWebSocket {
    host: HostClient,
    closed: OnceCell<()>,
    read: Mutex<PendingRead<Result<HostReply, SessionError>>>,
}

impl HostClient {
    /// 建立连接，续接时复用宿主恢复的同一连接
    ///
    /// # Errors
    /// 目标、握手或续接归属无效时失败
    pub async fn upstream_websocket(
        &self,
        request: UpstreamWebSocketRequest,
    ) -> Result<UpstreamWebSocketUpgrade, PluginFault> {
        let (response, body): (HttpResponse, _) =
            callback(self, "host.upstream.websocket.open", request, vec![]).await?;
        if response.stream.is_some() || (response.status == 101 && !body.is_empty()) {
            return Err(invalid());
        }
        Ok(if response.status == 101 {
            UpstreamWebSocketUpgrade::Connected {
                headers: response.headers,
                connection: UpstreamWebSocket {
                    host: self.clone(),
                    closed: OnceCell::new(),
                    read: Mutex::new(PendingRead::default()),
                },
            }
        } else {
            UpstreamWebSocketUpgrade::Rejected {
                status: response.status,
                headers: response.headers,
                body,
            }
        })
    }
}

impl UpstreamWebSocket {
    /// 发送完整文本或二进制消息
    ///
    /// # Errors
    /// 连接关闭、消息无效、网络失败或期限到达时失败
    pub async fn send(&self, kind: WebSocketMessageKind, body: Vec<u8>) -> Result<(), PluginFault> {
        if self.closed.get().is_some() {
            return Err(PluginFault::new(
                ErrorCode::InvalidInput,
                "WebSocket is closed",
            ));
        }
        empty_callback(
            &self.host,
            "host.upstream.websocket.send",
            UpstreamWebSocketMessage { kind },
            body,
        )
        .await
    }

    /// 按需读取完整消息，EOF 后再次读取返回 None；Ping/Pong 由宿主处理
    /// 取消等待不丢弃消息；同时只允许一个读取者，发送和关闭不受读取等待阻塞
    ///
    /// # Errors
    /// 父调用结束、网络失败或期限到达时失败
    pub async fn read(&self) -> Result<Option<(WebSocketMessageKind, Vec<u8>)>, PluginFault> {
        if self.closed.get().is_some() {
            return Ok(None);
        }
        let mut pending = self
            .read
            .try_lock()
            .map_err(|_| PluginFault::new(ErrorCode::Capacity, "WebSocket already has a reader"))?;
        let reply = pending
            .run(|| {
                let host = self.host.clone();
                async move {
                    host.call(
                        "host.upstream.websocket.read",
                        serde_json::json!({}),
                        vec![],
                    )
                    .await
                }
            })
            .await
            .map_err(SessionError::into_plugin_fault)?;
        let read: UpstreamWebSocketRead =
            serde_json::from_value(reply.result).map_err(|_| invalid())?;
        let body = reply.payload;
        match (read.eof, read.kind) {
            (true, None) if body.is_empty() => {
                let _ = self.closed.set(());
                Ok(None)
            }
            (false, Some(kind)) => Ok(Some((kind, body))),
            _ => Err(invalid()),
        }
    }

    /// 提前关闭并放弃连接内续接；并发关闭等待同一次回调，失败或取消后可重试
    ///
    /// # Errors
    /// 父调用已结束或宿主拒绝释放时失败
    pub async fn close(&self) -> Result<(), PluginFault> {
        self.closed
            .get_or_try_init(|| async {
                empty_callback(
                    &self.host,
                    "host.upstream.websocket.close",
                    serde_json::json!({}),
                    vec![],
                )
                .await
            })
            .await?;
        // 活跃读取由宿主关闭唤醒；不等待读锁，避免 close 与 read 互相等待
        if let Ok(mut pending) = self.read.try_lock() {
            pending.clear();
        }
        Ok(())
    }
}
