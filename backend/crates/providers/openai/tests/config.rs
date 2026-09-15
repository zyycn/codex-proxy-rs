use std::path::Path;

use chrono::{TimeZone as _, Utc};
use provider_openai::config::{
    CodexWireProfileConfig, DEFAULT_STREAM_MAX_RETRIES, MAX_STREAM_MAX_RETRIES, OpenAiConfig,
    OpenAiConfigError,
};
use provider_openai::transport::profile::CodexRequestLocation;
use serde_json::json;

#[test]
fn openai_location_should_default_to_passthrough() {
    let profile = OpenAiConfig::default().wire_profile_state().snapshot();
    assert_eq!(profile.location, None);
}

#[test]
fn openai_config_should_preserve_a_custom_location_in_the_runtime_profile() {
    let location: CodexRequestLocation = serde_json::from_value(json!({
        "country": "NZ", "region": "Auckland", "city": "Auckland", "timezone": "Pacific/Auckland"
    }))
    .expect("location configuration");
    let mut config = valid_config();
    config.wire_profile.location = Some(location.clone());
    config
        .resolve_and_validate(Path::new("/srv/gateway"))
        .expect("valid location");
    assert_eq!(
        config.wire_profile_state().snapshot().location,
        Some(location)
    );
}

#[test]
fn openai_config_should_reject_invalid_location_fields() {
    for (field, value) in [
        ("country", "USA"),
        ("country", "us"),
        ("country", ""),
        ("region", " "),
        ("city", ""),
    ] {
        let mut config = valid_config();
        let location = config
            .wire_profile
            .location
            .insert(CodexRequestLocation::default());
        let (target, expected) = match field {
            "country" => (
                &mut location.country,
                "openai.wire_profile.location.country",
            ),
            "region" => (&mut location.region, "openai.wire_profile.location.region"),
            _ => (&mut location.city, "openai.wire_profile.location.city"),
        };
        *target = value.to_owned();
        assert_eq!(
            config.resolve_and_validate(Path::new("/srv/gateway")),
            Err(OpenAiConfigError::InvalidField(expected))
        );
    }
}

#[test]
fn location_config_should_reject_unknown_timezones_and_incomplete_groups() {
    for value in [
        json!({ "country": "US", "region": "Ohio", "city": "Piketon", "timezone": "Not/A_Timezone" }),
        json!({ "timezone": "Pacific/Auckland" }),
        json!({ "country": "US", "region": "Ohio", "city": "Piketon", "timezone": "America/New_York", "extra": true }),
    ] {
        assert!(serde_json::from_value::<CodexRequestLocation>(value).is_err());
    }
}

#[test]
fn openai_config_builds_the_audited_wire_profile() {
    let mut config = valid_config();
    config
        .resolve_and_validate(Path::new("/srv/gateway"))
        .expect("valid OpenAI config");
    let profile = config.wire_profile_state().snapshot();

    assert_eq!(
        profile.user_agent(),
        "Codex Desktop/0.102.0 (Mac OS 15.5.0; arm64) xterm-256color (Codex Desktop; 1.2026.190)"
    );
    assert_eq!(profile.desktop_build, "19012345678");
}

#[test]
fn openai_config_derives_identity_secret_from_runtime_data_dir() {
    let mut config = valid_config();
    config
        .resolve_and_validate(Path::new("/srv/gateway/runtime-data"))
        .expect("valid OpenAI config");

    assert!(format!("{config:?}").contains("/srv/gateway/runtime-data/identity_hmac_secret"));
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
fn openai_config_defaults_to_the_provider_owned_operating_values() {
    let config = OpenAiConfig::default();
    assert_eq!(DEFAULT_STREAM_MAX_RETRIES, 5);

    assert_eq!(
        (
            config.api.base_url.as_str(),
            config.ws_pool.enabled,
            config.ws_pool.max_age_ms,
            config.ws_pool.max_connecting,
            config.ws_pool.stream_idle_timeout_ms,
            config.quota.refresh_interval_minutes,
            config.auth.refresh_enabled,
            config.auth.oauth_client_id.as_str(),
            config.auth.oauth_token_endpoint.as_str(),
            config.stream_max_retries(),
        ),
        (
            "https://chatgpt.com/backend-api",
            true,
            3_300_000,
            8,
            300_000,
            15,
            true,
            "app_EMoamEEZ73f0CkXaXp7hrann",
            "https://auth.openai.com/oauth/token",
            u32::try_from(DEFAULT_STREAM_MAX_RETRIES).expect("default retry budget fits u32"),
        )
    );
    assert_eq!(
        config.wire_profile_state().snapshot().user_agent(),
        "Codex Desktop/0.153.4 (Mac OS 15.7.1; arm64) unknown (Codex Desktop; 26.901.51231)"
    );
}

#[test]
fn openai_stream_retry_budget_uses_the_official_hard_cap() {
    let mut config = OpenAiConfig::default();
    config.stream_max_retries = MAX_STREAM_MAX_RETRIES + 1;

    assert_eq!(
        config.stream_max_retries(),
        u32::try_from(MAX_STREAM_MAX_RETRIES).expect("retry cap fits u32")
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
        residency: None,
        location: Default::default(),
        verified_at: Utc
            .with_ymd_and_hms(2026, 7, 19, 0, 0, 0)
            .single()
            .expect("valid test time"),
    };
    config
}
