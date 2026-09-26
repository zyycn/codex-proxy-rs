//! Provider 账号领域、持久化端口与同一 target 内的账号选择。

mod error;
mod location;
pub use location::{InvalidRequestLocation, RequestLocation};
mod model;
mod model_access;
pub use model_access::{
    AccountModelAccess, AccountModelAccessMode, InvalidAccountModelAccess,
    MAX_ACCOUNT_ACCESS_MODELS,
};
mod proxy;
pub use proxy::{InvalidOutboundProxy, OutboundProxy};
pub mod scope;
mod selection;
mod smart_scheduling;
mod store;

pub use error::CredentialError;
pub use model::*;
pub(crate) use selection::smart_score;
pub use selection::*;
pub use smart_scheduling::{SmartSchedulingConfig, SmartSchedulingConfigError};
pub use store::ProviderAccountStore;
