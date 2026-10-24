//! 出站请求的类型化准备入口；一次 next 只返回准备结果

use std::fmt;

use crate::call::middleware::{HANDLE_METHOD, NEXT_METHOD, outbound as wire};
use crate::client::{CallCancellation, CallReply, HostClient, PluginCall, SessionError};
use crate::{CallContext, PluginFault, Stage};

use super::{MiddlewareInput, MiddlewareOutput, invalid_input};

/// HTTP 正文为内容编码后的实际字节，WebSocket 正文为完整 UTF-8 消息
#[derive(Clone, PartialEq, Eq)]
pub struct OutboundRequest {
    pub head: wire::RequestHead,
    pub body: Vec<u8>,
}

impl fmt::Debug for OutboundRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutboundRequest")
            .field("head", &self.head)
            .field("body_bytes", &self.body.len())
            .finish()
    }
}

/// 当前 attempt 的准备输入与单次续体，归属不从返回请求重建
pub struct OutboundCall {
    pub context: CallContext,
    pub metadata: wire::Metadata,
    pub request: OutboundRequest,
    pub next: OutboundNext,
    pub host: HostClient,
    pub cancellation: CallCancellation,
}

impl MiddlewareInput for OutboundCall {
    type Output = OutboundRequest;

    fn accepts(stage: Stage) -> bool {
        stage == Stage::Outbound
    }

    fn decode(call: PluginCall) -> Result<Self, PluginFault> {
        if call.method != HANDLE_METHOD || !Self::accepts(call.context.stage) {
            return Err(invalid_input());
        }
        let head: wire::CallHead =
            serde_json::from_value(call.params).map_err(|_| invalid_input())?;
        Ok(Self {
            context: call.context,
            metadata: head.metadata,
            request: OutboundRequest {
                head: head.request,
                body: call.payload,
            },
            next: OutboundNext {
                host: call.host.clone(),
            },
            host: call.host,
            cancellation: call.cancellation,
        })
    }
}

impl MiddlewareOutput for OutboundRequest {
    fn encode(self) -> Result<CallReply, PluginFault> {
        Ok(CallReply::unary(
            serde_json::to_value(self.head).map_err(|_| invalid_input())?,
            self.body,
        ))
    }
}

/// 消费续体只能进入下一层一次，不发送网络；外层仍可改写返回的完整请求
pub struct OutboundNext {
    host: HostClient,
}

impl OutboundNext {
    pub async fn run(self, request: OutboundRequest) -> Result<OutboundRequest, PluginFault> {
        let reply = self
            .host
            .call(
                NEXT_METHOD,
                serde_json::to_value(request.head).map_err(|_| invalid_input())?,
                request.body,
            )
            .await
            .map_err(SessionError::into_plugin_fault)?;
        Ok(OutboundRequest {
            head: serde_json::from_value(reply.result).map_err(|_| invalid_input())?,
            body: reply.payload,
        })
    }
}
