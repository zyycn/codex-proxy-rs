//! Codex Desktop 官方发布观测任务接线。

use tracing::{info, warn};

use crate::upstream::openai::desktop_release::{
    APPCAST_POLL_INTERVAL, DesktopReleaseChecker, DesktopReleaseStatus,
};
use crate::upstream::openai::profile::CodexWireProfileState;

use super::{
    coordinator::SchedulerHandle,
    periodic::{PeriodicTaskConfig, PeriodicTaskRunner, spawn_periodic_task},
};

/// 启动 Desktop 官方发布检查后台任务。
pub fn start_desktop_release_update_task(
    status: DesktopReleaseStatus,
    wire_profile: CodexWireProfileState,
    appcast_url: String,
) -> SchedulerHandle {
    let checker = DesktopReleaseChecker::with_client(
        reqwest::Client::new(),
        appcast_url,
        status,
        wire_profile,
    );
    spawn_periodic_task(
        DesktopReleaseUpdateTask {
            checker,
            first_tick: true,
        },
        PeriodicTaskConfig::new(
            APPCAST_POLL_INTERVAL.as_secs(),
            "Codex Desktop 发布检查器已启动",
            "Codex Desktop 发布检查器已关闭",
        ),
    )
}

struct DesktopReleaseUpdateTask {
    checker: DesktopReleaseChecker,
    first_tick: bool,
}

impl PeriodicTaskRunner for DesktopReleaseUpdateTask {
    fn tick(&mut self) -> super::periodic::TaskFuture<'_, ()> {
        Box::pin(async move {
            let first_tick = std::mem::take(&mut self.first_tick);
            match self.checker.check_and_record().await {
                Ok(release) => info!(
                    version = %release.version,
                    build = %release.build,
                    "Codex Desktop 最新发布信息已刷新并已同步请求身份"
                ),
                Err(error) if first_tick => {
                    warn!(error = %error, "Codex Desktop 首次发布检查失败");
                }
                Err(error) => {
                    warn!(error = %error, "Codex Desktop 定期发布检查失败");
                }
            }
        })
    }
}
