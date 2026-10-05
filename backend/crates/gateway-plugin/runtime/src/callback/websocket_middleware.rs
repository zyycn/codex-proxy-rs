//! 每次消息调用拥有自己的续体和正文句柄，发送端由连接 owner 持有
use super::MiddlewareCallback;
use crate::RpcReply;
use bytes::Bytes;
use futures::future::BoxFuture;
use gateway_core::middleware::websocket as core;
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::middleware::{
        BODY_CLOSE_METHOD, BODY_READ_METHOD, MiddlewareBodyClose, MiddlewareBodyRead, NEXT_METHOD,
        websocket as wire,
    },
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

pub(crate) struct Invocation {
    next: Mutex<Option<core::Next>>,
    payloads: Mutex<BTreeMap<String, Bytes>>,
    sender: Arc<dyn core::Sender>,
}
impl Invocation {
    pub(crate) fn new(next: core::Next, sender: Arc<dyn core::Sender>) -> Arc<Self> {
        Arc::new(Self {
            next: Mutex::new(Some(next)),
            payloads: Mutex::new(BTreeMap::new()),
            sender,
        })
    }
    pub(crate) fn session(sender: Arc<dyn core::Sender>) -> Arc<Self> {
        Arc::new(Self {
            next: Mutex::new(None),
            payloads: Mutex::new(BTreeMap::new()),
            sender,
        })
    }
    pub(crate) fn owns_payload(&self, handle: &str) -> bool {
        self.payloads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(handle)
    }
    pub(crate) fn encode(&self, message: core::Message) -> Result<wire::Message, PluginFault> {
        let mut payloads = self
            .payloads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if payloads.len() >= 16 {
            return Err(PluginFault::new(
                ErrorCode::Capacity,
                "WebSocket payload capacity reached; consume or close pending messages",
            ));
        }
        let handle = uuid::Uuid::new_v4().to_string();
        payloads.insert(handle.clone(), message.payload);
        Ok(wire::Message {
            kind: wire_kind(message.kind),
            payload: wire::Payload::Handle { handle },
        })
    }
    pub(crate) fn decode(
        &self,
        message: wire::Message,
        payload: Vec<u8>,
    ) -> Result<core::Message, PluginFault> {
        let payload = match message.payload {
            wire::Payload::Bytes => Bytes::from(payload),
            wire::Payload::Handle { handle } if payload.is_empty() => self
                .payloads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&handle)
                .ok_or_else(unavailable)?,
            _ => return Err(invalid()),
        };
        Ok(core::Message {
            kind: kind(message.kind),
            payload,
        })
    }
}
impl MiddlewareCallback for Invocation {
    fn invoke(
        self: Arc<Self>,
        method: String,
        params: serde_json::Value,
        payload: Vec<u8>,
        maximum_payload: usize,
    ) -> BoxFuture<'static, Result<RpcReply, PluginFault>> {
        Box::pin(async move {
            let result = match method.as_str() {
                NEXT_METHOD => {
                    let message = self.decode(
                        serde_json::from_value(params).map_err(|_| invalid())?,
                        payload,
                    )?;
                    let next = self
                        .next
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take()
                        .ok_or_else(unavailable)?;
                    let result = next
                        .run(message)
                        .await
                        .map_err(|error| super::error::middleware(&error))?;
                    serde_json::to_value(result.map(|message| self.encode(message)).transpose()?)
                        .map_err(|_| invalid())?
                }
                wire::SEND_METHOD => {
                    let message = self.decode(
                        serde_json::from_value(params).map_err(|_| invalid())?,
                        payload,
                    )?;
                    self.sender
                        .send(message)
                        .await
                        .map_err(|error| super::error::middleware(&error))?;
                    serde_json::json!({})
                }
                BODY_READ_METHOD if payload.is_empty() => {
                    let read: MiddlewareBodyRead =
                        serde_json::from_value(params).map_err(|_| invalid())?;
                    let maximum = usize::try_from(read.maximum_bytes)
                        .map_err(|_| invalid())?
                        .min(maximum_payload);
                    if maximum == 0 {
                        return Err(invalid());
                    }
                    let mut payloads = self
                        .payloads
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let bytes = payloads.get_mut(&read.handle).ok_or_else(unavailable)?;
                    let eof = bytes.is_empty();
                    let payload = bytes.split_to(maximum.min(bytes.len())).to_vec();
                    if eof {
                        payloads.remove(&read.handle);
                    }
                    return Ok(RpcReply {
                        result: serde_json::to_value(wire::Read { eof }).map_err(|_| invalid())?,
                        payload,
                    });
                }
                BODY_CLOSE_METHOD if payload.is_empty() => {
                    let close: MiddlewareBodyClose =
                        serde_json::from_value(params).map_err(|_| invalid())?;
                    self.payloads
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .remove(&close.handle);
                    serde_json::json!({})
                }
                _ => return Err(invalid()),
            };
            Ok(RpcReply {
                result,
                payload: Vec::new(),
            })
        })
    }
}
fn wire_kind(kind: core::Kind) -> wire::Kind {
    match kind {
        core::Kind::Text => wire::Kind::Text,
        core::Kind::Binary => wire::Kind::Binary,
        core::Kind::Ping => wire::Kind::Ping,
        core::Kind::Pong => wire::Kind::Pong,
        core::Kind::Close { code } => wire::Kind::Close { code },
    }
}
fn kind(kind: wire::Kind) -> core::Kind {
    match kind {
        wire::Kind::Text => core::Kind::Text,
        wire::Kind::Binary => core::Kind::Binary,
        wire::Kind::Ping => core::Kind::Ping,
        wire::Kind::Pong => core::Kind::Pong,
        wire::Kind::Close { code } => core::Kind::Close { code },
    }
}
fn invalid() -> PluginFault {
    PluginFault::new(
        ErrorCode::InvalidInput,
        "WebSocket middleware input is invalid",
    )
}
fn unavailable() -> PluginFault {
    PluginFault::new(
        ErrorCode::Conflict,
        "WebSocket resource was consumed or closed",
    )
}
