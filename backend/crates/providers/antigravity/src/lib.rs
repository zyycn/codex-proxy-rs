//! Antigravity Provider 的装配与宿主注册门面

mod admin;
mod billing;
mod credential;
mod profile;
mod provider;
mod transport;

use gateway_admin::ports::provider::ProviderBundle;
use gateway_core::{provider_ports::ProviderStorePorts, routing::ProviderKind};
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
#[error("Antigravity Provider 初始化失败")]
pub struct ConfigError;

pub fn initialize(ports: ProviderStorePorts) -> Result<ProviderBundle, ConfigError> {
    let kind = ProviderKind::new("antigravity").map_err(|_| ConfigError)?;
    let oauth = Arc::new(credential::GoogleOAuth::for_provider()?);
    let origin = oauth.resource_origin().clone();
    let release = Arc::new(profile::ReleaseService::new(ports.artifact_profiles())?);
    let credentials = Arc::new(credential::CredentialService::new(
        kind.clone(),
        &ports,
        oauth.clone(),
        release.clone(),
    ));
    let catalog = Arc::new(credential::catalog::CatalogService::new(
        kind.clone(),
        ports.accounts(),
        credentials.clone(),
        oauth.registration(),
        origin.clone(),
        release.clone(),
    ));
    let quota = Arc::new(credential::quota::QuotaService::new(
        kind.clone(),
        ports.accounts(),
        credentials.clone(),
        catalog.clone(),
        release.clone(),
        origin.clone(),
    ));
    let core = Arc::new(provider::AntigravityProvider::new(
        kind.clone(),
        &ports,
        credentials.clone(),
        catalog.clone(),
        quota.clone(),
        release.clone(),
        origin,
    ));
    let admin = Arc::new(admin::AntigravityAdmin {
        kind: kind.clone(),
        credentials: credentials.clone(),
        authorization: credential::AuthorizationService {
            kind,
            credentials: credentials.clone(),
            oauth,
            store: ports.oauth_pending(),
        },
        catalog: catalog.clone(),
        release: release.clone(),
        quota: quota.clone(),
    });
    let workers = provider::workers::contributions(credentials, catalog, release, quota)
        .map_err(|_| ConfigError)?;
    Ok(ProviderBundle {
        core,
        admin,
        workers,
    })
}
