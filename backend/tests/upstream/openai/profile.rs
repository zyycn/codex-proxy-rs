#[test]
fn wire_profile_should_generate_codex_core_user_agent() {
    let profile = crate::support::wire_profile::test_wire_profile_value();

    assert_eq!(
        profile.user_agent(),
        "Codex Desktop/0.144.2 (Mac OS 15.7.1; arm64) unknown (Codex Desktop; 26.707.72221)"
    );
}
