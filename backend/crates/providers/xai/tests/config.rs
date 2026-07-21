use std::path::Path;

use provider_xai::XaiConfig;

use crate::support::xai_config;

#[test]
fn xai_config_requires_and_freezes_the_explicit_wire_profile() {
    let mut config = xai_config();
    config
        .resolve_and_validate(Path::new("/srv/gateway"))
        .expect("valid xAI wire profile");

    let profile = config.wire_profile_state();
    assert_eq!(profile.client_version(), "0.2.106");
    assert_eq!(profile.user_agent(), "grok-shell/0.2.106 (linux; x86_64)");
    assert!(serde_json::from_str::<XaiConfig>("{}").is_err());
    assert!(serde_json::from_str::<XaiConfig>(
        r#"{"wire_profile":{"client_identifier":"grok-shell","client_version":"0.2.106","client_mode":"headless","target_os":"linux","target_arch":"x86_64","verified_at":"2026-07-21T00:00:00+08:00","unknown":true}}"#,
    )
    .is_err());
}
