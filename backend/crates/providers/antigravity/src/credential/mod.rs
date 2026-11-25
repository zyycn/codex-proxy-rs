//! OAuth 流程、账号凭据与文件导入导出

mod authorization;
pub(crate) mod catalog;
mod documents;
pub(crate) mod oauth;
pub(crate) mod quota;
mod refresh;
mod service;
pub(crate) use authorization::AuthorizationService;
pub(crate) use oauth::{GoogleOAuth, Material};
pub(crate) use service::CredentialService;
