//! 跨模块行为合同测试

use super::*;

#[test]
fn model_rejection_requires_the_official_code_and_status() {
    for status in [400, 403, 404] {
        assert_eq!(
            classify(
                status,
                r#"{"error":{"code":"model_not_found","message":"Access to this deployment ended."}}"#
            ),
            CodexFailureCategory::ModelUnsupported
        );
    }
    for (status, body, expected) in [
        (
            400,
            r#"{"error":{"code":"unsupported_value","type":"invalid_request_error","param":"reasoning.effort","message":"Unsupported value: 'none' is not supported with the 'gpt-6-astra' model. Supported values are: 'low', 'medium', 'high', 'xhigh', and 'max'."}}"#,
            CodexFailureCategory::InvalidRequest,
        ),
        (
            400,
            r#"{"error":{"message":"The model is not supported."}}"#,
            CodexFailureCategory::InvalidRequest,
        ),
        (
            400,
            r#"{"error":{"code":"unknown_error","message":"model_not_supported"}}"#,
            CodexFailureCategory::InvalidRequest,
        ),
        (
            400,
            r#"{"error":{"code":"unknown_error"},"detail":"model_not_available"}"#,
            CodexFailureCategory::InvalidRequest,
        ),
        (
            500,
            r#"{"error":{"code":"model_not_found","message":"The model is not available."}}"#,
            CodexFailureCategory::Unavailable,
        ),
    ] {
        let failure = CodexUpstreamFailure::from_response(
            reqwest::StatusCode::from_u16(status).unwrap(),
            body,
            None,
            &CodexUpstreamDiagnostics::default(),
            None,
            &[],
            &[],
            CodexUpstreamSendPhase::AfterPayload,
        );
        assert_eq!(failure.category(), expected, "{body}");
        assert!(!failure.replay_is_safe(), "{body}");
    }
}

#[test]
fn flex_capacity_rejection_should_be_terminal_even_before_payload_send() {
    for phase in [
        CodexUpstreamSendPhase::BeforePayload,
        CodexUpstreamSendPhase::AfterPayload,
    ] {
        let failure = CodexUpstreamFailure::from_response(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"type":"resource_unavailable","code":"flex_unavailable","message":"Flex capacity unavailable."}}"#,
            Some(300),
            &CodexUpstreamDiagnostics::default(),
            None,
            &[],
            &[],
            phase,
        );
        assert_eq!(failure.category(), CodexFailureCategory::FlexUnavailable);
        assert!(!failure.replay_is_safe());
    }
}

#[test]
fn turn_rate_limit_does_not_grant_proxy_replay_or_account_cooldown() {
    for phase in [
        CodexUpstreamSendPhase::BeforePayload,
        CodexUpstreamSendPhase::AfterPayload,
        CodexUpstreamSendPhase::Ambiguous,
    ] {
        let failure = CodexUpstreamFailure::from_response(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"code":"turn_rate_limit_exceeded","type":"rate_limit_error","message":"Retry this turn later."}}"#,
            Some(53),
            &CodexUpstreamDiagnostics::default(),
            None,
            &[],
            &[],
            phase,
        );
        assert_eq!(failure.category(), CodexFailureCategory::TurnRateLimited);
        assert!(!failure.replay_is_safe());
    }
    assert_eq!(
        classify(429, r#"{"error":{"message":"turn_rate_limit_exceeded"}}"#),
        CodexFailureCategory::RateLimited
    );
}

