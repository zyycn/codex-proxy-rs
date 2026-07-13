use codex_proxy_rs::{
    bootstrap::{
        config::{FingerprintConfig, FingerprintHeaderConfig},
        services::fingerprint_from_config,
    },
    upstream::openai::fingerprint::{Fingerprint, RuntimeFingerprint},
};

pub(crate) fn test_fingerprint_config() -> FingerprintConfig {
    let header = |name: &str, value: &str| FingerprintHeaderConfig {
        name: name.to_string(),
        value: value.to_string(),
    };

    FingerprintConfig {
        originator: "Codex Desktop".to_string(),
        app_version: "26.707.51957".to_string(),
        build_number: "5175".to_string(),
        platform: "darwin".to_string(),
        arch: "arm64".to_string(),
        chromium_version: "146".to_string(),
        user_agent_template: "Codex Desktop/{version} ({platform}; {arch})".to_string(),
        default_headers: vec![
            header("Accept-Encoding", "gzip, deflate, br, zstd"),
            header("Accept-Language", "en-US,en;q=0.9"),
            header("sec-ch-ua-mobile", "?0"),
            header("sec-ch-ua-platform", "\"macOS\""),
            header("sec-fetch-site", "same-origin"),
            header("sec-fetch-mode", "cors"),
            header("sec-fetch-dest", "empty"),
        ],
        header_order: [
            "authorization",
            "chatgpt-account-id",
            "originator",
            "x-openai-internal-codex-residency",
            "x-client-request-id",
            "x-codex-installation-id",
            "x-codex-turn-state",
            "openai-beta",
            "user-agent",
            "sec-ch-ua",
            "sec-ch-ua-mobile",
            "sec-ch-ua-platform",
            "accept-encoding",
            "accept-language",
            "sec-fetch-site",
            "sec-fetch-mode",
            "sec-fetch-dest",
            "content-type",
            "accept",
            "cookie",
        ]
        .map(str::to_string)
        .to_vec(),
    }
}

pub(crate) fn test_fingerprint() -> Fingerprint {
    fingerprint_from_config(&test_fingerprint_config())
}

pub(crate) fn test_fingerprint_with_updated_at(updated_at: Option<&str>) -> Fingerprint {
    let mut fingerprint = test_fingerprint();
    fingerprint.updated_at = updated_at.map(ToString::to_string);
    fingerprint
}

pub(crate) fn runtime_test_fingerprint() -> RuntimeFingerprint {
    RuntimeFingerprint::new(test_fingerprint())
}

pub(crate) fn runtime_fingerprint(fingerprint: Fingerprint) -> RuntimeFingerprint {
    RuntimeFingerprint::new(fingerprint)
}
