//! 管理端画像预览与仪表盘投影，不参与出站身份解析

use super::{ClientKind, Platform, ReleaseService, Selection, VersionMode, object};
use gateway_admin::{
    model::observability::{
        DashboardDesktopRelease, DashboardWireAttribute, DashboardWireProfile, DashboardWireTarget,
        DesktopReleaseStatus,
    },
    ports::provider::ProviderAdminError,
};
use gateway_core::account::OpaqueProviderData;
use serde_json::{Value, json};

pub(crate) fn options() -> OpaqueProviderData {
    let presets=[Platform::Macos,Platform::Linux,Platform::Windows].into_iter().flat_map(|platform|[ClientKind::Desktop,ClientKind::Cli].map(move|client|json!({"configuration":Selection {client,platform,..Selection::default()},"automaticAvailable":true,"reason":null}))).collect::<Vec<_>>();
    object(
        json!({"defaults":Selection::default(),"presets":presets,"defaultSystemPrompt":super::selection::DEFAULT_SYSTEM_PROMPT,"systemPromptSource":"Antigravity CLI 1.5.15"}),
    )
}
pub(crate) fn dashboard(selection: &Selection, releases: &ReleaseService) -> DashboardWireProfile {
    let snapshot = releases.snapshot(selection.target());
    let profile = selection.resolve(snapshot.version(selection.client));
    let fixed = selection.version_mode == VersionMode::Fixed;
    let release = (!fixed).then(|| DashboardDesktopRelease {
        status: if snapshot.failed {
            DesktopReleaseStatus::Failed
        } else if snapshot.release.is_none() {
            DesktopReleaseStatus::Unchecked
        } else {
            DesktopReleaseStatus::Current
        },
        checked_at: snapshot.checked,
        latest_version: snapshot.release.as_ref().map(|r| r.version.clone()),
        latest_build: snapshot.release.as_ref().and_then(|r| r.build.clone()),
        download_url: snapshot.release.as_ref().map(|r| r.url.clone()),
        published_at: None,
        minimum_system_version: None,
        hardware_requirements: None,
        download_size: None,
        signature_present: None,
        error: snapshot
            .failed
            .then(|| "无法读取 Google 官方客户端版本".into()),
    });
    DashboardWireProfile {
        provider: "antigravity".into(),
        product: profile.client.product().into(),
        version: profile.client_version.clone(),
        build: None,
        target: DashboardWireTarget {
            os_type: profile.platform.label().into(),
            os_version: String::new(),
            arch: profile.arch.label().into(),
            terminal: String::new(),
        },
        attributes: vec![
            DashboardWireAttribute {
                key: "client_identifier".into(),
                value: profile.client_identifier(),
            },
            DashboardWireAttribute {
                key: "version_policy".into(),
                value: if fixed { "fixed" } else { "latest" }.into(),
            },
        ],
        user_agent: profile.user_agent(),
        release,
        verified_at: snapshot
            .verified_at(&profile.client_version)
            .or_else(|| profile.verified_at()),
    }
}
pub(crate) fn preview(
    configuration: &OpaqueProviderData,
    releases: &ReleaseService,
) -> Result<OpaqueProviderData, ProviderAdminError> {
    let selection = Selection::parse(configuration)?;
    let view = dashboard(&selection, releases);
    let fixed = selection.version_mode == VersionMode::Fixed;
    let checked = view.release.as_ref().and_then(|r| r.checked_at);
    let error = view.release.as_ref().and_then(|r| r.error.as_ref());
    Ok(object(
        json!({"configuration":Value::Object(selection.configuration().into_inner()),"versionSource":if fixed {"custom"} else {"official"},"product":view.product,"osType":view.target.os_type,"arch":view.target.arch,"clientVersion":view.version,"userAgent":view.user_agent,"checkedAt":checked,"verifiedAt":view.verified_at,"error":error}),
    ))
}
