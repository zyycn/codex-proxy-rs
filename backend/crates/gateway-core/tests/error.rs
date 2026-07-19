use gateway_core::engine::UpstreamSendState;
use gateway_core::error::{ProviderError, ProviderErrorKind, SafeUpstreamValue};

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
