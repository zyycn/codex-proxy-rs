//! 插件包校验与线协议适配；安装选择和制品接受事实由 Admin/Store 拥有

mod adapter;
mod callback;
mod contribution;
mod deprecation;
mod generation;
mod package;
mod rpc;
mod upstream_target;

pub use adapter::command_line::{PluginCommandError, PluginCommandOutput, PluginCommandSession};
pub use generation::{PluginRestartCircuitConfig, PluginRuntime, PluginRuntimeConfig};
pub use package::{
    PackageError, PackageInspector, PackageLimits, PreparedPackage, ValidatedPackage,
};
pub use rpc::{CallbackHandler, RpcError, RpcLimits, RpcReply, RpcSession, RpcStream};
