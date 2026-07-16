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

#[test]
fn generic_forbidden_should_not_ban_account() {
    let error = upstream_error(403, r#"{"error":{"message":"request forbidden"}}"#);

    assert!(classify_client_failure(&error).is_none());
}

#[test]
fn generic_permission_error_should_not_ban_account() {
    let error = upstream_error(
        403,
        r#"{"error":{"code":"forbidden","type":"permission_error"}}"#,
    );

    assert!(classify_client_failure(&error).is_none());
}

#[test]
fn identity_error_header_should_expire_account() {
    let mut diagnostics = CodexUpstreamDiagnostics::with_status(403);
    diagnostics.identity_error_code = Some("token_expired".to_string());
    let error = upstream_error_with_diagnostics(
        403,
        r#"{"error":{"message":"request forbidden"}}"#,
        diagnostics,
    );

    let failure = classify_client_failure(&error).expect("identity failure should be classified");

    assert_eq!(failure.kind, AccountFailureKind::Expired);
}

fn classify(
    status: u16,
    body: &str,
) -> codex_proxy_rs::fleet::account_failure::ClassifiedAccountFailure {
    classify_client_failure(&upstream_error(status, body)).expect("failure should be classified")
}

fn upstream_error(status: u16, body: &str) -> CodexClientError {
    upstream_error_with_diagnostics(status, body, CodexUpstreamDiagnostics::with_status(status))
}

fn upstream_error_with_diagnostics(
    status: u16,
    body: &str,
    diagnostics: CodexUpstreamDiagnostics,
) -> CodexClientError {
    CodexClientError::Upstream {
        status: StatusCode::from_u16(status).unwrap(),
        body: body.to_string(),
        retry_after_seconds: None,
        diagnostics: Box::new(diagnostics),
        set_cookie_headers: Vec::new(),
        rate_limit_headers: Vec::new(),
        transport: CodexBackendTransport::HttpSse,
    }
}
