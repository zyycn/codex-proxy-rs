//! 指纹更新任务接线。

use std::path::PathBuf;

use tracing::{info, warn};

use crate::upstream::fingerprint::{
    FingerprintRepository, RuntimeFingerprint, UpdateChecker, APPCAST_POLL_INTERVAL,
};

use super::{
    coordinator::SchedulerHandle,
    periodic::{spawn_periodic_task, PeriodicTaskConfig, PeriodicTaskRunner},
};

/// Codex Desktop 官方 appcast 地址。
pub const CODEX_DESKTOP_APPCAST_URL: &str =
    "https://persistent.oaistatic.com/codex-app-prod/appcast.xml";
/// 本地可选完整指纹提取文件路径。
pub const DEFAULT_EXTRACTED_FINGERPRINT_PATH: &str = "data/extracted-fingerprint.json";

/// 启动指纹自动更新后台任务。
pub fn start_fingerprint_update_task(
    repository: FingerprintRepository,
    runtime_fingerprint: RuntimeFingerprint,
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
    spawn_periodic_task(
        FingerprintUpdateTask {
            checker: update_checker,
            runtime_fingerprint,
            first_tick: true,
        },
        PeriodicTaskConfig::new(
            APPCAST_POLL_INTERVAL.as_secs(),
            "fingerprint 后台版本检查器已启动",
            "fingerprint 后台版本检查器已关闭",
        ),
    )
}

struct FingerprintUpdateTask {
    checker: UpdateChecker,
    runtime_fingerprint: RuntimeFingerprint,
    first_tick: bool,
}

impl PeriodicTaskRunner for FingerprintUpdateTask {
    fn tick(&mut self) -> super::periodic::TaskFuture<'_, ()> {
        Box::pin(async move {
            let first_tick = std::mem::take(&mut self.first_tick);
            match self.checker.check_and_apply_update().await {
                Ok(Some(fingerprint)) => {
                    let app_version = fingerprint.app_version.clone();
                    let build_number = fingerprint.build_number.clone();
                    self.runtime_fingerprint.replace(fingerprint);
                    info!(
                        app_version = %app_version,
                        build_number = %build_number,
                        "fingerprint 运行时版本已更新"
                    );
                }
                Ok(None) => {}
                Err(error) => {
                    if first_tick {
                        warn!(error = %error, "fingerprint 首次版本检查失败");
                    } else {
                        warn!(error = %error, "fingerprint 定期版本检查失败");
                    }
                }
            }
        })
    }
}
