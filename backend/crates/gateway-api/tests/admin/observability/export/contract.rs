//! 跨模块行为合同测试

use super::*;

#[tokio::test]
async fn diagnostic_export_requires_admin_and_exactly_one_target() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    assert_eq!(
        export(&fixture, "?errorId=missing", false).await.0,
        StatusCode::UNAUTHORIZED
    );
    for query in [
        "",
        "?requestId=a&errorId=b",
        "?requestId=",
        "?errorId=a&payload=untrusted",
    ] {
        assert_eq!(
            export(&fixture, query, true).await.0,
            StatusCode::BAD_REQUEST,
            "{query}"
        );
    }
    assert_eq!(
        export(&fixture, "?errorId=missing", true).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn diagnostic_export_keeps_retry_evidence_and_original_errors_but_not_credentials() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let mut error = ops_error_record();
    error.error_details = Some(json!({
        "causes": ["TLS certificate verification failed", "pool timeout after 5 seconds"],
        "upstream": json!({"error": {"code": "unknown_vendor_failure", "message": "quota exhausted"},
            "headers": {"Authorization": "fixture-authorization", "set-cookie": "fixture-cookie"},
            "access_token": "fixture-access", "refreshToken": "fixture-refresh",
        }).to_string(),
        "endpoint": "postgres://fixture-user:fixture-password@database:5432/main",
        "message": "connect failed: Bearer fixture-bearer, retry after 30 seconds",
    }).to_string());
    let mut unrelated = error.clone();
    unrelated.request_id = Some("another-request".into());
    unrelated.message = "unrelated evidence must not leak".into();
    fixture
        .ops_errors
        .lock()
        .unwrap()
        .extend([error, unrelated]);
    let trace = json!({"droppedEvents": 5, "droppedEvidenceEvents": 1, "stageCountOverflow": 2,
        "stageCounts": {"provider.response.parse_gap": 1}, "events": [
        {"stage": "request.routed", "data": {"accountPolicy": {"maxAccountRotations": 3}}},
        {"stage": "account.selection", "data": {"candidates": [{"id": "acct_high", "weight": 2}, {"id": "acct_low", "weight": 1}]}},
        {"stage": "attempt.failed", "data": {"kind": "unavailable", "recovery": {"replaySafe": true}}},
        {"stage": "retry.decided", "data": {"phase": "execute", "retryable": false, "clientRetrySuppressed": true, "replayBarrierReached": false}},
        {"stage": "provider.response.parse_gap", "data": {"reason": "invalid_json"}},
        {"stage": "provider.request.input", "data": {"input": {"omittedItems": 10}}},
        {"stage": "upstream.event", "data": {"truncated": true, "jsonValid": false}},
    ]});
    fixture.usage_detail.lock().unwrap().replace(UsageDetail {
        request: usage_record_with_account(
            "req_err",
            "acct_err",
            "private name",
            "fixture@example.invalid",
            "api_key",
            Utc::now(),
        ),
        attempts: Vec::new(),
        trace: Some(trace.clone()),
        related_requests: Vec::new(),
    });
    let (status, headers, file) = export(&fixture, "?requestId=req_err", true).await;
    assert_eq!(status, StatusCode::OK, "{file}");
    assert_eq!(
        headers[header::CONTENT_DISPOSITION],
        "attachment; filename=cpr-diagnostics-req_err.zip"
    );
    assert_eq!(headers[header::CACHE_CONTROL], "private, no-store");
    assert_eq!(file["schemaVersion"], 5);
    assert_eq!(
        file["exportTruncated"], false,
        "capture loss is distinct from export truncation"
    );
    for reason in [
        "events_evicted",
        "critical_evidence_evicted",
        "stage_coverage_incomplete",
        "response_parse_failed",
        "input_prefix_sample_limited",
        "event_data_truncated",
        "json_parse_failed",
    ] {
        assert!(
            file["availability"]["gaps"]
                .as_array()
                .unwrap()
                .iter()
                .any(|gap| gap["reason"] == reason),
            "missing gap {reason}"
        );
    }
    assert_eq!(file["requests"][0]["evidence"]["trace"], trace);
    assert_eq!(
        file["requests"][0]["evidence"]["incidentConfiguration"]["revision"],
        1
    );
    let details = &file["requests"][0]["errors"][0]["errorDetails"];
    assert_eq!(
        details["upstream"]["error"]["code"],
        "unknown_vendor_failure"
    );
    assert_eq!(details["causes"][0], "TLS certificate verification failed");
    assert_eq!(file["requests"][0]["errors"][0]["accountId"], "acct_err");
    assert!(
        file["availability"]["gaps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|gap| gap["section"] == "environment")
    );
    let text = file.to_string();
    for marker in [
        "fixture-authorization",
        "fixture-cookie",
        "fixture-access",
        "fixture-refresh",
        "fixture-password",
        "fixture-user",
        "fixture-bearer",
        "fixture@example.invalid",
        "private name",
        "unrelated evidence must not leak",
        "203.0.113.8",
    ] {
        assert!(!text.contains(marker), "leaked {marker}");
    }
    assert!(text.contains("retry after 30 seconds"));
}

#[tokio::test]
async fn background_error_exports_without_fabricating_a_model_request() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let mut error = ops_error_record();
    error.source = "ops_event".into();
    error.request_id = None;
    error.component = "worker".into();
    error.error_details =
        Some(json!({"causes": ["database connection refused"], "upstream": null}).to_string());
    fixture.ops_errors.lock().unwrap().push(error);
    let (status, _, file) = export(&fixture, "?errorId=err_snapshot", true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(file["requests"], json!([]));
    assert_eq!(file["error"]["requestId"], Value::Null);
    assert_eq!(
        file["error"]["errorDetails"]["causes"][0],
        "database connection refused"
    );
}

#[tokio::test]
async fn error_survives_missing_request_and_nested_credentials_are_redacted() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let mut error = ops_error_record();
    error.error_details = Some(json!({
        "cookies": "Cookie: a=fixture-cookie-a; b=fixture-cookie-b\nupstream timeout",
        "uri": "https://provider.invalid/path?api_key=fixture-query&model=gpt-test",
        "headers": [["Authorization", "fixture-tuple"], {"name": "Cookie", "value": "fixture-header"}],
    }).to_string());
    fixture.ops_errors.lock().unwrap().push(error);
    let (status, _, file) = export(&fixture, "?requestId=req_err", true).await;
    assert_eq!(status, StatusCode::OK);
    assert!(file["requests"][0]["evidence"].is_null());
    assert!(
        file["availability"]["gaps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|gap| gap["reason"] == "request_detail_unavailable")
    );
    let text = file.to_string();
    for marker in [
        "fixture-cookie-a",
        "fixture-cookie-b",
        "fixture-query",
        "fixture-tuple",
        "fixture-header",
    ] {
        assert!(!text.contains(marker), "leaked {marker}");
    }
    assert!(text.contains("upstream timeout"));
    assert!(text.contains("model=gpt-test"));
}

#[tokio::test]
async fn oversized_error_export_reports_truncation_instead_of_silent_loss() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let mut error = ops_error_record();
    error.source = "ops_event".into();
    error.request_id = None;
    error.message = "诊断文本".repeat(100_000);
    fixture.ops_errors.lock().unwrap().push(error);
    let (status, _, file) = export(&fixture, "?errorId=err_snapshot", true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(file["exportTruncated"], true);
    assert!(file["redaction"]["truncatedValues"].as_u64().unwrap() > 0);
    assert!(file["error"]["message"]["omittedBytes"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn related_read_failures_and_limits_keep_primary_evidence() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let mut related = vec![json!({"requestId": "req_err", "relation": "recovers"})];
    related.extend(
        (0..10).map(
            |index| json!({"requestId": format!("missing_{index}"), "relation": "recovered_by"}),
        ),
    );
    fixture.usage_detail.lock().unwrap().replace(UsageDetail {
        request: usage_record_with_account(
            "req_err",
            "acct_err",
            "fixture",
            "fixture@example.invalid",
            "api_key",
            Utc::now(),
        ),
        attempts: Vec::new(),
        trace: None,
        related_requests: related,
    });
    fixture
        .ops_errors
        .lock()
        .unwrap()
        .extend((0..130).map(|index| {
            let mut error = ops_error_record();
            error.event_id = format!("error_{index}");
            error
        }));
    let (status, _, file) = export(&fixture, "?requestId=req_err", true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(file["requests"].as_array().unwrap().len(), 8);
    assert_eq!(file["requests"][0]["errors"].as_array().unwrap().len(), 128);
    assert_eq!(
        file["requests"][0]["evidence"]["detail"]["requestId"],
        "req_err"
    );
    assert!(file["requests"][1]["evidence"].is_null());
    let gaps = file["availability"]["gaps"].as_array().unwrap();
    for reason in [
        "related_request_limit",
        "error_record_limit",
        "request_detail_unavailable",
        "not_recorded_or_retained",
    ] {
        assert!(
            gaps.iter().any(|gap| gap["reason"] == reason),
            "missing {reason}"
        );
    }
}

#[tokio::test]
async fn credential_schemas_are_redacted_without_guessing_business_field_names() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let mut error = ops_error_record();
    let business = json!({
        "token": {"input": 128, "output": 64},
        "credentials": {"status": "expired", "provider": "openai"},
        "secret": "tool argument name", "clientSecret": "business field description",
        "accountId": "account-correlation", "userId": "user-correlation",
        "planType": "plus", "status": "active", "accessTokenExpiresAt": "2026-10-16T00:00:00Z",
        "error": {"code": "unknown_code", "message": "upstream capacity exceeded"},
    });
    error.error_details = Some(json!({
        "business": business,
        "account": {"accessToken": "fixture-access", "refreshToken": "fixture-refresh", "idToken": "fixture-id"},
        "auth": {"OPENAI_API_KEY": "fixture-key", "personal_access_token": "fixture-pat", "tokens": {
            "access_token": "fixture-official-access", "refresh_token": "fixture-official-refresh",
            "id_token": "fixture-official-id", "account_id": "account-correlation",
        }},
    }).to_string());
    fixture.ops_errors.lock().unwrap().push(error);
    let (status, _, file) = export(&fixture, "?requestId=req_err", true).await;
    assert_eq!(status, StatusCode::OK);
    let details = &file["requests"][0]["errors"][0]["errorDetails"];
    assert_eq!(details["business"], business);
    assert_eq!(
        details["auth"]["tokens"]["account_id"],
        "account-correlation"
    );
    assert!(!file.to_string().contains("fixture-"));
}

#[tokio::test]
async fn export_budget_keeps_format_target_and_existing_gap_records() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    fixture.usage_detail.lock().unwrap().replace(UsageDetail {
        request: usage_record_with_account(
            "req_err",
            "acct_err",
            "QA",
            "qa@example.invalid",
            "oauth",
            Utc::now(),
        ),
        attempts: Vec::new(),
        trace: Some(json!({"events": vec![vec![true; 32]; 3500]})),
        related_requests: Vec::new(),
    });
    let (status, _, file) = export(&fixture, "?requestId=req_err", true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(file["schemaVersion"], 5);
    assert_eq!(file["source"], "gateway.diagnostics");
    assert_eq!(file["target"]["requestId"], "req_err");
    assert_eq!(file["exportTruncated"], true);
    let gaps = file["availability"]["gaps"].as_array().unwrap();
    assert!(gaps.iter().any(|gap| gap["reason"] == "export_size_limit"));
    assert!(
        gaps.iter()
            .any(|gap| gap["reason"] == "incident_policy_not_recorded")
    );
    assert!(serde_json::to_vec_pretty(&file).unwrap().len() < 16 * 1024 * 1024);
}

#[tokio::test]
async fn selected_event_is_exported_once_and_shares_the_total_error_budget() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let mut selected = ops_error_record();
    selected.source = "ops_event".into();
    let mut errors = vec![selected];
    errors.extend((0..129).map(|index| {
        let mut error = ops_error_record();
        error.source = "ops_event".into();
        error.event_id = format!("related_{index}");
        error
    }));
    fixture.ops_errors.lock().unwrap().extend(errors);
    let (status, _, file) = export(&fixture, "?errorId=err_snapshot", true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(file["error"]["id"], "err_snapshot");
    let errors = file["requests"][0]["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 127);
    assert!(errors.iter().all(|error| error["id"] != "err_snapshot"));
    assert!(
        file["availability"]["gaps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|gap| gap["reason"] == "error_record_limit")
    );
}

#[tokio::test]
async fn inline_authentication_headers_are_redacted_without_losing_error_context() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let mut error = ops_error_record();
    error.error_details = Some(json!({
        "headers": "x-api-key: fixture-api-key\nx-oai-is: fixture-session\nx-oai-is-update: fixture-update\nx-oai-attestation: fixture-attestation fixture-attestation-tail\nauthorization: Custom fixture-auth\nproxy-authorization: Custom fixture-proxy\nupstream timeout",
        "message": "access-token=fixture-access; refresh-token=fixture-refresh; model=gpt-test",
    }).to_string());
    fixture.ops_errors.lock().unwrap().push(error);
    let (status, _, file) = export(&fixture, "?requestId=req_err", true).await;
    assert_eq!(status, StatusCode::OK);
    let text = file.to_string();
    for marker in [
        "fixture-api-key",
        "fixture-session",
        "fixture-update",
        "fixture-attestation",
        "fixture-attestation-tail",
        "fixture-auth",
        "fixture-proxy",
        "fixture-access",
        "fixture-refresh",
    ] {
        assert!(!text.contains(marker), "leaked {marker}");
    }
    assert!(text.contains("upstream timeout"));
    assert!(text.contains("model=gpt-test"));
}
