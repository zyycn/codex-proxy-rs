//! 双工消息边界，不解析消息正文或第三方控制协议
use crate::{
    engine::middleware::{MiddlewareError, MiddlewareHeader},
    lifecycle::CancellationToken,
};
use bytes::Bytes;
use futures::future::BoxFuture;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Incoming,
    Outgoing,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Binary,
    Ping,
    Pong,
    Close { code: Option<u16> },
}
#[derive(Clone, Debug)]
pub struct Message {
    pub kind: Kind,
    pub payload: Bytes,
}

/// 等待实际 transport 写入；与接收端分开持有，不在插件调用期间锁住 writer
pub trait Sender: Send + Sync {
    fn send(&self, message: Message) -> BoxFuture<'_, Result<(), MiddlewareError>>;
}
/// 一个接收端和独立发送端；接收等待不能阻塞发送
pub trait Session: Sender {
    fn receive(&self) -> BoxFuture<'_, Result<Option<Message>, MiddlewareError>>;
}

#[derive(Clone)]
pub struct Context {
    pub plan: Option<crate::engine::middleware::FrozenMiddlewarePlan>,
    pub connection_id: String,
    pub direction: Direction,
    pub headers: Arc<[MiddlewareHeader]>,
    pub cancellation: CancellationToken,
    pub sender: Arc<dyn Sender>,
}
pub type Next = super::Next<Message, Option<Message>, MiddlewareError>;
