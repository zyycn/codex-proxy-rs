//! 请求画像快照与身份规则测试

use super::*;

#[test]
fn default_identity_should_match_official_grok_shell() {
    let profile = XaiWireProfile::default();

    assert_eq!(profile.client_identifier, "grok-shell");
    assert_eq!(profile.user_agent(), "grok-shell/1.0.13 (linux; x86_64)");
}

#[test]
fn user_agent_should_normalize_arm64_like_official_cli() {
    let profile = XaiWireProfile {
        target_os: "macos".to_owned(),
        target_arch: "arm64".to_owned(),
        ..XaiWireProfile::default()
    };

    assert_eq!(profile.user_agent(), "grok-shell/1.0.13 (macos; aarch64)");
}