#[test]
fn server_error_replay_requires_explicit_503_and_unambiguous_send() {
    for (status, body, retryable) in [
        (
            503,
            r#"{"error":{"code":"server_error","type":"server_error"}}"#,
            true,
        ),
        (
            500,
            r#"{"error":{"code":"server_error","type":"server_error"}}"#,
            false,
        ),
        (
            504,
            r#"{"error":{"code":"server_error","type":"server_error"}}"#,
            false,
        ),
        (503, r#"{"error":{"type":"server_error"}}"#, false),
        (
            503,
            r#"{"error":{"code":"Vendor.Future.v2","type":"server_error"}}"#,
            false,
        ),
        (
            503,
            r#"{"error":{"code":"flex_unavailable","type":"server_error"}}"#,
            false,
        ),
        (503, "server_error", false),
    ] {
        for phase in [
            CodexUpstreamSendPhase::BeforePayload,
            CodexUpstreamSendPhase::AfterPayload,
            CodexUpstreamSendPhase::Ambiguous,
        ] {
            let failure = CodexUpstreamFailure::from_response(
                reqwest::StatusCode::from_u16(status).unwrap(),
                body,
                None,
                &CodexUpstreamDiagnostics::default(),
                None,
                &[],
                &[],
                phase,
            );
            assert_eq!(
                failure.replay_is_safe(),
                retryable && phase != CodexUpstreamSendPhase::Ambiguous,
                "status={status}, body={body}, phase={phase:?}"
            );
        }
    }
}

#[test]
fn capacity_rejections_are_request_scoped_across_http_statuses() {
    for status in [400, 429, 503] {
        for body in [
            r#"{"error":{"code":"server_is_overloaded","message":"busy"}}"#,
            r#"{"error":{"code":"slow_down","type":"server_error"}}"#,
            r#"{"error":{"type":"server_is_overloaded"}}"#,
            r#"{"error":{"message":"slow_down"}}"#,
            r#"{"error":{"type":"invalid_request_error","message":"Selected model is at capacity. Please try a different model."}}"#,
        ] {
            assert_eq!(
                classify(status, body),
                CodexFailureCategory::CapacityUnavailable
            );
            for (phase, safe) in [
                (CodexUpstreamSendPhase::BeforePayload, true),
                (CodexUpstreamSendPhase::AfterPayload, true),
                (CodexUpstreamSendPhase::Ambiguous, false),
            ] {
                let failure = CodexUpstreamFailure::from_response(
                    reqwest::StatusCode::from_u16(status).expect("status"),
                    body,
                    Some(129_600),
                    &CodexUpstreamDiagnostics::default(),
                    None,
                    &[],
                    &[],
                    phase,
                );
                assert_eq!(failure.replay_is_safe(), safe);
            }
        }
    }
}

#[test]
fn capacity_words_do_not_override_quota_policy_or_arbitrary_body_fields() {
    for (status, body, expected) in [
        (
            429,
            r#"{"error":{"type":"usage_limit_reached","message":"Selected model is at capacity"}}"#,
            CodexFailureCategory::UsageLimitExhausted,
        ),
        (
            400,
            r#"{"error":{"code":"cyber_policy","message":"Selected model is at capacity"}}"#,
            CodexFailureCategory::InvalidRequest,
        ),
        (
            400,
            r#"{"error":{"message":"bad input"},"input":"Selected model is at capacity"}"#,
            CodexFailureCategory::InvalidRequest,
        ),
        (
            429,
            r#"{"error":{"code":"rate_limit_exceeded","message":"retry later"}}"#,
            CodexFailureCategory::RateLimited,
        ),
    ] {
        assert_eq!(classify(status, body), expected);
    }
}

#[test]
fn diagnostics_should_extract_request_id_and_trace_headers() {
    let mut headers = HeaderMap::new();
    headers.insert("x-request-id", HeaderValue::from_static("req_1"));
    headers.insert("cf-ray", HeaderValue::from_static("ray_1"));

    let diagnostics = CodexUpstreamDiagnostics::from_headers(Some(429), &headers);

    assert_eq!(
        (
            diagnostics.status_code,
            diagnostics.request_id.as_deref(),
            trace_header(&diagnostics, "cf-ray"),
        ),
        (Some(429), Some("req_1"), Some("ray_1"))
    );
}

#[test]
fn diagnostics_should_not_treat_cf_ray_as_a_request_id() {
    let mut headers = HeaderMap::new();
    headers.insert("cf-ray", HeaderValue::from_static("ray_1"));

    let diagnostics = CodexUpstreamDiagnostics::from_headers(Some(403), &headers);

    assert_eq!(
        (
            diagnostics.request_id.as_deref(),
            trace_header(&diagnostics, "cf-ray"),
        ),
        (None, Some("ray_1"))
    );
}

#[test]
fn diagnostics_should_accept_the_oai_request_id_header() {
    let mut headers = HeaderMap::new();
    headers.insert("x-oai-request-id", HeaderValue::from_static("oai_req_1"));

    let diagnostics = CodexUpstreamDiagnostics::from_headers(Some(500), &headers);

    assert_eq!(diagnostics.request_id.as_deref(), Some("oai_req_1"));
}

#[test]
fn diagnostics_should_extract_only_the_identity_error_code() {
    let encoded = STANDARD.encode(r#"{"error":{"code":"token_expired","message":"secret"}}"#);
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-openai-authorization-error",
        HeaderValue::from_static("authorization failed"),
    );
    headers.insert(
        "x-error-json",
        HeaderValue::from_str(&encoded).expect("encoded header"),
    );

    let diagnostics = CodexUpstreamDiagnostics::from_headers(Some(403), &headers);

    assert_eq!(
        (
            diagnostics.identity_authorization_error.as_deref(),
            diagnostics.identity_error_code.as_deref(),
        ),
        (Some("authorization failed"), Some("token_expired"))
    );
    assert!(!format!("{diagnostics:?}").contains("secret"));
}

#[test]
fn diagnostics_should_ignore_malformed_identity_error_json() {
    let mut headers = HeaderMap::new();
    headers.insert("x-error-json", HeaderValue::from_static("not-base64"));

    let diagnostics = CodexUpstreamDiagnostics::from_headers(Some(403), &headers);

    assert_eq!(diagnostics.identity_error_code, None);
}

#[test]
fn unauthorized_substring_in_a_server_error_body_is_not_credential_expiry() {
    assert_eq!(
        classify(
            500,
            r#"{"error":{"message":"upstream proxy replied: unauthorized"}}"#
        ),
        CodexFailureCategory::Unavailable
    );
}

#[test]
fn http_401_classifies_as_credential_expiry_without_body_evidence() {
    assert_eq!(classify(401, ""), CodexFailureCategory::CredentialExpired);
}

#[test]
fn auth_text_with_http_403_still_classifies_as_credential_expiry() {
    assert_eq!(
        classify(
            403,
            r#"{"error":{"message":"unauthorized: token expired"}}"#
        ),
        CodexFailureCategory::CredentialExpired
    );
}

#[test]
fn structured_expiry_code_classifies_without_an_auth_status() {
    assert_eq!(
        classify(
            500,
            r#"{"error":{"code":"token_expired","message":"internal"}}"#
        ),
        CodexFailureCategory::CredentialExpired
    );
}

#[test]
fn http_429_without_usage_limit_type_remains_a_temporary_rate_limit() {
    assert_eq!(
        classify(
            429,
            r#"{"error":{"code":"insufficient_quota","message":"rate limit reached"}}"#
        ),
        CodexFailureCategory::RateLimited
    );
}

#[test]
fn structured_usage_limit_type_is_resettable_quota_exhaustion() {
    assert_eq!(
        classify(
            429,
            r#"{"error":{"code":null,"type":"usage_limit_reached","message":"The usage limit has been reached"}}"#
        ),
        CodexFailureCategory::UsageLimitExhausted
    );
}

#[test]
fn http_429_usage_limit_message_without_a_type_remains_a_temporary_rate_limit() {
    assert_eq!(
        classify(
            429,
            r#"{"error":{"message":"The usage limit has been reached"}}"#
        ),
        CodexFailureCategory::RateLimited
    );
}

#[test]
fn http_429_usage_limit_code_without_a_type_remains_a_temporary_rate_limit() {
    assert_eq!(
        classify(
            429,
            r#"{"error":{"code":"usage_limit_reached","message":"The usage limit has been reached"}}"#
        ),
        CodexFailureCategory::RateLimited
    );
}

#[test]
fn bare_http_429_remains_a_temporary_rate_limit() {
    assert_eq!(classify(429, ""), CodexFailureCategory::RateLimited);
}

#[test]
fn openai_account_failure_matrix_is_preserved() {
    let cases = [
        (
            400,
            r#"{"error":{"message":"Organization has been disabled"}}"#,
            CodexFailureCategory::Banned,
        ),
        (
            400,
            r#"{"error":{"message":"Identity verification is required"}}"#,
            CodexFailureCategory::IdentityVerificationRequired,
        ),
        (
            400,
            r#"{"error":{"message":"invalid request payload"}}"#,
            CodexFailureCategory::InvalidRequest,
        ),
        (
            401,
            r#"{"error":{"code":"token_revoked","message":"token has been revoked"}}"#,
            CodexFailureCategory::CredentialExpired,
        ),
        (
            401,
            r#"{"detail":"Unauthorized"}"#,
            CodexFailureCategory::CredentialExpired,
        ),
        (
            402,
            r#"{"detail":{"code":"deactivated_workspace","message":"workspace disabled"}}"#,
            CodexFailureCategory::Banned,
        ),
        (
            402,
            r#"{"error":{"message":"payment required"}}"#,
            CodexFailureCategory::QuotaExhausted,
        ),
        (
            403,
            r#"{"error":{"message":"access forbidden"}}"#,
            CodexFailureCategory::PermissionDenied,
        ),
        (404, "", CodexFailureCategory::CloudflarePathBlocked),
        (404, " \r\n\t", CodexFailureCategory::CloudflarePathBlocked),
        (
            404,
            r#"{"error":{"type":"invalid_request_error","code":null,"message":"Item with id 'rs_missing' not found.","param":"input"}}"#,
            CodexFailureCategory::InvalidRequest,
        ),
        (
            429,
            r#"{"error":{"type":"usage_limit_reached","message":"quota window reached"}}"#,
            CodexFailureCategory::UsageLimitExhausted,
        ),
        (
            429,
            r#"{"error":{"type":"project_spend_limit_exceeded"}}"#,
            CodexFailureCategory::RateLimited,
        ),
        (
            401,
            r#"{"error":{"code":"project_spend_limit_exceeded"}}"#,
            CodexFailureCategory::CredentialExpired,
        ),
        (
            529,
            r#"{"error":{"type":"server_error","message":"overloaded"}}"#,
            CodexFailureCategory::Unavailable,
        ),
    ];

    for (status, body, expected) in cases {
        assert_eq!(
            classify(status, body),
            expected,
            "status={status} body={body}"
        );
    }
}
