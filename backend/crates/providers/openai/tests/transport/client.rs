use provider_openai::transport::CodexUpstreamSendPhase;
use provider_openai::transport::{CodexBackendTransport, CodexClientError, CodexRequestContext};

#[test]
fn upstream_error_formatting_should_redact_body() {
    let secret = "bearer-secret-in-upstream-body";
    let error = CodexClientError::Upstream {
        status: reqwest::StatusCode::UNAUTHORIZED,
        body: secret.to_owned(),
        retry_after_seconds: None,
        diagnostics: Box::default(),
        set_cookie_headers: vec!["session=secret".to_owned()],
        rate_limit_headers: Vec::new(),
        transport: CodexBackendTransport::HttpSse,
        transport_metrics: Box::default(),
        send_phase: CodexUpstreamSendPhase::AfterPayload,
    };

    assert!(!format!("{error}").contains(secret));
    assert!(!format!("{error:?}").contains(secret));
    assert!(!format!("{error:?}").contains("session=secret"));
}

#[test]
fn request_context_debug_should_redact_all_identity_material() {
    let context = CodexRequestContext {
        access_token: "access-secret-marker",
        account_id: Some("account-secret-marker"),
        request_id: "req_safe",
        turn_state: Some("turn-secret-marker"),
        turn_metadata: Some("metadata-secret-marker"),
        beta_features: None,
        include_timing_metrics: None,
        version: None,
        codex_window_id: None,
        parent_thread_id: None,
        cookie_header: Some("cookie-secret-marker"),
        installation_id: Some("installation-pseudonym-marker"),
        session_id: None,
        thread_id: None,
        client_request_id: None,
        turn_id: None,
    };
    let debug = format!("{context:?}");

    for secret in [
        "access-secret-marker",
        "account-secret-marker",
        "turn-secret-marker",
        "metadata-secret-marker",
        "cookie-secret-marker",
        "installation-pseudonym-marker",
    ] {
        assert!(!debug.contains(secret));
    }
    assert!(debug.contains("req_safe"));
}
