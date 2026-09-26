//! 可选的插件侧异步传输与会话辅助；SDK 不启动网关服务。

mod data;
mod frame;
mod middleware;
mod plugin;
mod resources;
mod session;

use crate::{ErrorCode, PluginFault};
use serde::{Serialize, de::DeserializeOwned};

pub use frame::{read_frame, validate_frame, write_frame};
pub use middleware::{
    MiddlewareBody, MiddlewareBodySender, MiddlewareCall, MiddlewareNext, MiddlewarePlugin,
    MiddlewareRequest, MiddlewareResponse,
};
pub use plugin::{
    AuthorError, ComposedPlugin, Empty, Method, PluginBuilder, TypedCall, TypedReply, methods,
};
pub use session::{
    CallCancellation, CallFuture, CallReply, HostClient, HostReply, PluginCall, PluginHandler,
    PluginSession, ResponseStream, SessionConfig, SessionError, StreamSender,
};

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
