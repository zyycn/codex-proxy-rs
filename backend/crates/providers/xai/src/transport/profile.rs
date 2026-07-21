//! Grok CLI runtime wire profile.

use std::{
    sync::{Arc, RwLock},
    time::Duration,
};

use chrono::{DateTime, Utc};
use futures::{StreamExt as _, future::BoxFuture};
use reqwest::{Client, redirect::Policy};
use serde::Deserialize;
use url::Url;

use crate::config::XaiWireProfileConfig;

#[derive(Debug, Clone)]
pub struct XaiWireProfileState(Arc<RwLock<XaiWireProfileConfig>>);

impl XaiWireProfileState {
    #[must_use]
    pub(crate) fn new(profile: XaiWireProfileConfig) -> Self {
        Self(Arc::new(RwLock::new(profile)))
    }

    #[must_use]
    pub fn client_identifier(&self) -> String {
        self.snapshot().client_identifier
    }
    #[must_use]
    pub fn client_version(&self) -> String {
        self.snapshot().client_version
    }
    #[must_use]
    pub fn client_mode(&self) -> String {
        self.snapshot().client_mode
    }
    #[must_use]
    pub fn target_os(&self) -> String {
        self.snapshot().target_os
    }
    #[must_use]
    pub fn target_arch(&self) -> String {
        self.snapshot().target_arch
    }
    #[must_use]
    pub fn verified_at(&self) -> DateTime<Utc> {
        self.snapshot().verified_at
    }

    #[must_use]
    pub fn user_agent(&self) -> String {
        let profile = self.snapshot();
        format!(
            "{}/{} ({}; {})",
            profile.client_identifier,
            profile.client_version,
            profile.target_os,
            profile.target_arch
        )
    }

    #[must_use]
    pub fn snapshot(&self) -> XaiWireProfileConfig {
        self.0
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn update_client_version(&self, version: &str) {
        let mut profile = self
            .0
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if profile.client_version != version {
            profile.client_version = version.to_owned();
        }
    }
}

pub const GROK_CLI_RELEASE_URL: &str = "https://registry.npmjs.org/@xai-official%2Fgrok/latest";
pub const GROK_CLI_RELEASE_POLL_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

const RELEASE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RELEASE_BYTES: usize = 64 * 1024;

pub trait GrokCliReleaseTransport: Send + Sync {
    fn fetch(&self) -> BoxFuture<'_, Result<String, GrokCliReleaseError>>;
}

#[derive(Clone)]
pub struct OfficialGrokCliReleaseTransport {
    client: Client,
    endpoint: Url,
}

impl OfficialGrokCliReleaseTransport {
    pub fn new() -> Result<Self, GrokCliReleaseError> {
        let endpoint =
            Url::parse(GROK_CLI_RELEASE_URL).map_err(|_| GrokCliReleaseError::InvalidEndpoint)?;
        let client = Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(RELEASE_TIMEOUT)
            .build()
            .map_err(|_| GrokCliReleaseError::ClientInitialization)?;
        Ok(Self { client, endpoint })
    }
}

impl GrokCliReleaseTransport for OfficialGrokCliReleaseTransport {
    fn fetch(&self) -> BoxFuture<'_, Result<String, GrokCliReleaseError>> {
        Box::pin(async move {
            let response = self.client.get(self.endpoint.clone()).send().await?;
            if !response.status().is_success() {
                return Err(GrokCliReleaseError::HttpStatus(response.status().as_u16()));
            }
            if response
                .content_length()
                .is_some_and(|size| size > MAX_RELEASE_BYTES as u64)
            {
                return Err(GrokCliReleaseError::ResponseTooLarge);
            }
            let mut body = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                if body
                    .len()
                    .checked_add(chunk.len())
                    .is_none_or(|size| size > MAX_RELEASE_BYTES)
                {
                    return Err(GrokCliReleaseError::ResponseTooLarge);
                }
                body.extend_from_slice(&chunk);
            }
            parse_release(&body)
        })
    }
}

#[derive(Clone)]
pub struct GrokCliReleaseService {
    profile: XaiWireProfileState,
    transport: Arc<dyn GrokCliReleaseTransport>,
}

impl GrokCliReleaseService {
    #[must_use]
    pub fn new(profile: XaiWireProfileState, transport: Arc<dyn GrokCliReleaseTransport>) -> Self {
        Self { profile, transport }
    }

    pub async fn refresh(&self) -> Result<String, GrokCliReleaseError> {
        let version = self.transport.fetch().await?;
        self.profile.update_client_version(&version);
        Ok(version)
    }
}

#[derive(Deserialize)]
struct NpmRelease {
    version: String,
}

fn parse_release(body: &[u8]) -> Result<String, GrokCliReleaseError> {
    let release: NpmRelease =
        serde_json::from_slice(body).map_err(|_| GrokCliReleaseError::InvalidDocument)?;
    if semver::Version::parse(&release.version).is_err() {
        return Err(GrokCliReleaseError::InvalidVersion);
    }
    Ok(release.version)
}

#[derive(Debug, thiserror::Error)]
pub enum GrokCliReleaseError {
    #[error("Grok CLI release client initialization failed")]
    ClientInitialization,
    #[error("Grok CLI release endpoint is invalid")]
    InvalidEndpoint,
    #[error("Grok CLI release request failed")]
    Request(#[from] reqwest::Error),
    #[error("Grok CLI release endpoint returned HTTP {0}")]
    HttpStatus(u16),
    #[error("Grok CLI release response exceeded the size limit")]
    ResponseTooLarge,
    #[error("Grok CLI release document is invalid")]
    InvalidDocument,
    #[error("Grok CLI release version is invalid")]
    InvalidVersion,
}
