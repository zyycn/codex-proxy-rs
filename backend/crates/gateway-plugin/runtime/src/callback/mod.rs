//! 插件宿主回调端口、调用作用域与分派的模块入口

mod accounts;
mod admin;
mod affinity;
mod call_resources;
mod client;
mod data;
mod dispatch;
pub(crate) mod error;
mod facts;
mod http;
mod http_dispatch;
pub(crate) mod http_middleware;
mod invocation;
mod keys;
mod log;
mod middleware;
mod model;
pub(crate) mod outbound;
mod ports;
pub(crate) mod private_state;
pub(crate) mod request_limits;
mod resources;
mod scope;
pub(crate) mod services;
pub(crate) mod upstream;
pub(crate) mod websocket_middleware;

pub(crate) use accounts::PluginAccountPortSlot;
pub(crate) use affinity::PluginAffinityPortSlot;
use call_resources::{CallResources, HttpStream};
pub(crate) use client::PluginClientIdentityPortSlot;
use gateway_plugin_sdk::{ErrorCode, PluginFault};
pub(crate) use keys::PluginClientKeyPortSlot;
pub(crate) use middleware::{
    MiddlewareBinding, MiddlewareBodyAuthority, MiddlewareCallback, MiddlewareCompletionBody,
    MiddlewareInvocation,
};
pub(crate) use model::PluginModelPortSlot;
pub(crate) use resources::PluginResourcePorts;
pub(crate) use scope::{CallbackScope, InvocationContext};

pub(crate) use ports::{PluginCallbackPorts, PluginCallbacks};

fn denied() -> PluginFault {
    PluginFault::new(
        ErrorCode::PermissionDenied,
        "callback resource is not authorized",
    )
}
fn invalid() -> PluginFault {
    PluginFault::new(ErrorCode::InvalidInput, "callback input is invalid")
}
