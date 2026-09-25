//! 可选的插件侧异步传输与会话辅助；SDK 不启动网关服务。

mod data;
mod frame;
mod middleware;
mod plugin;
mod session;

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
