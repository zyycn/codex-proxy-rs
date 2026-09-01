//! Provider 账号领域、持久化端口与同一 target 内的账号选择。

mod error;
mod model;
mod selection;
mod store;

pub use error::CredentialError;
pub use model::*;
pub use selection::*;
pub use store::ProviderAccountStore;
