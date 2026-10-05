//! 可选的插件侧异步传输与会话辅助；SDK 不启动网关服务

mod data;
mod frame;
mod http;
mod keys;
mod middleware;
mod plugin;
mod read;
mod resources;
mod session;
mod upstream_adapter;

use crate::{ErrorCode, PluginFault};
use serde::{Serialize, de::DeserializeOwned};

pub use frame::{read_frame, validate_frame, write_frame};
pub use http::{HostHttpBody, HostHttpResponse};
pub use middleware::{
    HttpBody, HttpCall, HttpFrame, HttpNext, HttpRequest, HttpResponse, MiddlewareBody,
    MiddlewareBodySender, MiddlewareCall, MiddlewareInput, MiddlewareNext, MiddlewareOutput,
    MiddlewarePlugin, MiddlewareRequest, MiddlewareResponse, MiddlewareResult, RequestCall,
    ServiceCall, ServiceNext, ServiceResponse, TypedServiceCall, WebSocketCall, WebSocketDirection,
    WebSocketKind, WebSocketMessage, WebSocketNext, WebSocketPayload, WebSocketSender,
    WebSocketSession,
};
pub use plugin::{
    AuthorError, ComposedPlugin, Empty, Method, PluginBuilder, TypedCall, TypedReply, methods,
};
pub use session::{
    CallCancellation, CallFuture, CallReply, HostClient, HostReply, PluginCall, PluginHandler,
    PluginSession, PullResponseFuture, PullResponseStream, ResponseStream, SessionConfig,
    SessionError, StreamSender,
};
pub use upstream_adapter::{UpstreamWebSocket, UpstreamWebSocketUpgrade};

async fn payload_call<T: Serialize, R: DeserializeOwned>(
    host: &HostClient,
    method: &str,
    query: T,
) -> Result<R, PluginFault> {
    let invalid = || PluginFault::new(ErrorCode::InvalidInput, "invalid host callback payload");
    let reply = host
        .call(
            method,
            serde_json::json!({}),
            serde_json::to_vec(&query).map_err(|_| invalid())?,
        )
        .await
        .map_err(SessionError::into_plugin_fault)?;
    if reply.result != serde_json::json!({}) {
        return Err(invalid());
    }
    serde_json::from_slice(&reply.payload).map_err(|_| invalid())
}
