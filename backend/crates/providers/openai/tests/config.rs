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
        "Codex Desktop/1.2026.190 (Mac OS; arm64)"
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

#[test]
fn openai_config_normalizes_the_legacy_macos_wire_label() {
    let mut config = valid_config();
    config.wire_profile.os_type = "macOS".to_owned();
    config
        .resolve_and_validate(Path::new("/srv/gateway"))
        .expect("legacy macOS label remains supported");

    assert_eq!(
        config.wire_profile_state().snapshot().user_agent(),
        "Codex Desktop/1.2026.190 (Mac OS; arm64)"
    );
}

#[test]
fn openai_config_restricts_upstream_base_url_to_https_or_loopback_http() {
    for base_url in [
        "http://internal.example.com/backend-api",
        "http://10.0.0.7/backend-api",
        "https://chatgpt.com/backend-api?debug=1",
        "https://user:pass@chatgpt.com/backend-api",
        "https://chatgpt.com/backend-api#fragment",
        "ftp://chatgpt.com/backend-api",
    ] {
        let mut config = valid_config();
        config.api.base_url = base_url.to_owned();
        assert!(
            config
                .resolve_and_validate(Path::new("/srv/gateway"))
                .is_err(),
            "expected {base_url} to be rejected"
        );
    }

    for base_url in [
        "https://chatgpt.com/backend-api",
        "http://127.0.0.1:8080/backend-api",
        "http://localhost:8080/backend-api",
        "http://[::1]:8080/backend-api",
    ] {
        let mut config = valid_config();
        config.api.base_url = base_url.to_owned();
        assert!(
            config
                .resolve_and_validate(Path::new("/srv/gateway"))
                .is_ok(),
            "expected {base_url} to be accepted"
        );
    }
}

#[test]
fn openai_config_keeps_the_quota_exhaustion_scheduling_switch() {
    let mut config = valid_config();
    config.quota.skip_exhausted = false;

    assert!(!config.quota_skip_exhausted());
}

#[test]
fn openai_config_defaults_to_the_provider_owned_operating_values() {
    let config = OpenAiConfig::default();

    assert_eq!(
        (
            config.api.base_url.as_str(),
            config.ws_pool.enabled,
            config.ws_pool.max_age_ms,
            config.ws_pool.max_per_account,
            config.ws_pool.max_total,
            config.ws_pool.max_connecting,
            config.ws_pool.initial_event_timeout_ms,
            config.quota.refresh_interval_minutes,
            config.quota.skip_exhausted,
            config.auth.refresh_enabled,
            config.auth.oauth_client_id.as_str(),
            config.auth.oauth_token_endpoint.as_str(),
        ),
        (
            "https://chatgpt.com/backend-api",
            true,
            3_300_000,
            8,
            64,
            8,
            20_000,
            15,
            true,
            true,
            "app_EMoamEEZ73f0CkXaXp7hrann",
            "https://auth.openai.com/oauth/token",
        )
    );
    assert_eq!(
        config.wire_profile_state().snapshot().user_agent(),
        "Codex Desktop/26.727.51351 (Mac OS; arm64)"
    );
}

fn valid_config() -> OpenAiConfig {
    let mut config = OpenAiConfig::default();
    config.wire_profile = CodexWireProfileConfig {
        originator: "Codex Desktop".to_owned(),
        codex_version: "0.102.0".to_owned(),
        desktop_version: "1.2026.190".to_owned(),
        desktop_build: "19012345678".to_owned(),
        os_type: "Mac OS".to_owned(),
        os_version: "15.5.0".to_owned(),
        arch: "arm64".to_owned(),
        terminal: "xterm-256color".to_owned(),
        verified_at: Utc
            .with_ymd_and_hms(2026, 7, 19, 0, 0, 0)
            .single()
            .expect("valid test time"),
    };
    config
}
