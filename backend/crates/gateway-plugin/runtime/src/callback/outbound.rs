//! 出站准备的单次续体与完整请求编解码，不发起网络请求

use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use gateway_core::engine::middleware::{MiddlewareHeader, outbound as core};
use gateway_plugin_sdk::{
    PluginFault,
    call::middleware::{MiddlewareHeader as WireHeader, NEXT_METHOD, outbound as wire},
};

use super::{MiddlewareCallback, invalid};
use crate::RpcReply;

pub(crate) struct Invocation(Mutex<Option<core::Next>>);

impl Invocation {
    pub(crate) fn new(next: core::Next) -> Arc<Self> {
        Arc::new(Self(Mutex::new(Some(next))))
    }
}

impl MiddlewareCallback for Invocation {
    fn invoke(
        self: Arc<Self>,
        method: String,
        params: serde_json::Value,
        payload: Vec<u8>,
        _maximum_payload: usize,
    ) -> BoxFuture<'static, Result<RpcReply, PluginFault>> {
        Box::pin(async move {
            if method != NEXT_METHOD {
                return Err(invalid());
            }
            let request_head = serde_json::from_value(params).map_err(|_| invalid())?;
            let request = decode(request_head, payload)?;
            let next = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
                .ok_or_else(invalid)?;
            let request = next
                .run(request)
                .await
                .map_err(|error| super::error::middleware(&error))?;
            Ok(RpcReply {
                result: serde_json::to_value(head(&request)).map_err(|_| invalid())?,
                payload: request.body.into(),
            })
        })
    }
}

pub(crate) fn head(request: &core::Request) -> wire::RequestHead {
    wire::RequestHead {
        target: match &request.target {
            core::Target::Http { method, uri } => wire::Target::Http {
                method: method.to_string(),
                uri: uri.to_string(),
            },
            core::Target::WebSocket { uri } => wire::Target::WebSocket {
                uri: uri.to_string(),
            },
        },
        headers: request
            .headers
            .iter()
            .map(|header| WireHeader {
                name: header.name().to_owned(),
                value: header.value().to_vec(),
            })
            .collect(),
    }
}

pub(crate) fn decode(
    head: wire::RequestHead,
    payload: Vec<u8>,
) -> Result<core::Request, PluginFault> {
    Ok(core::Request {
        target: match head.target {
            wire::Target::Http { method, uri } => core::Target::Http {
                method: method.parse().map_err(|_| invalid())?,
                uri: uri.parse().map_err(|_| invalid())?,
            },
            wire::Target::WebSocket { uri } => core::Target::WebSocket {
                uri: uri.parse().map_err(|_| invalid())?,
            },
        },
        headers: head
            .headers
            .into_iter()
            .map(|header| MiddlewareHeader::new(header.name, header.value.into()))
            .collect(),
        body: payload.into(),
    })
}
