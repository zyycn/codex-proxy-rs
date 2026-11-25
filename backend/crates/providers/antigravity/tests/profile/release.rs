//! 官方发布资料的来源、目标制品与歧义校验

use crate::profile::{
    Selection, release,
    selection::{ClientKind, Platform},
};

#[test]
fn desktop_release_selects_only_matching_official_artifact() {
    let prefix = "https://storage.googleapis.com/antigravity-public/antigravity-hub";
    let mac = format!("{prefix}/2.34.0-5887025308631040/darwin-arm/Antigravity.dmg");
    let page = format!(
        r#"<a href="{prefix}/2.99.0-7/darwin-x64/Antigravity.dmg"></a><a href="https://other.invalid/2.9.0-2/darwin-arm/Antigravity.dmg"></a><a href='{mac}'></a>"#
    );
    let target = Selection::default().target();
    let release = release::parse_desktop(&page, target).unwrap();
    assert_eq!(release.version, "2.34.0");
    assert_eq!(release.build.as_deref(), Some("5887025308631040"));
    assert!(
        release::parse_desktop(
            &format!(r#"{page}<a href="{prefix}/2.35.0-1/darwin-arm/Antigravity.dmg">"#),
            target
        )
        .is_none()
    );
    assert!(release::parse_desktop(&page.replace(&mac, ""), target).is_none());
}

#[test]
fn cli_manifest_requires_official_origin_and_matching_version() {
    for url in [
        "https://storage.googleapis.com/antigravity-public/antigravity-cli/1.5.15-123/linux-x64/cli_linux_x64.tar.gz",
        "https://other.invalid/antigravity-cli/1.5.15-123/linux-x64/cli_linux_x64.tar.gz",
        "https://storage.googleapis.com/antigravity-public/antigravity-cli/1.5.14-123/linux-x64/cli_linux_x64.tar.gz",
    ] {
        let body = serde_json::to_vec(
            &serde_json::json!({"version":"1.5.15", "url":url,"sha512":"a".repeat(128)}),
        )
        .unwrap();
        let target = Selection {
            client: ClientKind::Cli,
            platform: Platform::Linux,
            ..Selection::default()
        }
        .target();
        let release = release::parse_cli(&body, target);
        assert_eq!(
            release.is_ok(),
            url.contains("googleapis.com") && url.contains("1.5.15-123")
        );
    }
}
