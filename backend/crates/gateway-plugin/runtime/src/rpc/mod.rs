//! 插件双向 RPC 会话、调用结果与生命周期的公共入口

mod admission;
mod calls;
mod contract;
mod dispatch;
mod session;

pub(crate) use calls::Shared;
pub use contract::{CallbackHandler, RpcError, RpcLimits, RpcReply};
pub(crate) use contract::{RpcSessionDiagnostic, RpcSessionLifecycle, RpcSessionLoad};
pub use session::RpcSession;
