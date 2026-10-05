//! 插件双向 RPC 会话、调用结果与生命周期的公共入口

mod dispatch;
mod session;

pub use session::{CallbackHandler, RpcError, RpcLimits, RpcReply, RpcSession};
pub(crate) use session::{RpcSessionDiagnostic, RpcSessionLifecycle, Shared};
