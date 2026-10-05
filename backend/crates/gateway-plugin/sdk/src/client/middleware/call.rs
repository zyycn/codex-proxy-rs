//! 多边界插件只注册一次处理器，按类型选择当前调用视图
use super::super::session::{CallReply, PluginCall};
use super::{
    HttpCall, HttpResponse, MiddlewareInput, MiddlewareOutput, MiddlewareResponse, RequestCall,
    ServiceCall, ServiceResponse, WebSocketCall, WebSocketMessage,
};
use crate::{CallContext, PluginFault, Stage};

pub enum MiddlewareCall {
    Service(Box<ServiceCall>),
    Http(Box<HttpCall>),
    WebSocket(Box<WebSocketCall>),
    Request(Box<RequestCall>),
}
pub enum MiddlewareResult {
    Service(ServiceResponse),
    Http(HttpResponse),
    WebSocket(Option<WebSocketMessage>),
    Request(MiddlewareResponse),
}
impl MiddlewareCall {
    #[must_use]
    pub fn context(&self) -> &CallContext {
        match self {
            Self::Service(call) => &call.context,
            Self::Http(call) => &call.context,
            Self::WebSocket(call) => &call.context,
            Self::Request(call) => &call.context,
        }
    }
    /// 原样进入下游，供当前处理器不感兴趣的调用复用
    pub async fn forward(self) -> Result<MiddlewareResult, PluginFault> {
        match self {
            Self::Service(call) => call.forward().await.map(MiddlewareResult::Service),
            Self::Http(call) => call
                .next
                .run(call.request)
                .await
                .map(MiddlewareResult::Http),
            Self::WebSocket(call) => call
                .next
                .run(call.message)
                .await
                .map(MiddlewareResult::WebSocket),
            Self::Request(call) => call
                .next
                .run(call.request)
                .await
                .map(MiddlewareResult::Request),
        }
    }
}
impl MiddlewareInput for MiddlewareCall {
    type Output = MiddlewareResult;
    fn accepts(stage: Stage) -> bool {
        ServiceCall::accepts(stage)
            || HttpCall::accepts(stage)
            || WebSocketCall::accepts(stage)
            || RequestCall::accepts(stage)
    }
    fn decode(call: PluginCall) -> Result<Self, PluginFault> {
        match call.context.stage {
            Stage::Service => ServiceCall::decode(call).map(Box::new).map(Self::Service),
            Stage::Http => HttpCall::decode(call).map(Box::new).map(Self::Http),
            Stage::WebSocket => WebSocketCall::decode(call)
                .map(Box::new)
                .map(Self::WebSocket),
            _ => RequestCall::decode(call).map(Box::new).map(Self::Request),
        }
    }
}
impl MiddlewareOutput for MiddlewareResult {
    fn encode(self) -> Result<CallReply, PluginFault> {
        match self {
            Self::Service(response) => response.encode(),
            Self::Http(response) => response.encode(),
            Self::WebSocket(response) => response.encode(),
            Self::Request(response) => response.encode(),
        }
    }
}
impl From<HttpResponse> for MiddlewareResult {
    fn from(response: HttpResponse) -> Self {
        Self::Http(response)
    }
}
impl From<Option<WebSocketMessage>> for MiddlewareResult {
    fn from(response: Option<WebSocketMessage>) -> Self {
        Self::WebSocket(response)
    }
}
impl From<MiddlewareResponse> for MiddlewareResult {
    fn from(response: MiddlewareResponse) -> Self {
        Self::Request(response)
    }
}

impl From<ServiceResponse> for MiddlewareResult {
    fn from(response: ServiceResponse) -> Self {
        Self::Service(response)
    }
}
