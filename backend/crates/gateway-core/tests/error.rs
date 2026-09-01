use bytes::Bytes;
use gateway_core::error::{
    ClientVisibleUpstreamError, ClientVisibleUpstreamResponse, GatewayError, OpaqueUpstreamValue,
    ProviderDiagnostic, ProviderError, ProviderErrorKind, RawUpstreamError,
};
use gateway_core::event::{ProtocolWireEvent, ProviderEvent, ProviderResponseHeader};
use gateway_core::upstream::UpstreamSendState;
use serde_json::json;

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
    let value = OpaqueUpstreamValue::new(diagnostic);
    let error = ProviderError::new(ProviderErrorKind::Unavailable, UpstreamSendState::Sent)
        .with_upstream_request_id(value);

    assert!(!format!("{error:?}").contains(diagnostic));
}

#[test]
fn classified_provider_diagnostic_survives_clone_without_entering_debug() {
    let message = "OpenAI WebSocket closed before terminal response (close code 1000)";
    let error = ProviderError::new(ProviderErrorKind::Transport, UpstreamSendState::Ambiguous)
        .with_diagnostic(ProviderDiagnostic::new(message));
    let cloned = error.clone();
    let gateway = GatewayError::from_provider(&cloned);

    assert_eq!(
        gateway.diagnostic().map(ProviderDiagnostic::as_str),
        Some(message)
    );
    assert!(!format!("{error:?}").contains(message));
    assert!(!format!("{gateway:?}").contains(message));
}

#[test]
fn raw_upstream_error_survives_clone_without_entering_debug() {
    let raw = r#"{"error":{"message":"verbatim upstream marker"}}"#;
    let error = ProviderError::new(ProviderErrorKind::Unavailable, UpstreamSendState::Sent)
        .with_raw_upstream_error(RawUpstreamError::new(raw));
    let cloned = error.clone();

    assert_eq!(
        cloned.raw_upstream_error().map(RawUpstreamError::as_str),
        Some(raw)
    );
    assert!(!format!("{error:?}").contains("verbatim upstream marker"));
}

#[test]
fn opaque_upstream_value_should_preserve_arbitrary_text_without_logging_it() {
    let original = format!("\0{}\n", "x".repeat(9_000));
    let value = OpaqueUpstreamValue::new(original.clone());

    assert_eq!(value.as_str(), original);
    assert!(!format!("{value:?}").contains(&original));
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
fn provider_error_atomic_client_events_should_be_take_only_and_debug_redacted() {
    let marker = "atomic-wire-must-not-enter-debug";
    let event = ProviderEvent::wire(
        ProtocolWireEvent::json(
            "openai",
            Some("response.failed".to_owned()),
            json!({"type": "response.failed", "message": marker}),
        )
        .expect("atomic wire"),
    );
    let mut error = ProviderError::new(ProviderErrorKind::RateLimited, UpstreamSendState::Sent)
        .with_atomic_client_events(vec![event]);

    assert!(error.has_atomic_client_events());
    assert!(!format!("{error:?}").contains(marker));
    assert_eq!(error.take_atomic_client_events().len(), 1);
    assert!(!error.has_atomic_client_events());
}

#[test]
fn provider_error_clone_should_drop_request_local_upstream_response() {
    let marker = Bytes::from_static(b"raw-client-response-only");
    let error = ProviderError::new(ProviderErrorKind::RateLimited, UpstreamSendState::Sent)
        .with_client_visible_upstream_response(
            ClientVisibleUpstreamResponse::new(
                429,
                Some(b"application/problem+json".to_vec()),
                marker.clone(),
            )
            .with_headers(vec![ProviderResponseHeader::new(
                "x-future-error",
                Bytes::from_static(b"opaque-header-value"),
            )]),
        );

    let cloned = error.clone();

    assert_eq!(
        error
            .client_visible_upstream_response()
            .map(ClientVisibleUpstreamResponse::body),
        Some(&marker)
    );
    assert!(cloned.client_visible_upstream_response().is_none());
    assert!(!format!("{error:?}").contains("raw-client-response-only"));
    assert!(!format!("{error:?}").contains("opaque-header-value"));
}

#[test]
fn provider_error_clone_should_drop_atomic_client_events() {
    let event = ProviderEvent::wire(
        ProtocolWireEvent::json(
            "openai",
            Some("response.failed".to_owned()),
            json!({"type": "response.failed"}),
        )
        .expect("atomic wire"),
    );
    let error = ProviderError::new(ProviderErrorKind::RateLimited, UpstreamSendState::Sent)
        .with_atomic_client_events(vec![event]);
    let cloned = error.clone();

    assert!(error.has_atomic_client_events());
    assert!(!cloned.has_atomic_client_events());
}

#[test]
fn gateway_error_should_keep_client_visible_upstream_fields_out_of_safe_diagnostics() {
    let message = "Your Codex quota is exhausted";
    let error = ProviderError::new(ProviderErrorKind::QuotaExhausted, UpstreamSendState::Sent)
        .with_client_visible_upstream_error(ClientVisibleUpstreamError::new(
            message,
            Some("quota_exhausted".to_owned()),
            Some("rate_limit_error".to_owned()),
        ));
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

#[test]
fn client_visible_upstream_error_should_preserve_opaque_structured_fields() {
    let message = format!("\0{}\n", "m".repeat(9_000));
    let code = format!("\0{}", "c".repeat(300));
    let error_type = String::new();
    let detail = ClientVisibleUpstreamError::new(
        message.clone(),
        Some(code.clone()),
        Some(error_type.clone()),
    );

    assert_eq!(detail.message(), message);
    assert_eq!(detail.code(), Some(code.as_str()));
    assert_eq!(detail.error_type(), Some(error_type.as_str()));
    assert!(!format!("{detail:?}").contains(&message));
}
