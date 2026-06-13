use serde::Deserialize;
use thiserror::Error;

use crate::codex::gateway::fingerprint::repository::FingerprintRepository;

pub const CODEX_DESKTOP_UPDATE_SOURCE: &str = "codex_desktop_update_source";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FingerprintUpdate {
    pub app_version: String,
    pub build_number: String,
}

#[derive(Debug, Error)]
pub enum FingerprintError {
    #[error("invalid update manifest: {0}")]
    InvalidManifest(#[from] serde_json::Error),
    #[error("failed to fetch update manifest: {0}")]
    Http(#[from] reqwest::Error),
    #[error("failed to persist fingerprint update: {0}")]
    Database(#[from] sqlx::Error),
}

#[derive(Deserialize)]
struct Manifest {
    version: String,
    build_number: String,
}

pub fn parse_update_manifest(input: &str) -> Result<FingerprintUpdate, FingerprintError> {
    // 自动更新只同步桌面端指纹字段，不把远端配置当作运行时业务配置执行。
    let manifest: Manifest = serde_json::from_str(input)?;
    Ok(FingerprintUpdate {
        app_version: manifest.version,
        build_number: manifest.build_number,
    })
}

#[derive(Clone)]
pub struct FingerprintUpdater {
    client: reqwest::Client,
    repository: FingerprintRepository,
    update_url: String,
}

impl FingerprintUpdater {
    pub fn new(
        client: reqwest::Client,
        repository: FingerprintRepository,
        update_url: impl Into<String>,
    ) -> Self {
        Self {
            client,
            repository,
            update_url: update_url.into(),
        }
    }

    pub async fn poll_once(&self) -> Result<FingerprintUpdate, FingerprintError> {
        let manifest = self
            .client
            .get(&self.update_url)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        let update = parse_update_manifest(&manifest)?;
        self.repository.insert_update(&update).await?;
        Ok(update)
    }
}
