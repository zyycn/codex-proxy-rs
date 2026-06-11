use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FingerprintUpdate {
    pub app_version: String,
    pub build_number: String,
}

#[derive(Debug, Error)]
pub enum FingerprintError {
    #[error("invalid update manifest: {0}")]
    InvalidManifest(#[from] serde_json::Error),
}

#[derive(Deserialize)]
struct Manifest {
    version: String,
    build_number: String,
}

pub fn parse_update_manifest(input: &str) -> Result<FingerprintUpdate, FingerprintError> {
    // 中文注释：自动更新只同步桌面端指纹字段，不把远端配置当作运行时业务配置执行。
    let manifest: Manifest = serde_json::from_str(input)?;
    Ok(FingerprintUpdate {
        app_version: manifest.version,
        build_number: manifest.build_number,
    })
}
