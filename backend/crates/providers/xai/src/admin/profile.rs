//! 客户端版本与账号资料的管理展示投影

use crate::profile::GrokCliReleaseSnapshot;
use gateway_admin::model::observability::{DashboardDesktopRelease, DesktopReleaseStatus};

pub(super) fn dashboard_cli_release(
    profile_version: &str,
    snapshot: GrokCliReleaseSnapshot,
) -> DashboardDesktopRelease {
    let status = if snapshot.checked_at.is_none() {
        DesktopReleaseStatus::Unchecked
    } else if snapshot.last_error.is_some() {
        DesktopReleaseStatus::Failed
    } else if snapshot.latest_version.as_deref() == Some(profile_version) {
        DesktopReleaseStatus::Current
    } else if snapshot.latest_version.is_some() {
        DesktopReleaseStatus::UpdateAvailable
    } else {
        DesktopReleaseStatus::Failed
    };
    DashboardDesktopRelease {
        status,
        checked_at: snapshot.checked_at,
        latest_version: snapshot.latest_version,
        latest_build: None,
        published_at: None,
        minimum_system_version: None,
        hardware_requirements: None,
        download_url: None,
        download_size: None,
        signature_present: None,
        error: snapshot.last_error,
    }
}
