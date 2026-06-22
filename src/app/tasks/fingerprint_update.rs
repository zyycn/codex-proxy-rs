//! 指纹更新任务接线。

use std::path::PathBuf;

use crate::codex::fingerprint::{FingerprintRepository, UpdateChecker};

use super::coordinator::SchedulerHandle;

/// Codex Desktop 官方 appcast 地址。
pub const CODEX_DESKTOP_APPCAST_URL: &str =
    "https://persistent.oaistatic.com/codex-app-prod/appcast.xml";
/// 本地可选完整指纹提取文件路径。
pub const DEFAULT_EXTRACTED_FINGERPRINT_PATH: &str = "data/extracted-fingerprint.json";

/// 启动指纹自动更新后台任务。
pub fn start_fingerprint_update_task(
    repository: Option<FingerprintRepository>,
    appcast_url: String,
    extracted_fingerprint_path: PathBuf,
    current_version: String,
    current_build: String,
) -> SchedulerHandle {
    let update_checker = UpdateChecker::with_client(
        repository,
        reqwest::Client::new(),
        appcast_url,
        extracted_fingerprint_path,
        current_version,
        current_build,
    );
    SchedulerHandle::from_join_handle(update_checker.start_background_checker())
}
