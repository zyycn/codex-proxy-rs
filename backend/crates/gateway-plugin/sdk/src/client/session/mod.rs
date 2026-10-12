//! 插件侧双向 RPC 会话的公开合同与内部职责入口

mod call;
mod config;
mod credit;
mod deadline;
mod driver;
mod error;
mod lifecycle;
mod output;
mod registry;
mod stream;

pub use call::{CallFuture, CallReply, PluginCall, PluginHandler};
pub use config::SessionConfig;
pub use deadline::{CallDeadline, CallDeadlinePause};
pub use driver::PluginSession;
pub use error::SessionError;
pub use lifecycle::CallCancellation;
pub use registry::{HostClient, HostReply};
pub use stream::{PullResponseFuture, PullResponseStream, ResponseStream, StreamSender};
