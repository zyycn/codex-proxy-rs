//! 跨模块行为合同测试

use super::*;

#[test]
fn disabled_client_key_should_be_denied() {
    let policy = ClientPolicy::new(
        ClientApiKeyId::new("key_disabled").expect("valid key ID"),
        plaintext("sk_disabled_secret"),
        account_scope(),
        false,
        RateLimits::unlimited(),
    );

    assert!(policy.authorize().is_err());
}

#[test]
fn enabled_client_key_should_be_authorized() {
    let policy = ClientPolicy::new(
        ClientApiKeyId::new("key_enabled").expect("valid key ID"),
        plaintext("sk_enabled_secret"),
        account_scope(),
        true,
        RateLimits::unlimited(),
    );

    assert!(policy.authorize().is_ok());
}

#[test]
fn zero_rate_limits_should_mean_unlimited() {
    assert_eq!(RateLimits::unlimited().requests_per_minute, 0);
}

#[test]
fn plaintext_client_key_debug_should_be_redacted() {
    let key = plaintext("sk_must_not_appear");

    let debug = format!("{key:?}");
    assert!(!debug.contains("must_not_appear"));
    assert_eq!(key.expose_for_auth(), "sk_must_not_appear");
}

#[test]
fn migrated_client_keys_keep_their_exact_value_without_prefix_or_length_limits() {
    for value in [
        "q".to_owned(),
        "legacy-!@#$%^&*()[]{}:;,.?/+=~|\\".to_owned(),
        "x".repeat(8192),
    ] {
        assert_eq!(plaintext(&value).expose_for_auth(), value);
    }
    for value in [
        "",
        " ",
        " key",
        "key ",
        "key\n",
        "key\tvalue",
        "key\u{7f}",
        "密钥",
    ] {
        assert!(PlaintextClientApiKey::new(value).is_err());
    }
}
