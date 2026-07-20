use std::path::Path;

use chrono::{TimeZone as _, Utc};
use provider_openai::config::{CodexWireProfileConfig, OpenAiConfig};

#[test]
fn openai_config_builds_the_audited_wire_profile() {
    let mut config = valid_config();
    config
        .resolve_and_validate(Path::new("/srv/gateway"))
        .expect("valid OpenAI config");
    let profile = config.wire_profile_state().snapshot();

    assert_eq!(
        profile.user_agent(),
        "Codex Desktop/0.102.0 (macOS 15.5.0; arm64) xterm-256color (Codex Desktop; 1.2026.190)"
    );
    assert_eq!(profile.desktop_build, "19012345678");
}

#[test]
fn openai_config_rejects_noncanonical_versions_and_empty_fields() {
    let mut config = valid_config();
    config.wire_profile.codex_version = "latest".to_owned();
    assert!(
        config
            .resolve_and_validate(Path::new("/srv/gateway"))
            .is_err()
    );

    let mut config = valid_config();
    config.wire_profile.desktop_version = "1.preview".to_owned();
    assert!(
        config
            .resolve_and_validate(Path::new("/srv/gateway"))
            .is_err()
    );

    let mut config = valid_config();
    config.wire_profile.originator.clear();
    assert!(
        config
            .resolve_and_validate(Path::new("/srv/gateway"))
            .is_err()
    );
}

fn valid_config() -> OpenAiConfig {
    OpenAiConfig {
        wire_profile: CodexWireProfileConfig {
            originator: "Codex Desktop".to_owned(),
            codex_version: "0.102.0".to_owned(),
            desktop_version: "1.2026.190".to_owned(),
            desktop_build: "19012345678".to_owned(),
            os_type: "macOS".to_owned(),
            os_version: "15.5.0".to_owned(),
            arch: "arm64".to_owned(),
            terminal: "xterm-256color".to_owned(),
            verified_at: Utc
                .with_ymd_and_hms(2026, 7, 19, 0, 0, 0)
                .single()
                .expect("valid test time"),
        },
    }
}
