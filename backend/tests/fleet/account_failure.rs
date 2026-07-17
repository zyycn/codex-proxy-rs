use codex_proxy_rs::fleet::{
    account::AccountStatus,
    account_failure::{AccountFailureKind, AccountStateEffect, classify_account_failure},
    account_gateway::AccountFailureObservation,
};

#[test]
fn deactivated_workspace_should_be_banned_before_generic_payment_classification() {
    let failure = classify(observation(402, Some("deactivated_workspace"), ""));

    assert_eq!(failure.kind, AccountFailureKind::Banned);
    assert_eq!(
        failure.effect,
        Some(AccountStateEffect::SetStatus(AccountStatus::Banned))
    );
}

#[test]
fn generic_payment_required_should_exhaust_quota() {
    let failure = classify(observation(402, Some("payment_required"), ""));

    assert_eq!(failure.kind, AccountFailureKind::QuotaExhausted);
    assert_eq!(
        failure.effect,
        Some(AccountStateEffect::SetStatus(AccountStatus::QuotaExhausted))
    );
}

#[test]
fn unauthorized_should_expire_account() {
    let failure = classify(observation(401, None, "unauthorized"));

    assert_eq!(failure.kind, AccountFailureKind::Expired);
    assert_eq!(
        failure.effect,
        Some(AccountStateEffect::SetStatus(AccountStatus::Expired))
    );
}

#[test]
fn rate_limit_should_create_temporary_quota_effect() {
    let failure = classify(observation(429, Some("rate_limit_exceeded"), ""));

    assert_eq!(failure.kind, AccountFailureKind::RateLimited);
    assert!(matches!(
        failure.effect,
        Some(AccountStateEffect::MarkQuotaLimitedUntil(_))
    ));
}

#[test]
fn html_forbidden_page_should_not_ban_account() {
    let facts = observation(
        403,
        None,
        "<!doctype html><html>temporary edge failure</html>",
    );

    assert!(classify_account_failure(&facts).is_none());
}

#[test]
fn generic_forbidden_should_not_ban_account() {
    let facts = observation(403, None, "request forbidden");

    assert!(classify_account_failure(&facts).is_none());
}

#[test]
fn generic_permission_error_should_not_ban_account() {
    let mut facts = observation(403, Some("forbidden"), "");
    facts.error_type = Some("permission_error".to_string());

    assert!(classify_account_failure(&facts).is_none());
}

#[test]
fn identity_error_header_should_expire_account() {
    let mut facts = observation(403, None, "request forbidden");
    facts.identity_error_code = Some("token_expired".to_string());

    let failure = classify(facts);

    assert_eq!(failure.kind, AccountFailureKind::Expired);
}

fn classify(
    facts: AccountFailureObservation,
) -> codex_proxy_rs::fleet::account_failure::ClassifiedAccountFailure {
    classify_account_failure(&facts).expect("failure should be classified")
}

fn observation(status_code: u16, code: Option<&str>, message: &str) -> AccountFailureObservation {
    AccountFailureObservation {
        status_code: Some(status_code),
        code: code.map(ToString::to_string),
        message: message.to_string(),
        body: message.to_string(),
        ..AccountFailureObservation::default()
    }
}
