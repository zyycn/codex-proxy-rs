//! Provider 账号领域、持久化端口与同一 target 内的账号选择。

mod error;
mod model;
mod proxy;
pub use proxy::{InvalidOutboundProxy, OutboundProxy};
pub mod scope;
mod selection;
mod store;

pub use error::CredentialError;
pub use model::*;
pub use selection::*;
pub(crate) use selection::{SMART_SCORE_TOLERANCE, smart_score};
pub use store::ProviderAccountStore;
