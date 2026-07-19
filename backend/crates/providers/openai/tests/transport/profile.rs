use chrono::{TimeZone, Utc};
use provider_openai::transport::profile::CodexWireProfile;

#[test]
fn wire_profile_should_generate_codex_core_user_agent() {
    let profile = CodexWireProfile {
        originator: "Codex Desktop".to_owned(),
        codex_version: "0.144.2".to_owned(),
        desktop_version: "26.707.72221".to_owned(),
        desktop_build: "72221".to_owned(),
        os_type: "Mac OS".to_owned(),
        os_version: "15.7.1".to_owned(),
        arch: "arm64".to_owned(),
        terminal: "unknown".to_owned(),
        verified_at: Utc
            .with_ymd_and_hms(2026, 7, 18, 0, 0, 0)
            .single()
            .expect("valid fixture time"),
    };

    assert_eq!(
        profile.user_agent(),
        "Codex Desktop/0.144.2 (Mac OS 15.7.1; arm64) unknown (Codex Desktop; 26.707.72221)"
    );
}
