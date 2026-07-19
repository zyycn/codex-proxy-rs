use gateway_core::policy::{
    AdmissionUnits, ClientApiKeyId, ClientPolicy, PlaintextClientApiKey, RateLimits,
};
use gateway_core::routing::ProviderKind;

fn plaintext(value: &str) -> PlaintextClientApiKey {
    PlaintextClientApiKey::new(value).expect("valid plaintext client key")
}

#[test]
fn disabled_client_key_should_be_denied() {
    let policy = ClientPolicy::new(
        ClientApiKeyId::new("key_disabled").expect("valid key ID"),
        plaintext("sk_disabled_secret"),
        ProviderKind::new("openai").expect("valid provider"),
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
        ProviderKind::new("openai").expect("valid provider"),
        true,
        RateLimits::unlimited(),
    );

    assert!(policy.authorize().is_ok());
}

#[test]
fn admission_should_reject_zero_request_units() {
    assert!(AdmissionUnits::new(0, 10).is_err());
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
