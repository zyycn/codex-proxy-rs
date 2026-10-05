//! 验证上游诊断的容量错误分类、请求标识与响应事实提取

use base64::{Engine as _, engine::general_purpose::STANDARD};
use provider_openai::transport::CodexUpstreamDiagnostics;
use provider_openai::transport::diagnostics::{
    CodexFailureCategory, CodexUpstreamFailure, CodexUpstreamSendPhase,
};
use reqwest::header::{HeaderMap, HeaderValue};

fn classify(status: u16, body: &str) -> CodexFailureCategory {
    let status = reqwest::StatusCode::from_u16(status).expect("status code");
    CodexUpstreamFailure::from_response(
        status,
        body,
        None,
        &CodexUpstreamDiagnostics::with_status(status.as_u16()),
        None,
        &[],
        &[],
        CodexUpstreamSendPhase::AfterPayload,
    )
    .category()
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

fn trace_header<'a>(diagnostics: &'a CodexUpstreamDiagnostics, name: &str) -> Option<&'a str> {
    diagnostics
        .trace_headers
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
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
        (
            429,
            r#"{"error":{"type":"usage_limit_reached","message":"quota window reached"}}"#,
            CodexFailureCategory::UsageLimitExhausted,
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

#[test]
fn diagnostic_wire_dump_preserves_binary_bytes_and_numbers_every_fragment() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use gateway_core::diagnostics::TraceContext;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct Writer(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write_all(bytes)?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let output = Arc::new(Mutex::new(Vec::new()));
    let writer = Writer(Arc::clone(&output));
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    let bytes: Vec<u8> = (0..100_000).map(|index| (index % 256) as u8).collect();
    tracing::subscriber::with_default(subscriber, || {
        let trace = TraceContext::new("req_binary_dump");
        trace
            .attempt(3)
            .exchange("websocket")
            .dump("upstream.binary", &bytes);
        trace.headers(
            "client.connection.headers",
            serde_json::json!({}),
            [
                ("authorization", b"Bearer secret".as_slice()),
                ("x-binary", b"\xff\x80".as_slice()),
                ("x-binary", b"\x00".as_slice()),
            ],
        );
        let snapshot = trace.snapshot().unwrap();
        assert_eq!(snapshot["wireFrames"], 2);
        assert!(!snapshot.to_string().contains("Bearer secret"));
    });
    let log = output.lock().unwrap();
    let records: Vec<serde_json::Value> = std::str::from_utf8(&log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .filter(|record: &serde_json::Value| record["target"] == "request_dump")
        .collect();
    assert_eq!(records.len(), 5);
    let mut recovered = Vec::new();
    for (index, record) in records.iter().take(4).enumerate() {
        let fields = &record["fields"];
        assert_eq!(fields["wire_sequence"], 1);
        assert_eq!(fields["attempt_index"], 3);
        assert_eq!(fields["exchange_id"], 1);
        assert_eq!(fields["chunk_index"], index);
        assert_eq!(fields["chunk_count"], 4);
        recovered.extend(
            STANDARD
                .decode(fields["body_base64"].as_str().unwrap())
                .unwrap(),
        );
    }
    assert_eq!(recovered, bytes);
    let header_record = &records[4]["fields"];
    assert_eq!(header_record["wire_sequence"], 2);
    let header_bytes = STANDARD
        .decode(header_record["body_base64"].as_str().unwrap())
        .unwrap();
    let headers: serde_json::Value = serde_json::from_slice(&header_bytes).unwrap();
    for (index, expected) in [b"Bearer secret".as_slice(), b"\xff\x80", b"\x00"]
        .into_iter()
        .enumerate()
    {
        let value = STANDARD
            .decode(headers["headers"][index]["valueBase64"].as_str().unwrap())
            .unwrap();
        assert_eq!(value, expected);
    }
}
