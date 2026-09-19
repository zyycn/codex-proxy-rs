use gateway_core::account::OpaqueProviderData;
use provider_openai::transport::profile::selection::{
    ClientKind, ClientPlatform, ClientProfileSelection, VersionMode,
};
use provider_openai::transport::profile::{CodexBundledReleaseProfile, CodexWireProfileState};
use serde_json::json;

#[test]
fn six_presets_have_explicit_availability_and_cli_has_no_desktop_suffix() {
    let state = CodexWireProfileState::new(super::wire_profile());
    let options = state.selection_options().unwrap();
    let presets = options.expose_to_provider()["presets"].as_array().unwrap();
    assert_eq!(presets.len(), 6);
    assert!(
        presets
            .iter()
            .all(|preset| preset["automaticAvailable"] == true)
    );
    for platform in [
        ClientPlatform::Macos,
        ClientPlatform::Linux,
        ClientPlatform::Windows,
    ] {
        let selection = ClientProfileSelection {
            client: ClientKind::Cli,
            platform,
            ..Default::default()
        };
        let profile = selection.resolve(&state).unwrap();
        assert_eq!(profile.originator, "codex_cli_rs");
        assert_eq!(profile.os_type, platform.os_type());
        assert!(profile.desktop_version.is_empty());
        assert!(!profile.user_agent().contains("Desktop"));
        assert!(profile.user_agent().ends_with("unknown"));
    }
    for platform in [ClientPlatform::Linux, ClientPlatform::Windows] {
        for arch in ["arm64", "x86_64"] {
            let selection = ClientProfileSelection {
                platform,
                arch: Some(arch.into()),
                ..Default::default()
            };
            let profile = selection.resolve(&state).unwrap();
            assert_eq!(profile.os_type, platform.os_type());
            assert_eq!(profile.arch, arch);
            assert!(profile.desktop_version.starts_with("26."));
        }
    }
    let unsupported = ClientProfileSelection {
        arch: Some("x86_64".into()),
        ..Default::default()
    };
    assert!(
        unsupported.resolve(&state).is_err(),
        "macOS x86_64 cannot borrow the arm64 artifact"
    );
}

#[test]
fn fixed_and_frozen_profiles_survive_published_release_changes() {
    let state = CodexWireProfileState::new(super::wire_profile());
    let automatic = ClientProfileSelection::default();
    let frozen = automatic.resolve(&state).unwrap();
    let fixed = ClientProfileSelection {
        version_mode: VersionMode::Fixed,
        codex_version: Some("0.120.0".into()),
        desktop_version: Some("26.100.1".into()),
        desktop_build: Some("100".into()),
        platform: ClientPlatform::Windows,
        originator: Some("My Desktop".into()),
        ..Default::default()
    };
    let custom = fixed.resolve(&state).unwrap();
    state.update_bundled_release(&CodexBundledReleaseProfile {
        codex_version: "0.200.0".into(),
        desktop_version: "27.100.1".into(),
        desktop_build: "9000".into(),
        verified_at: chrono::Utc::now(),
    });
    assert_eq!(frozen.codex_version, "0.147.0-alpha.6.6");
    assert_eq!(automatic.resolve(&state).unwrap().codex_version, "0.200.0");
    assert_eq!(fixed.resolve(&state).unwrap(), custom);
    assert_eq!(custom.os_type, "Windows");
    let preview = state.preview_selection(&fixed.document().unwrap()).unwrap();
    assert_eq!(preview.expose_to_provider()["versionSource"], "custom");
    assert!(preview.expose_to_provider()["verifiedAt"].is_null());
}

#[test]
fn invalid_headers_and_mixed_version_modes_are_rejected() {
    let baseline = json!({"client":"cli", "platform":"linux", "versionMode":"latest"});
    for patch in [
        json!({"originator":"client\r\nx-injected: yes"}),
        json!({"terminal":"term; forged"}),
        json!({"arch":""}),
        json!({"codexVersion":"0.155.0"}),
        json!({"desktopVersion":"26.1.0"}),
        json!({"versionMode":"fixed"}),
        json!({"versionMode":"fixed", "codexVersion":"latest"}),
        json!({"client":"desktop", "versionMode":"fixed", "codexVersion":"0.153.4", "desktopVersion":"1.preview", "desktopBuild":"8109"}),
        json!({"client":"desktop", "versionMode":"fixed", "codexVersion":"0.153.4", "desktopVersion":"26.901.51231", "desktopBuild":"build"}),
        json!({"extraHeader":"secret"}),
    ] {
        let mut fields = baseline.as_object().unwrap().clone();
        fields.extend(patch.as_object().unwrap().clone());
        assert!(ClientProfileSelection::parse(&OpaqueProviderData::new(fields)).is_err());
    }
}
