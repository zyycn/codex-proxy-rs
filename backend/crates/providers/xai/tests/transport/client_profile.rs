use gateway_core::account::OpaqueProviderData;
use provider_xai::transport::client_profile::{GrokClientProfileSelection, VersionMode};
use provider_xai::{XaiWireProfile, XaiWireProfileState};
use serde_json::json;

#[test]
fn client_profile_preserves_environment_and_fixed_version_independently_of_release() {
    let initial = XaiWireProfile::default();
    let mut selection = GrokClientProfileSelection {
        target_os: "windows".to_owned(),
        target_arch: "arm64".to_owned(),
        ..Default::default()
    };
    let state = XaiWireProfileState::new(XaiWireProfile {
        client_version: "2.0.0".to_owned(),
        ..initial
    });
    let automatic = selection.resolve(&state).unwrap();
    assert_eq!(automatic.client_version, "2.0.0");
    assert_eq!(automatic.target_os, "windows");
    selection.version_mode = VersionMode::Fixed;
    selection.client_version = Some("1.0.13".to_owned());
    let fixed = selection.resolve(&state).unwrap();
    assert_eq!(
        XaiWireProfileState::new(fixed).user_agent(),
        "grok-shell/1.0.13 (windows; aarch64)"
    );
    assert_eq!(state.client_version(), "2.0.0");
    let document = selection.document().unwrap();
    assert_eq!(
        GrokClientProfileSelection::parse(&document).unwrap(),
        selection
    );
}

#[test]
fn client_profile_rejects_header_injection_and_inconsistent_version_modes() {
    let valid = serde_json::to_value(GrokClientProfileSelection::default()).unwrap();
    for (field, invalid) in [
        ("clientIdentifier", json!("Grok\r\nAuthorization: injected")),
        ("clientMode", json!("")),
        ("targetOs", json!("linux; injected")),
        ("targetArch", json!("x".repeat(65))),
        ("clientVersion", json!("1.0.13")),
        ("versionMode", json!("fixed")),
        ("unknown", json!(true)),
    ] {
        let mut document = valid.clone();
        document[field] = invalid;
        assert!(
            GrokClientProfileSelection::parse(&OpaqueProviderData::new(
                document.as_object().unwrap().clone()
            ))
            .is_err(),
            "{field}"
        );
    }
}
