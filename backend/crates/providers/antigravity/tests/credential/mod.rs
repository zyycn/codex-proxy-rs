//! 在凭据模块内验证 OAuth 叶子边界，并复用真实授权与刷新服务

#[path = "../../src/credential/authorization.rs"]
mod authorization;
#[path = "../../src/credential/documents.rs"]
mod documents;
#[path = "../../src/credential/model_groups.rs"]
mod model_groups;
#[path = "../../src/credential/oauth.rs"]
pub(crate) mod oauth;
#[path = "../../src/credential/refresh.rs"]
mod refresh;
#[path = "../../src/credential/service.rs"]
mod service;
pub(crate) use authorization::AuthorizationService;
pub(crate) use oauth::{GoogleOAuth, Material};
pub(crate) use service::CredentialService;

#[path = "documents.rs"]
mod documents_tests;
mod fixture;
#[path = "oauth.rs"]
mod oauth_tests;
pub(crate) use fixture::refreshed_credential;

#[path = "../../src/credential/catalog.rs"]
pub(crate) mod catalog;
#[path = "catalog.rs"]
mod catalog_tests;
#[path = "../../src/credential/quota/mod.rs"]
pub(crate) mod quota;
#[path = "quota/mod.rs"]
mod quota_tests;
