//! 会话亲和身份、存储、解析与运行时服务。

pub mod identity;
pub mod resolve;
pub mod service;
pub mod store;
pub mod types;

pub use identity::AccountIdentityService;
pub(in crate::dispatch) use identity::{AccountIdentityScope, AccountScopedRequest};
pub use resolve::*;
pub use service::*;
pub use store::*;
pub use types::*;
