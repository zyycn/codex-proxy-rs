use std::sync::Arc;

use gateway_core::policy::{
    ClientApiKeyId, ClientPolicy, ClientVersionRejection, CodexClientKind, CodexClientMinVersions,
    CodexClientVersion, PlaintextClientApiKey, RateLimits,
};
use gateway_core::routing::{ClientRoutingScope, FrozenAccountScope, RuntimeAccountDirectory};

fn plaintext(value: &str) -> PlaintextClientApiKey {
    PlaintextClientApiKey::new(value).expect("valid plaintext client key")
}

fn account_scope() -> Arc<FrozenAccountScope> {
    Arc::new(FrozenAccountScope::new(
        Arc::new(RuntimeAccountDirectory::default()),
        ClientRoutingScope::all_accounts(),
    ))
}

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
fn client_version_should_require_strict_semver() {
    assert!(CodexClientVersion::parse("0.152.0").is_ok());
    assert!(CodexClientVersion::parse("v0.152.0").is_err());
    assert!(CodexClientVersion::parse("0.152").is_err());
    assert!(CodexClientVersion::parse(" 0.152.0").is_err());
    assert!(CodexClientVersion::parse("18446744073709551616.0.0").is_err());
}

#[test]
fn min_client_versions_should_reject_missing_and_old_recognized_clients() {
    let min = CodexClientVersion::parse("0.40.0").expect("min version");
    let policy = CodexClientMinVersions::new(None, Some(min.clone()));
    let old = CodexClientVersion::parse("0.39.0").expect("old version");
    let current = CodexClientVersion::parse("0.40.0").expect("current version");

    assert_eq!(
        policy.enforce(CodexClientKind::Cli, None),
        Err(ClientVersionRejection::Unavailable {
            kind: CodexClientKind::Cli,
            min: min.clone(),
        })
    );
    assert_eq!(
        policy.enforce(CodexClientKind::Cli, Some(&old)),
        Err(ClientVersionRejection::TooOld {
            kind: CodexClientKind::Cli,
            current: old,
            min,
        })
    );
    assert_eq!(policy.enforce(CodexClientKind::Cli, Some(&current)), Ok(()));
    assert_eq!(policy.enforce(CodexClientKind::Desktop, None), Ok(()));
}

#[test]
fn min_client_versions_should_follow_semver_precedence() {
    let min = CodexClientVersion::parse("1.0.0").expect("min version");
    let policy = CodexClientMinVersions::new(None, Some(min));
    let prerelease = CodexClientVersion::parse("1.0.0-rc.1").expect("prerelease");
    let release = CodexClientVersion::parse("1.0.0").expect("release");
    let newer = CodexClientVersion::parse("1.0.1").expect("newer release");

    assert!(
        policy
            .enforce(CodexClientKind::Cli, Some(&prerelease))
            .is_err()
    );
    assert_eq!(policy.enforce(CodexClientKind::Cli, Some(&release)), Ok(()));
    assert_eq!(policy.enforce(CodexClientKind::Cli, Some(&newer)), Ok(()));
}
