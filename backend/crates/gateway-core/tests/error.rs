use gateway_core::engine::UpstreamSendState;
use gateway_core::error::{
    ClientVisibleUpstreamError, GatewayError, ProviderError, ProviderErrorKind, SafeUpstreamValue,
};

#[test]
fn provider_error_debug_should_not_expose_sensitive_context() {
    let secret = "sk-do-not-log-this";
    let error = ProviderError::new(ProviderErrorKind::Unauthorized, UpstreamSendState::Sent)
        .redact_sensitive_context(secret);

    assert!(!format!("{error:?}").contains(secret));
}

#[test]
fn provider_error_debug_should_not_print_classified_upstream_values() {
    let diagnostic = "request-visible-only-through-explicit-accessor";
    let value = SafeUpstreamValue::new(diagnostic).expect("test diagnostic is valid");
    let error = ProviderError::new(ProviderErrorKind::Unavailable, UpstreamSendState::Sent)
        .with_upstream_request_id(value);

    assert!(!format!("{error:?}").contains(diagnostic));
}

#[test]
fn provider_error_replay_proof_should_default_to_false() {
    let error = ProviderError::new(ProviderErrorKind::RateLimited, UpstreamSendState::Sent);

    assert!(!error.replay_is_safe());
}

#[test]
fn provider_error_replay_proof_should_be_explicit() {
    let error = ProviderError::new(ProviderErrorKind::RateLimited, UpstreamSendState::Sent)
        .with_replay_safe();

    assert!(error.replay_is_safe());
}

#[test]
fn gateway_error_should_keep_client_visible_upstream_fields_out_of_safe_diagnostics() {
    let message = "Your Codex quota is exhausted";
    let error = ProviderError::new(ProviderErrorKind::QuotaExhausted, UpstreamSendState::Sent)
        .with_client_visible_upstream_error(
            ClientVisibleUpstreamError::new(
                message,
                Some("quota_exhausted".to_owned()),
                Some("rate_limit_error".to_owned()),
            )
            .expect("safe structured upstream error"),
        );
    let gateway = GatewayError::from_provider(&error);

    assert_eq!(
        gateway.safe_message(),
        "upstream capacity is temporarily unavailable"
    );
    assert_eq!(gateway.client_message(), message);
    assert_eq!(gateway.client_error_code(), Some("quota_exhausted"));
    assert_eq!(gateway.client_error_type(), Some("rate_limit_error"));
    assert!(!format!("{gateway:?}").contains(message));
}
