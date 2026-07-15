use codex_proxy_rs::{
    fleet::{
        account::AccountStatus,
        account_failure::{AccountFailureKind, AccountStateEffect, classify_client_failure},
    },
    upstream::openai::transport::{
        CodexBackendTransport, CodexClientError, CodexUpstreamDiagnostics,
    },
};
use reqwest::StatusCode;

#[test]
fn deactivated_workspace_should_be_banned_before_generic_payment_classification() {
    let failure = classify(402, r#"{"detail":{"code":"deactivated_workspace"}}"#);

    assert_eq!(failure.kind, AccountFailureKind::Banned);
    assert_eq!(
        failure.effect,
        Some(AccountStateEffect::SetStatus(AccountStatus::Banned))
    );
}

#[test]
fn generic_payment_required_should_exhaust_quota() {
    let failure = classify(402, r#"{"detail":{"code":"payment_required"}}"#);

    assert_eq!(failure.kind, AccountFailureKind::QuotaExhausted);
    assert_eq!(
        failure.effect,
        Some(AccountStateEffect::SetStatus(AccountStatus::QuotaExhausted))
    );
}

#[test]
fn unauthorized_should_expire_account() {
    let failure = classify(401, r#"{"error":{"message":"unauthorized"}}"#);

    assert_eq!(failure.kind, AccountFailureKind::Expired);
    assert_eq!(
        failure.effect,
        Some(AccountStateEffect::SetStatus(AccountStatus::Expired))
    );
}

#[test]
fn rate_limit_should_create_temporary_quota_effect() {
    let failure = classify(429, r#"{"error":{"code":"rate_limit_exceeded"}}"#);

    assert_eq!(failure.kind, AccountFailureKind::RateLimited);
    assert!(matches!(
        failure.effect,
        Some(AccountStateEffect::MarkQuotaLimitedUntil(_))
    ));
}

#[test]
fn html_forbidden_page_should_not_ban_account() {
    let error = upstream_error(403, "<!doctype html><html>temporary edge failure</html>");

    assert!(classify_client_failure(&error).is_none());
}

fn classify(
    status: u16,
    body: &str,
) -> codex_proxy_rs::fleet::account_failure::ClassifiedAccountFailure {
    classify_client_failure(&upstream_error(status, body)).expect("failure should be classified")
}

fn upstream_error(status: u16, body: &str) -> CodexClientError {
    CodexClientError::Upstream {
        status: StatusCode::from_u16(status).unwrap(),
        body: body.to_string(),
        retry_after_seconds: None,
        diagnostics: CodexUpstreamDiagnostics::with_status(status),
        set_cookie_headers: Vec::new(),
        transport: CodexBackendTransport::HttpSse,
    }
}
