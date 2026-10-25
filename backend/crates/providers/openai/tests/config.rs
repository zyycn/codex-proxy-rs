//! 验证 OpenAI 配置解析、路径派生与上游地址约束

use std::path::Path;

use provider_openai::config::{MAX_STREAM_MAX_RETRIES, OpenAiConfig};

#[test]
fn openai_config_preserves_optional_routing_header_values() {
    for (document, expected) in [
        (r#"{}"#, (None, None)),
        (
            r#"{"residency":null,"account_routing_override":null}"#,
            (None, None),
        ),
        (
            r#"{"residency":"us","account_routing_override":"us"}"#,
            (Some("us"), Some("us")),
        ),
        (
            r#"{"residency":"custom-region","account_routing_override":"custom-route"}"#,
            (Some("custom-region"), Some("custom-route")),
        ),
        (
            r#"{"residency":"","account_routing_override":"NO_CONSTRAINT"}"#,
            (Some(""), Some("NO_CONSTRAINT")),
        ),
    ] {
        let config: OpenAiConfig = serde_json::from_str(document).unwrap();
        assert_eq!(
            (
                config.residency.as_deref(),
                config.account_routing_override.as_deref()
            ),
            expected,
            "{document}",
        );
    }
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
fn openai_stream_retry_budget_uses_the_official_hard_cap() {
    let mut config = OpenAiConfig::default();
    config.stream_max_retries = MAX_STREAM_MAX_RETRIES + 1;

    assert_eq!(
        config.stream_max_retries(),
        u32::try_from(MAX_STREAM_MAX_RETRIES).expect("retry cap fits u32")
    );
}

fn valid_config() -> OpenAiConfig {
    OpenAiConfig::default()
}
