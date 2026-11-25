//! 对应生产职责的行为测试

use super::*;

#[test]
fn oauth_pending_binding_debug_redacts_raw_value() {
    let binding = OAuthPendingBinding::try_new("must-not-appear").expect("valid binding");

    assert_eq!(format!("{binding:?}"), "OAuthPendingBinding([REDACTED])");
}

#[test]
fn oauth_pending_ttl_rejects_zero_and_more_than_thirty_minutes() {
    let provider = ProviderKind::new("fixture").expect("valid provider");
    let flow = OAuthPendingBinding::try_new("flow").expect("valid flow");
    let owner = OAuthPendingBinding::try_new("owner").expect("valid owner");
    let payload = OpaqueProviderData::new(serde_json::Map::new());

    for ttl in [Duration::ZERO, Duration::from_secs(30 * 60 + 1)] {
        let error = NewOAuthPendingFlow::try_new(
            provider.clone(),
            flow.clone(),
            owner.clone(),
            ttl,
            payload.clone(),
        )
        .expect_err("invalid TTL must fail");
        assert_eq!(error.kind(), ProviderStoreErrorKind::InvalidData);
    }
}
