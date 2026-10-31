//! 验证观测响应的指标、展示投影与查询时间边界

use chrono::Utc;
use serde_json::json;

#[tokio::test]
async fn usage_summary_keeps_request_attempt_cost_and_average_latency_wire_fields() {
    use crate::admin::{AdminTestFixture, AdminTestState};
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use chrono::Duration;
    use gateway_admin::model::observability::{
        AttemptMetrics, CostCoverage, CurrencyCost, RequestMetrics, TimeRange, UsageOverview,
    };
    use gateway_api::admin::observability;
    use tower::ServiceExt as _;

    for (sum, count, display, raw) in [
        (404, 3, "134 ms", Some(134)),
        (0, 1, "0 ms", Some(0)),
        (0, 0, "—", None),
    ] {
        let fixture = AdminTestFixture::new().await;
        fixture.auth.insert_session("valid-session");
        let now = Utc::now();
        fixture.observations.lock().unwrap().summary = Some(UsageOverview {
            analytics: gateway_admin::model::observability::AnalyticsMetadata {
                generated_at: now,
                ..Default::default()
            },
            range: TimeRange::new(now - Duration::hours(1), now).unwrap(),
            requests: RequestMetrics {
                request_count: 5,
                success_count: 4,
                cancelled_count: 1,
                caller_error_count: 1,
                input_tokens: 400,
                output_tokens: 80,
                cached_input_tokens: 30,
                cache_write_input_tokens: 7,
                reasoning_output_tokens: 9,
                total_tokens: 480,
                latency_sum_ms: sum,
                latency_count: count,
                ..Default::default()
            },
            attempts: AttemptMetrics {
                attempt_count: 6,
                success_count: 4,
                failure_count: 1,
                cancelled_count: 1,
                rate_limited_count: 1,
                cost_coverage: CostCoverage {
                    provider_reported_count: 3,
                    unavailable_count: 1,
                    ..Default::default()
                },
                costs: vec![CurrencyCost {
                    currency: "USD".into(),
                    amount: "1.25".parse().unwrap(),
                }],
                ..Default::default()
            },
            providers: Vec::new(),
        });
        let response = observability::router::<AdminTestState>()
            .with_state(fixture.state())
            .oneshot(
                Request::builder()
                    .uri("/api/admin/usage/records/summary?provider=openai")
                    .header(header::COOKIE, "cpr_session=valid-session")
                    .header("x-request-id", "req_usage_summary_projection")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(status, StatusCode::OK, "{value}");
        assert_eq!(
            value["data"],
            json!({
                "analytics": value["data"]["analytics"],
                "totalRequests": "5", "inputTokens": "400", "outputTokens": "80",
                "cachedInputTokens": "30", "cacheWriteInputTokens": "7", "totalTokens": "480",
                "averageLatencyMs": display,
                "averageLatencyMsValue": raw,
                "logicalRequests": {"requestCount":5,"successCount":4,"failureCount":0,"cancelledCount":1,"incompleteCount":0,"callerErrorCount":1,"inputTokens":400,"outputTokens":80,"cachedInputTokens":30,"cacheWriteInputTokens":7,"reasoningOutputTokens":9,"totalTokens":480},
                "attempts": {"attemptCount":6,"successCount":4,"failureCount":1,"cancelledCount":1,"incompleteCount":0,"rateLimitedCount":1,"authFailureCount":0,"provider5xxCount":0,"costCoverage":{"known":3,"partial":0,"unknown":1,"notBillable":0},"costs":[{"currency":"USD","estimatedAmount":"1.25"}]}
            })
        );
        let observations = fixture.observations.lock().unwrap();
        assert_eq!(observations.summaries.len(), 1);
        assert_eq!(
            observations.summaries[0].1.provider_kind.as_deref(),
            Some("openai")
        );
    }
}

#[tokio::test]
async fn dashboard_summary_should_include_quota_exhaustion_in_unavailable_headline_count() {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use chrono::{Duration, Utc};
    use gateway_admin::model::observability::{
        AccountPoolMetrics, DashboardObservation, TimeRange,
    };
    use gateway_api::admin::observability;
    use tower::ServiceExt as _;

    use crate::admin::{AdminTestFixture, AdminTestState};

    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let now = Utc::now();
    *fixture
        .dashboard_observation
        .lock()
        .expect("dashboard observation") = Some(DashboardObservation {
        yesterday: Default::default(),
        analytics: gateway_admin::model::observability::AnalyticsMetadata {
            coverage_start: Some(now - Duration::hours(1)),
            coverage_end: Some(now),
            generated_at: now,
            lag_seconds: 12,
            state: gateway_admin::model::observability::AnalyticsState::Lagging,
            percentiles_approximate: true,
            schema_version: 1,
            histogram_version: 1,
            generation: 42,
        },
        runtime_slots: None,
        range: TimeRange::new(now - Duration::hours(1), now).expect("dashboard range"),
        totals: Default::default(),
        provider_accounts: AccountPoolMetrics {
            total: 1002,
            normal: 943,
            quota_exhausted: 52,
            rate_limited: 0,
            disabled: 3,
            error: 4,
        },
        trend: Vec::new(),
        account_usage: Vec::new(),
        recent_requests: Vec::new(),
    });

    let response = observability::router::<AdminTestState>()
        .with_state(fixture.state())
        .oneshot(
            Request::builder()
                .uri("/api/admin/dashboard/summary")
                .header(header::COOKIE, "cpr_session=valid-session")
                .header("x-request-id", "req_dashboard_account_summary")
                .body(Body::empty())
                .expect("dashboard request"),
        )
        .await
        .expect("dashboard response");
    let status = response.status();
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("dashboard response body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("dashboard response JSON");
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["data"]["analytics"]["state"], "lagging");
    assert_eq!(value["data"]["analytics"]["lagSeconds"], 12);
    assert_eq!(value["data"]["analytics"]["percentilesApproximate"], true);
    assert_eq!(value["data"]["analytics"]["schemaVersion"], 1);
    assert_eq!(value["data"]["analytics"]["histogramVersion"], 1);
    assert!(value["data"]["analytics"]["coverageStart"].is_string());
    assert!(value["data"]["analytics"]["generatedAtDisplay"].is_string());
    assert!(value["data"]["analytics"].get("generation").is_none());

    assert_eq!(
        (
            &value["data"]["cards"]["credentials"]["totalValue"],
            &value["data"]["cards"]["credentials"]["availableValue"],
            &value["data"]["cards"]["credentials"]["unavailableValue"],
        ),
        (
            &serde_json::json!(1002),
            &serde_json::json!(943),
            &serde_json::json!(59)
        ),
    );
    assert_eq!(
        (
            &value["data"]["cards"]["credentials"]["total"],
            &value["data"]["cards"]["credentials"]["available"],
            &value["data"]["cards"]["credentials"]["unavailable"],
        ),
        (
            &serde_json::json!("1002"),
            &serde_json::json!("943"),
            &serde_json::json!("59")
        ),
    );
}

#[tokio::test]
async fn dashboard_summary_should_keep_yesterday_comparison_separate_from_today_and_lifetime() {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use chrono::{Duration, Utc};
    use gateway_admin::model::observability::{
        AccountPoolMetrics, DashboardObservation, DashboardTotals, RequestMetrics, TimeRange,
    };
    use gateway_api::admin::observability;
    use tower::ServiceExt as _;

    use crate::admin::{AdminTestFixture, AdminTestState};

    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let now = Utc::now();
    *fixture
        .dashboard_observation
        .lock()
        .expect("dashboard observation") = Some(DashboardObservation {
        yesterday: RequestMetrics {
            request_count: 7,
            input_tokens: 400,
            cached_input_tokens: 200,
            total_tokens: 500,
            ..Default::default()
        },
        analytics: Default::default(),
        runtime_slots: None,
        range: TimeRange::new(now - Duration::hours(1), now).expect("dashboard range"),
        totals: DashboardTotals {
            request_count: 42,
            input_tokens: 800,
            cached_input_tokens: 200,
            total_tokens: 840,
            billing_usd: None,
        },
        provider_accounts: AccountPoolMetrics::default(),
        trend: Vec::new(),
        account_usage: Vec::new(),
        recent_requests: Vec::new(),
    });

    let response = observability::router::<AdminTestState>()
        .with_state(fixture.state())
        .oneshot(
            Request::builder()
                .uri("/api/admin/dashboard/summary")
                .header(header::COOKIE, "cpr_session=valid-session")
                .header("x-request-id", "req_dashboard_lifetime_totals")
                .body(Body::empty())
                .expect("dashboard request"),
        )
        .await
        .expect("dashboard response");
    let status = response.status();
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("dashboard response body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("dashboard response JSON");
    assert_eq!(status, StatusCode::OK, "{value}");
    let cards = &value["data"]["cards"];
    assert_eq!(cards["traffic"]["todayRequestsValue"], 0);
    assert_eq!(cards["traffic"]["yesterdayRequestsValue"], 7);
    assert_eq!(cards["tokens"]["yesterdayTokensValue"], 500);
    assert_eq!(cards["cache"]["yesterdayHitRateValue"], 0.5);
    assert_eq!(
        value["data"]["analytics"]["coverageStartDisplay"],
        serde_json::Value::Null
    );
    assert_eq!(
        value["data"]["analytics"]["coverageEndDisplay"],
        serde_json::Value::Null
    );

    assert_eq!(
        (
            &value["data"]["cards"]["traffic"]["totalRequests"],
            &value["data"]["cards"]["tokens"]["totalTokens"],
            &value["data"]["cards"]["cache"]["totalHitRate"],
            &value["data"]["cards"]["cache"]["totalCachedInputTokens"],
        ),
        (
            &serde_json::json!("42"),
            &serde_json::json!("840"),
            &serde_json::json!("25.0%"),
            &serde_json::json!("200"),
        ),
    );
}

#[tokio::test]
async fn dashboard_summary_should_default_to_current_deployment_day() {
    use axum::{
        body::Body,
        http::{Request, StatusCode, header},
    };
    use chrono::{Duration, Utc};
    use gateway_admin::model::observability::{
        AccountPoolMetrics, DashboardObservation, TimeRange,
    };
    use gateway_api::admin::observability;
    use tower::ServiceExt as _;

    use crate::admin::{AdminTestFixture, AdminTestState};

    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let now = Utc::now();
    *fixture
        .dashboard_observation
        .lock()
        .expect("dashboard observation") = Some(DashboardObservation {
        yesterday: Default::default(),
        analytics: Default::default(),
        runtime_slots: None,
        range: TimeRange::new(now - Duration::hours(1), now).expect("dashboard range"),
        totals: Default::default(),
        provider_accounts: AccountPoolMetrics::default(),
        trend: Vec::new(),
        account_usage: Vec::new(),
        recent_requests: Vec::new(),
    });

    let response = observability::router::<AdminTestState>()
        .with_state(fixture.state())
        .oneshot(
            Request::builder()
                .uri("/api/admin/dashboard/summary")
                .header(header::COOKIE, "cpr_session=valid-session")
                .header("x-request-id", "req_dashboard_today_range")
                .body(Body::empty())
                .expect("dashboard request"),
        )
        .await
        .expect("dashboard response");
    let range = fixture
        .dashboard_summary_range
        .lock()
        .expect("dashboard summary range")
        .take()
        .expect("recorded dashboard summary range");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        range.start,
        gateway_core::time::DeploymentTimeZone::default()
            .day_start(range.end)
            .unwrap()
    );
}

#[tokio::test]
async fn usage_detail_should_keep_attempt_snapshot_contract() {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use chrono::Utc;
    use gateway_admin::model::observability::{UsageAttempt, UsageDetail};
    use gateway_api::admin::observability;
    use tower::ServiceExt as _;

    use crate::admin::{AdminTestFixture, AdminTestState};

    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let now = Utc::now();
    let mut record = usage_record_with_account(
        "req_detail",
        "acct_snap_a",
        "Snapshot Alpha",
        "alpha@example.invalid",
        "oauth",
        now,
    );
    record.first_token_ms = Some(20);
    record.upstream_response_ms = Some(1_000);
    record.upstream_api_overhead_ms = Some(120.25);
    record.upstream_engine_ms = Some(6400.0);
    fixture
        .usage_detail
        .lock()
        .expect("usage detail")
        .replace(UsageDetail {
            trace: None,
            related_requests: Vec::new(),
            request: record,
            attempts: vec![UsageAttempt {
                source: "ops_event".to_owned(),
                id: "ops_detail".to_owned(),
                attempt_index: 1,
                component: "routing".to_owned(),
                operation: "fallback".to_owned(),
                provider_kind: Some("openai".to_owned()),
                provider_account_ref: Some("acct_snap_b".to_owned()),
                provider_account_name: Some("Snapshot Beta".to_owned()),
                provider_account_email: Some("beta@example.invalid".to_owned()),
                provider_account_authentication_kind: Some("api_key".to_owned()),
                upstream_model_id: Some("upstream-b".to_owned()),
                upstream_transport: Some("http_sse".to_owned()),
                upstream_send_state: Some("sent".to_owned()),
                outcome: gateway_admin::model::observability::RequestOutcome::Failed,
                downstream_committed: false,
                status_code: Some(429),
                provider_error_code: Some("rate_limit".to_owned()),
                failure_kind: Some("rate_limited".to_owned()),
                retry_after_ms: Some(1_000),
                upstream_request_id: None,
                latency_ms: Some(120),
                message: Some("limited".to_owned()),
                input_tokens: None,
                output_tokens: None,
                cached_input_tokens: None,
                cache_write_input_tokens: None,
                reasoning_output_tokens: None,
                total_tokens: None,
                cost_source: Some("unavailable".to_owned()),
                cost_amount: None,
                cost_currency: None,
                occurred_at: now,
            }],
        });
    let response = observability::router::<AdminTestState>()
        .with_state(fixture.state())
        .oneshot(
            Request::builder()
                .uri("/api/admin/usage/records/detail?id=req_detail")
                .header(header::COOKIE, "cpr_session=valid-session")
                .header("x-request-id", "req_usage_detail_snapshot")
                .body(Body::empty())
                .expect("usage detail request"),
        )
        .await
        .expect("usage detail response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("usage detail body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("usage detail JSON");
    assert_eq!(value["data"]["firstTokenLatencyMs"], 20);
    assert_eq!(value["data"]["latencyDetails"]["upstreamResponseMs"], 1_000);
    assert_eq!(
        value["data"]["latencyDetails"]["upstreamApiOverheadMs"],
        json!(120.25)
    );
    assert_eq!(
        value["data"]["latencyDetails"]["upstreamEngineMs"],
        json!(6400.0)
    );
    assert_eq!(
        serde_json::json!({
            "accountId": value["data"]["accountId"],
            "accountName": value["data"]["accountName"],
            "accountEmail": value["data"]["accountEmail"],
            "authenticationKind": value["data"]["authenticationKind"],
            "attemptCredentialName": value["data"]["attempts"][0]["credentialName"],
            "attemptAccountId": value["data"]["attempts"][0]["accountId"],
            "attemptAccountName": value["data"]["attempts"][0]["accountName"],
            "attemptAccountEmail": value["data"]["attempts"][0]["accountEmail"],
            "attemptAuthenticationKind": value["data"]["attempts"][0]["authenticationKind"],
        }),
        serde_json::json!({
            "accountId": "acct_snap_a",
            "accountName": "Snapshot Alpha",
            "accountEmail": "alpha@example.invalid",
            "authenticationKind": "oauth",
            "attemptCredentialName": "Snapshot Beta",
            "attemptAccountId": "acct_snap_b",
            "attemptAccountName": "Snapshot Beta",
            "attemptAccountEmail": "beta@example.invalid",
            "attemptAuthenticationKind": "api_key",
        })
    );
}

#[tokio::test]
async fn zero_attempt_failure_detail_omits_trace_and_keeps_missing_upstream_facts() {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use gateway_admin::model::observability::{RequestOutcome, UsageDetail};
    use gateway_api::admin::observability;
    use tower::ServiceExt as _;

    use crate::admin::{AdminTestFixture, AdminTestState};

    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let mut record = usage_record_with_account("req_early", "", "", "", "", Utc::now());
    record.provider_kind = None;
    record.provider_account_ref = None;
    record.provider_account_name = None;
    record.provider_account_email = None;
    record.provider_account_authentication_kind = None;
    record.upstream_model_id = None;
    record.upstream_transport = None;
    record.http_version = None;
    record.attempt_count = 0;
    record.upstream_send_state = "not_sent".to_owned();
    record.downstream_committed_at = None;
    record.outcome = RequestOutcome::Failed;
    record.client_status_code = Some(503);
    record.upstream_status_code = None;
    record.error_kind = Some("no_available_provider".to_owned());
    record.error_message = Some("no upstream account is available".to_owned());
    record.input_tokens = None;
    record.output_tokens = None;
    record.cached_input_tokens = None;
    record.cache_write_input_tokens = None;
    record.reasoning_output_tokens = None;
    record.image_input_tokens = None;
    record.image_output_tokens = None;
    record.total_tokens = None;
    let trace = json!({
        "events": [
            {"stage": "attempt.started", "attemptIndex": 1},
            {"stage": "attempt.failed", "attemptIndex": 1},
            {"stage": "request.finished", "data": {"attempts": 0}},
        ]
    });
    fixture.usage_detail.lock().unwrap().replace(UsageDetail {
        request: record,
        attempts: Vec::new(),
        trace: Some(trace.clone()),
        related_requests: Vec::new(),
    });
    let response = observability::router::<AdminTestState>()
        .with_state(fixture.state())
        .oneshot(
            Request::builder()
                .uri("/api/admin/usage/records/detail?id=req_early")
                .header(header::COOKIE, "cpr_session=valid-session")
                .header("x-request-id", "req_zero_attempt_detail")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let data = &value["data"];
    assert_eq!(data["requestId"], "req_early");
    assert_eq!(data["attemptCount"], 0);
    assert_eq!(data["attempts"], json!([]));
    // 明细仍沿用全局 best-effort 合同，不从预备 trace 推断完整尝试列表
    assert_eq!(data["attemptsComplete"], false);
    assert!(data.get("trace").is_none());
    assert!(data.get("relatedRequests").is_none());
    assert_eq!(data["logicalOutcome"], "failed");
    assert_eq!(data["clientStatusCode"], 503);
    assert_eq!(data["message"], "no upstream account is available");
    assert_eq!(data["model"], "coding");
    for field in [
        "provider",
        "accountId",
        "upstreamModel",
        "upstreamTransport",
        "inputTokens",
        "outputTokens",
    ] {
        assert_eq!(data.get(field), Some(&serde_json::Value::Null), "{field}");
    }
}

#[tokio::test]
async fn ops_errors_should_keep_account_label_and_authentication_contract() {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use gateway_api::admin::observability;
    use tower::ServiceExt as _;

    use crate::admin::{AdminTestFixture, AdminTestState};

    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    fixture
        .ops_errors
        .lock()
        .expect("ops errors")
        .push(ops_error_record());
    let response = observability::router::<AdminTestState>()
        .with_state(fixture.state())
        .oneshot(
            Request::builder()
                .uri("/api/admin/operations/errors")
                .header(header::COOKIE, "cpr_session=valid-session")
                .header("x-request-id", "req_ops_errors_snapshot")
                .body(Body::empty())
                .expect("ops errors request"),
        )
        .await
        .expect("ops errors response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("ops errors body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("ops errors JSON");
    assert_eq!(value["data"]["items"][0]["clientApiKeyName"], "Production");
    assert_eq!(
        serde_json::json!({
            "provider": value["data"]["items"][0]["provider"],
            "authenticationKind": value["data"]["items"][0]["authenticationKind"],
            "kind": value["data"]["items"][0]["kind"],
            "accountId": value["data"]["items"][0]["accountId"],
            "accountLabel": value["data"]["items"][0]["metadata"]["accountLabel"],
            "accountPlanType": value["data"]["items"][0]["accountPlanType"],
            "accountPlanTypeDisplay": value["data"]["items"][0]["accountPlanTypeDisplay"],
            "clientStatusCode": value["data"]["items"][0]["clientStatusCode"],
            "route": value["data"]["items"][0]["route"],
            "requestedModel": value["data"]["items"][0]["requestedModel"],
            "clientTransport": value["data"]["items"][0]["clientTransport"],
            "clientIp": value["data"]["items"][0]["clientIp"],
            "userAgent": value["data"]["items"][0]["userAgent"],
            "providerErrorCode": value["data"]["items"][0]["providerErrorCode"],
            "upstreamSendState": value["data"]["items"][0]["upstreamSendState"],
            "errorDetails": value["data"]["items"][0]["errorDetails"],
            "continuationUnavailableReason": value["data"]["items"][0]["metadata"]["continuationUnavailableReason"],
            "upstreamConnectionExitReason": value["data"]["items"][0]["metadata"]["upstreamConnectionExitReason"],
            "upstreamConnectionAgeMs": value["data"]["items"][0]["metadata"]["upstreamConnectionAgeMs"],
        }),
        serde_json::json!({
            "provider": "openai",
            "authenticationKind": "api_key",
            "kind": "model_request",
            "accountId": "acct_err",
            "accountLabel": "err@example.invalid",
            "accountPlanType": "pro",
            "accountPlanTypeDisplay": "Pro",
            "clientStatusCode": 502,
            "route": "/v1/responses",
            "requestedModel": "gpt-5.4",
            "clientTransport": "http_sse",
            "clientIp": "203.0.113.8",
            "userAgent": "codex-cli/0.144.0",
            "providerErrorCode": "upstream",
            "upstreamSendState": "sent",
            "errorDetails": serde_json::to_string_pretty(&json!({"error": {"code": "upstream", "message": "raw upstream marker"}})).unwrap(),
            "continuationUnavailableReason": "reused_connection_lost",
            "upstreamConnectionExitReason": "tcp_reset",
            "upstreamConnectionAgeMs": 12000,
        })
    );
}

#[tokio::test]
async fn diagnostics_should_keep_stable_key_and_display_name_contract() {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use gateway_admin::model::observability::{
        CostCoverage, DiagnosticObservation, DiagnosticsObservation,
    };
    use gateway_api::admin::observability;
    use tower::ServiceExt as _;

    use crate::admin::{AdminTestFixture, AdminTestState};

    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    *fixture.diagnostics.lock().expect("diagnostics") = DiagnosticsObservation {
        analytics: Default::default(),
        total_request_count: 4,
        items: vec![DiagnosticObservation {
            key: "acct_diag".to_owned(),
            account_provider_kind: Some("openai".to_owned()),
            account_plan_type: Some("pro".to_owned()),
            name: "diag@example.invalid".to_owned(),
            request_count: 2,
            success_count: 2,
            failure_count: 0,
            attempt_count: 5,
            total_tokens: 200,
            average_latency_ms: Some(100),
            latency_p95_ms: Some(3800),
            first_token_p95_ms: Some(1200),
            non_completion_count: 0,
            retry_count: 3,
            retried_request_count: 1,
            cost_coverage: CostCoverage::default(),
            costs: Vec::new(),
        }],
    };
    let response = observability::router::<AdminTestState>()
        .with_state(fixture.state())
        .oneshot(
            Request::builder()
                .uri("/api/admin/usage/insights/diagnostics?dimension=account")
                .header(header::COOKIE, "cpr_session=valid-session")
                .header("x-request-id", "req_diagnostics_snapshot")
                .body(Body::empty())
                .expect("diagnostics request"),
        )
        .await
        .expect("diagnostics response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("diagnostics body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("diagnostics JSON");
    assert_eq!(value["data"]["items"][0]["requestShare"], 0.5);
    assert_eq!(value["data"]["items"][0]["retryCount"], 3);
    assert_eq!(value["data"]["items"][0]["retryRate"], 0.5);
    assert_eq!(value["data"]["items"][0]["accountPlanType"], "pro");
    assert_eq!(value["data"]["items"][0]["accountPlanTypeDisplay"], "Pro");
    assert_eq!(
        (
            &value["data"]["items"][0]["key"],
            &value["data"]["items"][0]["name"],
            &value["data"]["items"][0]["latencyP95Ms"],
            &value["data"]["dimension"],
        ),
        (
            &serde_json::json!("acct_diag"),
            &serde_json::json!("diag@example.invalid"),
            &serde_json::json!(3800),
            &serde_json::json!("account"),
        )
    );
}

pub(super) fn usage_record_with_account(
    id: &str,
    account_id: &str,
    name: &str,
    email: &str,
    authentication_kind: &str,
    started_at: chrono::DateTime<chrono::Utc>,
) -> gateway_admin::model::observability::UsageRecord {
    use gateway_admin::model::observability::{RequestOutcome, UsageRecord};

    UsageRecord {
        id: id.to_owned(),
        client_api_key_ref: "key_detail".to_owned(),
        config_revision: 1,
        routing_scope: "all".to_owned(),
        routing_group_refs: Vec::new(),
        routing_group_names_snapshot: Vec::new(),
        protocol: "openai".to_owned(),
        operation: "responses".to_owned(),
        endpoint: "/v1/responses".to_owned(),
        client_transport: "http_sse".to_owned(),
        requested_model_id: Some("coding".to_owned()),
        provider_kind: Some("openai".to_owned()),
        provider_account_ref: Some(account_id.to_owned()),
        provider_account_name: Some(name.to_owned()),
        provider_account_email: Some(email.to_owned()),
        provider_account_authentication_kind: Some(authentication_kind.to_owned()),
        upstream_model_id: Some("upstream-model".to_owned()),
        upstream_transport: Some("http_sse".to_owned()),
        http_version: Some("h2".to_owned()),
        websocket_pool: None,
        service_tier: None,
        provider_metadata_json: None,
        attempt_count: 1,
        upstream_send_state: "sent".to_owned(),
        downstream_committed_at: Some(started_at),
        outcome: RequestOutcome::Succeeded,
        client_status_code: Some(200),
        upstream_status_code: Some(200),
        client_response_id: None,
        upstream_request_id: None,
        upstream_response_id: None,
        error_kind: None,
        provider_error_code: None,
        error_message: None,
        retry_after_ms: None,
        input_tokens: Some(1),
        output_tokens: Some(1),
        cached_input_tokens: Some(0),
        cache_write_input_tokens: Some(0),
        reasoning_output_tokens: Some(0),
        image_input_tokens: Some(0),
        image_output_tokens: Some(0),
        total_tokens: Some(2),
        cost_source: "unavailable".to_owned(),
        cost_amount: None,
        cost_currency: None,
        billing: None,
        transport_decision_wait_ms: None,
        connect_ms: None,
        headers_ms: None,
        first_event_ms: None,
        first_reasoning_ms: None,
        first_text_ms: None,
        first_token_ms: None,
        provider_processing_ms: None,
        upstream_response_ms: None,
        upstream_api_overhead_ms: None,
        upstream_engine_ms: None,
        latency_ms: None,
        admission_decision_ms: None,
        account_selection_wait_ms: None,
        capacity_used_slots: None,
        capacity_total_slots: None,
        client_ip: None,
        user_agent: None,
        reasoning_effort: None,
        reasoning_preset: None,
        request_kind: None,
        subagent_kind: None,
        compact: false,
        image_generation_requested: false,
        image_generation_succeeded: None,
        started_at,
        deadline_at: started_at + chrono::Duration::seconds(30),
        completed_at: Some(started_at),
    }
}

#[tokio::test]
async fn usage_route_should_forward_name_and_model_contains_filters() {
    use axum::{
        body::Body,
        http::{Request, StatusCode, header},
    };
    use gateway_api::admin::observability;
    use tower::ServiceExt as _;

    use crate::admin::{AdminTestFixture, AdminTestState};

    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let response = observability::router::<AdminTestState>()
        .with_state(fixture.state())
        .oneshot(
            Request::builder()
                .uri("/api/admin/usage/records?model=%20coding%20&clientApiKeyName=%20Key%20%25%20&accountName=%20Account%20Name%20")
                .header(header::COOKIE, "cpr_session=valid-session")
                .header("x-request-id", "req_usage_other_outcome")
                .body(Body::empty())
                .expect("usage request"),
        )
        .await
        .expect("usage response");

    assert_eq!(response.status(), StatusCode::OK);
    let observations = fixture.observations.lock().unwrap();
    let filter = &observations.records.last().expect("usage query").filter;
    assert_eq!(filter.client_api_key_name.as_deref(), Some("Key %"));
    assert_eq!(filter.model_contains.as_deref(), Some("coding"));
    assert_eq!(
        filter.provider_account_name.as_deref(),
        Some("Account Name")
    );
    assert!(filter.client_api_key_ref.is_none());
    assert!(filter.provider_account_ref.is_none());
}

#[tokio::test]
async fn usage_route_should_expose_table_facts_without_detail_payload() {
    use std::str::FromStr as _;

    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use chrono::Utc;
    use gateway_admin::model::observability::{
        CalculatedBillingBreakdown, CurrencyCost, DecimalAmount, UsageBilling, UsageListRecord,
    };
    use gateway_api::admin::observability;
    use tower::ServiceExt as _;

    use crate::admin::{AdminTestFixture, AdminTestState};

    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let usd = |amount: &str| CurrencyCost {
        currency: "USD".to_owned(),
        amount: DecimalAmount::from_str(amount).expect("USD amount"),
    };
    fixture
        .usage_records
        .lock()
        .expect("usage records")
        .push(UsageListRecord {
            error_code: None,
            error_message: None,
            client_status_code: None,
            outcome: gateway_admin::model::observability::RequestOutcome::Succeeded,
            client_api_key_name: Some("Production".to_owned()),
            id: "request_endpoint".to_owned(),
            endpoint: "/v1/responses".to_owned(),
            client_transport: "websocket".to_owned(),
            requested_model_id: Some("grok-4.5".to_owned()),
            provider_kind: Some("xai".to_owned()),
            provider_account_ref: Some("acct_snapshot".to_owned()),
            provider_account_name: Some("Snapshot Alpha".to_owned()),
            provider_account_email: Some("alpha@example.invalid".to_owned()),
            provider_account_notes: Some("Team workspace".to_owned()),
            provider_account_plan_type: Some("supergrok_plus".to_owned()),
            provider_account_plan_type_display: None,
            provider_account_authentication_kind: Some("oauth".to_owned()),
            upstream_model_id: Some("grok-4.5".to_owned()),
            upstream_transport: Some("http_sse".to_owned()),
            service_tier: Some("default".to_owned()),
            input_tokens: Some(1),
            output_tokens: Some(1),
            cached_input_tokens: Some(0),
            cache_write_input_tokens: Some(0),
            reasoning_output_tokens: Some(0),
            image_input_tokens: Some(31),
            image_output_tokens: Some(9),
            total_tokens: Some(2),
            cost_source: "unavailable".to_owned(),
            cost_amount: None,
            cost_currency: None,
            billing: Some(UsageBilling::Calculated(Box::new(
                CalculatedBillingBreakdown {
                    long_context_billing_applied: true,
                    custom_multiplier_bps: 10_000,
                    image: None,
                    input_amount: usd("0.03"),
                    output_amount: usd("0.07"),
                    cache_read_amount: usd("0.00"),
                    cache_write_amount: usd("0.00"),
                    standard_amount: usd("0.10"),
                    total_amount: usd("0.10"),
                    input_price_per_million: usd("10.0000"),
                    output_price_per_million: usd("60.0000"),
                    cache_read_price_per_million: usd("1.0000"),
                    cache_write_price_per_million: usd("12.5000"),
                    service_tier: Some("default".to_owned()),
                    multiplier_percent: 100,
                },
            ))),
            transport_decision_wait_ms: Some(4),
            connect_ms: Some(5),
            headers_ms: Some(7),
            first_event_ms: Some(17),
            first_reasoning_ms: Some(18),
            first_text_ms: Some(19),
            first_token_ms: Some(20),
            provider_processing_ms: Some(21),
            upstream_response_ms: Some(2_000),
            upstream_api_overhead_ms: None,
            upstream_engine_ms: None,
            latency_ms: Some(31),
            admission_decision_ms: Some(1),
            account_selection_wait_ms: Some(2),
            capacity_used_slots: Some(3),
            capacity_total_slots: Some(8),
            client_ip: Some("192.0.2.10".to_owned()),
            user_agent: Some("codex-cli/1.0".to_owned()),
            reasoning_effort: Some("max".to_owned()),
            reasoning_preset: Some("ultra".to_owned()),
            subagent_kind: Some("worker".to_owned()),
            compact: true,
            started_at: Utc::now(),
        });
    {
        let mut records = fixture.usage_records.lock().expect("usage records");
        for source in ["calculated", "provider_reported"] {
            let mut image = records[0].clone();
            image.id = format!("image_{source}");
            image.endpoint = "/v1/images/generations".to_owned();
            image.provider_kind = Some("openai".to_owned());
            image.requested_model_id = Some("gpt-image-2".to_owned());
            image.upstream_model_id = None;
            image.billing = Some(UsageBilling::Total {
                source: source.to_owned(),
                total: usd("0.00696"),
            });
            records.push(image);
        }
        let mut failed = records[0].clone();
        failed.id = "request_failed".to_owned();
        failed.outcome = gateway_admin::model::observability::RequestOutcome::Failed;
        failed.error_code = Some("server_error".to_owned());
        failed.error_message = Some("The upstream service is temporarily unavailable.".to_owned());
        records.push(failed);
    }
    let response = observability::router::<AdminTestState>()
        .with_state(fixture.state())
        .oneshot(
            Request::builder()
                .uri("/api/admin/usage/records?status=all")
                .header(header::COOKIE, "cpr_session=valid-session")
                .header("x-request-id", "req_usage_endpoint")
                .body(Body::empty())
                .expect("usage request"),
        )
        .await
        .expect("usage response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("usage response body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("usage response JSON");

    assert!(value["data"]["items"][0]["errorCode"].is_null());
    assert!(value["data"]["items"][0]["errorMessage"].is_null());
    assert_eq!(value["data"]["items"][3]["errorCode"], "server_error");
    assert_eq!(
        value["data"]["items"][3]["errorMessage"],
        "The upstream service is temporarily unavailable."
    );
    assert_eq!(value["data"]["items"][0]["clientApiKeyName"], "Production");
    assert_eq!(value["data"]["items"][0]["accountNotes"], "Team workspace");
    assert_eq!(
        value["data"]["items"][0]["accountPlanType"],
        "supergrok_plus"
    );
    assert_eq!(
        value["data"]["items"][0]["accountPlanTypeDisplay"],
        "SupergrokPlus"
    );
    assert_eq!(
        value["data"]["items"][0]["billing"]["longContextBillingApplied"],
        true
    );
    assert_eq!(
        value["data"]["items"][1]["billing"]["longContextBillingApplied"],
        false
    );
    assert_eq!(
        value["data"]["items"][0]["billing"]["inputPriceDisplay"],
        "$10 / 1M Token"
    );
    assert_eq!(
        value["data"]["items"][0]["billing"]["outputPriceDisplay"],
        "$60 / 1M Token"
    );
    assert_eq!(
        value["data"]["items"][0]["billing"]["cacheWritePriceDisplay"],
        "$12.5 / 1M Token"
    );
    assert_eq!(
        value["data"]["items"][0]["billing"]["serviceTierDisplay"],
        "Standard"
    );

    assert_eq!(
        serde_json::json!({
            "route": value["data"]["items"][0]["route"],
            "serviceTier": value["data"]["items"][0]["serviceTier"],
            "accountId": value["data"]["items"][0]["accountId"],
            "accountName": value["data"]["items"][0]["accountName"],
            "accountEmail": value["data"]["items"][0]["accountEmail"],
            "authenticationKind": value["data"]["items"][0]["authenticationKind"],
            "imageInputTokens": value["data"]["items"][0]["tokenDetails"]["imageInputTokens"],
            "imageOutputTokens": value["data"]["items"][0]["tokenDetails"]["imageOutputTokens"],
            "requestedModel": value["data"]["items"][0]["requestedModel"],
            "upstreamModel": value["data"]["items"][0]["upstreamModel"],
            "reasoningEffort": value["data"]["items"][0]["reasoningEffort"],
            "reasoningPreset": value["data"]["items"][0]["reasoningPreset"],
            "subagentKind": value["data"]["items"][0]["subagentKind"],
            "compact": value["data"]["items"][0]["compact"],
            "clientTransport": value["data"]["items"][0]["clientTransport"],
            "upstreamTransport": value["data"]["items"][0]["upstreamTransport"],
            "latencyDetails": value["data"]["items"][0]["latencyDetails"],
            "firstTokenLatencyMs": value["data"]["items"][0]["firstTokenLatencyMs"],
            "latencyMs": value["data"]["items"][0]["latencyMs"],
            "clientIp": value["data"]["items"][0]["clientIp"],
            "userAgent": value["data"]["items"][0]["userAgent"],
        }),
        serde_json::json!({
            "route": "/v1/responses",
            "serviceTier": "default",
            "accountId": "acct_snapshot",
            "accountName": "Snapshot Alpha",
            "accountEmail": "alpha@example.invalid",
            "authenticationKind": "oauth",
            "imageInputTokens": 31,
            "imageOutputTokens": 9,
            "requestedModel": "grok-4.5",
            "upstreamModel": "grok-4.5",
            "reasoningEffort": "max",
            "reasoningPreset": "ultra",
            "subagentKind": "worker",
            "compact": true,
            "clientTransport": "websocket",
            "upstreamTransport": "http_sse",
            "latencyDetails": {
                "admissionDecisionMs": 1,
                "accountSelectionWaitMs": 2,
                "capacityUsedSlots": 3,
                "capacityTotalSlots": 8,
                "transportDecisionWaitMs": 4,
                "wsConnectMs": 5,
                "upstreamHeadersMs": 7,
                "firstEventMs": 17,
                "firstReasoningMs": 18,
                "firstTextMs": 19,
                "openaiProcessingMs": 21,
                "upstreamResponseMs": 2000,
            },
            "firstTokenLatencyMs": 20,
            "latencyMs": 31,
            "clientIp": "192.0.2.10",
            "userAgent": "codex-cli/1.0",
        })
    );
    assert!(value["data"]["items"][0].get("metadata").is_none());
    assert_eq!(
        value["data"]["items"][1]["billing"]["totalAmountDisplay"],
        "≈ $0.007"
    );
    assert_eq!(
        value["data"]["items"][2]["billing"]["totalAmountDisplay"],
        "$0.007"
    );
}

#[tokio::test]
async fn calendar_queries_and_display_follow_deployment_timezone_with_one_anchor() {
    use crate::admin::{AdminTestFixture, AdminTestState};
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use gateway_admin::model::observability::{CalendarPeriod, DashboardObservation, TimeRange};
    use gateway_api::admin::observability;
    use gateway_core::time::DeploymentTimeZone;
    use tower::ServiceExt as _;

    for (name, anchor, start, display, slots) in [
        (
            "Asia/Shanghai",
            "2026-10-01T00:00:00Z",
            "2026-09-30T16:00:00Z",
            "2026-10-01 08:00:00",
            96,
        ),
        (
            "UTC",
            "2026-10-01T00:00:00Z",
            "2026-10-01T00:00:00Z",
            "2026-10-01 00:00:00",
            96,
        ),
        (
            "Asia/Kathmandu",
            "2026-10-01T00:00:00Z",
            "2026-09-30T18:15:00Z",
            "2026-10-01 05:45:00",
            96,
        ),
        (
            "America/New_York",
            "2026-03-09T03:59:00Z",
            "2026-03-08T05:00:00Z",
            "2026-03-08 23:59:00",
            92,
        ),
        (
            "America/New_York",
            "2026-11-02T04:59:00Z",
            "2026-11-01T04:00:00Z",
            "2026-11-01 23:59:00",
            100,
        ),
    ] {
        let timezone: DeploymentTimeZone = name.parse().unwrap();
        let end: chrono::DateTime<Utc> = anchor.parse().unwrap();
        let range = TimeRange::calendar_at(CalendarPeriod::Today, end, timezone).unwrap();
        let fixture = AdminTestFixture::with_timezone(timezone).await;
        fixture.auth.insert_session("valid-session");
        *fixture.dashboard_observation.lock().unwrap() = Some(DashboardObservation {
            yesterday: Default::default(),
            analytics: gateway_admin::model::observability::AnalyticsMetadata {
                coverage_start: Some(range.start),
                coverage_end: Some(end),
                generated_at: end,
                ..Default::default()
            },
            runtime_slots: None,
            range,
            totals: Default::default(),
            provider_accounts: Default::default(),
            trend: Vec::new(),
            account_usage: Vec::new(),
            recent_requests: Vec::new(),
        });
        let router = observability::router::<AdminTestState>().with_state(fixture.state());
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/admin/dashboard/summary?period=today&asOf={}",
                        end.timestamp_millis()
                    ))
                    .header(header::COOKIE, "cpr_session=valid-session")
                    .header("x-request-id", "req_timezone_calendar")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        assert_eq!(
            status,
            StatusCode::OK,
            "{name}: {}",
            String::from_utf8_lossy(&bytes)
        );
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let queried = fixture.dashboard_summary_range.lock().unwrap().unwrap();
        assert_eq!(
            queried.start,
            start.parse::<chrono::DateTime<Utc>>().unwrap()
        );
        assert_eq!(queried.end, end);
        assert_eq!(value["data"]["asOf"], anchor);
        assert_eq!(value["data"]["asOfDisplay"], display);
        let analytics = &value["data"]["analytics"];
        assert_eq!(analytics["coverageStart"], start);
        assert_eq!(analytics["coverageEnd"], anchor);
        assert_eq!(
            analytics["coverageStartDisplay"],
            format!("{} 00:00:00", &display[..10])
        );
        assert_eq!(analytics["coverageEndDisplay"], display);
        assert_eq!(analytics["generatedAtDisplay"], display);
        let points = value["data"]["healthTimeline"]["points"]
            .as_array()
            .unwrap();
        assert_eq!(points.len(), slots);
        let labels = points
            .iter()
            .map(|p| p["time"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(labels.iter().all(|label| label.len() == 5));
        let buckets = points
            .iter()
            .map(|p| p["bucketStart"].as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(buckets.len(), slots, "fold buckets remain distinct");
        if slots == 100 {
            assert_eq!(labels.iter().filter(|label| **label == "01:00").count(), 2);
        }
        let invalid = router.oneshot(Request::builder()
            .uri(format!("/api/admin/dashboard/summary?period=today&asOf={}&startTime=2026-01-01T00%3A00%3A00Z", end.timestamp_millis()))
            .header(header::COOKIE, "cpr_session=valid-session")
            .header("x-request-id", "req_timezone_mixed_range")
            .body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn calendar_query_rejects_an_anchor_outside_the_local_calendar() {
    use crate::admin::{AdminTestFixture, AdminTestState};
    use axum::{
        body::Body,
        http::{Request, StatusCode, header},
    };
    use gateway_api::admin::observability;
    use tower::ServiceExt as _;

    let fixture = AdminTestFixture::with_timezone(Default::default()).await;
    fixture.auth.insert_session("valid-session");
    let response = observability::router::<AdminTestState>()
        .with_state(fixture.state())
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/admin/dashboard/summary?period=today&asOf={}",
                    chrono::DateTime::<chrono::Utc>::MAX_UTC.timestamp_millis()
                ))
                .header(header::COOKIE, "cpr_session=valid-session")
                .header("x-request-id", "req_timezone_out_of_range")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(fixture.dashboard_summary_range.lock().unwrap().is_none());
}

#[tokio::test]
async fn usage_insights_maps_load_summary_and_points_to_camel_case() {
    use crate::admin::{AdminTestFixture, AdminTestState};
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use chrono::Duration;
    use gateway_admin::model::observability::{
        Granularity, LatencyPercentiles, PercentileMilliseconds, RequestMetricPoint,
        RequestMetrics, TimeRange, UsageConcurrencyPoint, UsageOverview,
    };
    use gateway_api::admin::observability;
    use tower::ServiceExt as _;

    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let now = Utc::now();
    let range = TimeRange::new(now - Duration::minutes(15), now).unwrap();
    let bucket = range.start;
    {
        let mut observations = fixture.observations.lock().unwrap();
        observations.summary = Some(UsageOverview {
            analytics: Default::default(),
            range,
            requests: RequestMetrics {
                admission_decision_percentiles: LatencyPercentiles {
                    p95_ms: Some(PercentileMilliseconds::new(12.5).unwrap()),
                    ..Default::default()
                },
                account_selection_wait_percentiles: LatencyPercentiles {
                    p95_ms: Some(PercentileMilliseconds::new(128.75).unwrap()),
                    ..Default::default()
                },
                ..Default::default()
            },
            attempts: Default::default(),
            providers: Vec::new(),
        });
        observations.trend = Some(vec![RequestMetricPoint {
            bucket_start: bucket,
            granularity: Granularity::FifteenMinutes,
            metrics: RequestMetrics {
                admission_decision_percentiles: LatencyPercentiles {
                    p95_ms: Some(PercentileMilliseconds::new(2.5).unwrap()),
                    ..Default::default()
                },
                ..Default::default()
            },
            cost_coverage: Default::default(),
            costs: Vec::new(),
        }]);
        observations.concurrency = vec![UsageConcurrencyPoint {
            bucket_start: bucket,
            average_concurrency: 1.25,
            peak_concurrency: 3,
        }];
    }

    let response = observability::router::<AdminTestState>()
        .with_state(fixture.state())
        .oneshot(
            Request::builder()
                .uri("/api/admin/usage/insights/overview")
                .header(header::COOKIE, "cpr_session=valid-session")
                .header("x-request-id", "req_usage_insights_load")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(status, StatusCode::OK, "{value}");
    let performance = &value["data"]["performance"];
    assert_eq!(
        performance["load"],
        json!({
            "averageConcurrency": 1.25,
            "peakConcurrency": 3,
            "admissionDecisionP95Ms": 12.5,
            "accountSelectionWaitP95Ms": 128.75,
        })
    );
    assert_eq!(
        performance["points"][0]["load"],
        json!({
            "averageConcurrency": 1.25,
            "peakConcurrency": 3,
            "admissionDecisionP95Ms": 2.5,
            "accountSelectionWaitP95Ms": null,
        })
    );
}

#[tokio::test]
async fn observability_queries_reject_group_filters_and_analytics_search() {
    use crate::admin::{AdminTestFixture, AdminTestState};
    use axum::{
        body::Body,
        http::{Request, StatusCode, header},
    };
    use gateway_api::admin::observability;
    use tower::ServiceExt as _;
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let router = observability::router::<AdminTestState>().with_state(fixture.state());
    for (path, parameter) in [
        ("/api/admin/usage/records", "routingGroupRefs=group-a"),
        ("/api/admin/operations/errors", "routingGroupRefs=group-a"),
        (
            "/api/admin/usage/records/summary",
            "routingGroupRefs=group-a",
        ),
        (
            "/api/admin/usage/insights/overview",
            "routingGroupRefs=group-a",
        ),
        (
            "/api/admin/usage/insights/diagnostics",
            "routingGroupRefs=group-a",
        ),
        ("/api/admin/usage/records/summary", "search=request-prefix"),
        (
            "/api/admin/usage/insights/overview",
            "search=request-prefix",
        ),
        (
            "/api/admin/usage/insights/diagnostics",
            "search=request-prefix",
        ),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("{path}?{parameter}"))
                    .header(header::COOKIE, "cpr_session=valid-session")
                    .header("x-request-id", "req_rejected_observability_filter")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "{path}?{parameter}"
        );
    }
    let observations = fixture.observations.lock().unwrap();
    assert!(observations.records.is_empty());
    assert!(observations.errors.is_empty());
}

pub(super) fn ops_error_record() -> gateway_admin::model::observability::OpsError {
    use gateway_admin::model::observability::OpsError;
    OpsError {
        client_api_key_name: Some("Production".to_owned()),
        source: "model_request".to_owned(),
        event_id: "err_snapshot".to_owned(),
        request_id: Some("req_err".to_owned()),
        attempt_index: Some(1),
        client_api_key_ref: Some("key_err".to_owned()),
        component: "model_request".to_owned(),
        operation: "responses".to_owned(),
        protocol: Some("openai".to_owned()),
        client_transport: Some("http_sse".to_owned()),
        requested_model_id: Some("gpt-5.4".to_owned()),
        service_tier: Some("priority".to_owned()),
        endpoint: Some("/v1/responses".to_owned()),
        provider_kind: Some("openai".to_owned()),
        provider_account_ref: Some("acct_err".to_owned()),
        provider_account_name: None,
        provider_account_email: Some("err@example.invalid".to_owned()),
        provider_account_plan_type: Some("pro".to_owned()),
        provider_account_plan_type_display: Some("Pro".to_owned()),
        provider_account_authentication_kind: Some("api_key".to_owned()),
        upstream_model_id: Some("upstream-err".to_owned()),
        upstream_transport: Some("http_sse".to_owned()),
        failure_kind: "upstream_error".to_owned(),
        upstream_send_state: Some("sent".to_owned()),
        client_status_code: Some(502),
        upstream_status_code: Some(502),
        provider_error_code: Some("upstream".to_owned()),
        client_response_id: None,
        upstream_request_id: None,
        latency_ms: Some(90),
        message: "snapshot error".to_owned(),
        error_details: Some(
            r#"{"error":{"code":"upstream","message":"raw upstream marker"}}"#.to_owned(),
        ),
        client_ip: Some("203.0.113.8".to_owned()),
        user_agent: Some("codex-cli/0.144.0".to_owned()),
        reasoning_effort: Some("medium".to_owned()),
        reasoning_preset: None,
        request_kind: Some("root".to_owned()),
        subagent_kind: None,
        compact: Some(false),
        continuation_affinity_hash: Some("affinity-hash".to_owned()),
        continuation_previous_response_id_hash: Some("response-hash".to_owned()),
        continuation_unavailable_reason: Some("reused_connection_lost".to_owned()),
        upstream_connection_id: Some("connection-id".to_owned()),
        upstream_connection_exit_reason: Some("tcp_reset".to_owned()),
        upstream_connection_age_ms: Some(12_000),
        upstream_connection_idle_ms: Some(4_000),
        recovery_request_id: None,
        recovered_at: None,
        recovery_attempt_count: 0,
        recovery_retry_delay_ms: None,
        recovery_total_latency_ms: None,
        occurred_at: Utc::now(),
        stable_sort_id: "model_request:req_err".to_owned(),
    }
}

#[tokio::test]
async fn usage_status_query_accepts_three_modes_and_rejects_unknown_values() {
    use crate::admin::{AdminTestFixture, AdminTestState};
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use gateway_api::admin::observability;
    use tower::ServiceExt as _;
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    for (status, expected) in [
        ("success", StatusCode::OK),
        ("error", StatusCode::OK),
        ("all", StatusCode::OK),
        ("failed", StatusCode::BAD_REQUEST),
    ] {
        let response = observability::router::<AdminTestState>()
            .with_state(fixture.state())
            .oneshot(
                Request::builder()
                    .uri(format!("/api/admin/usage/records?status={status}"))
                    .header(header::COOKIE, "cpr_session=valid-session")
                    .header("x-request-id", "usage_status_fixture")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let actual = response.status();
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        assert_eq!(
            actual,
            expected,
            "{status}: {}",
            String::from_utf8_lossy(&body)
        );
        if expected == StatusCode::OK {
            use gateway_admin::model::observability::UsageRecordStatus;
            let status_value = match status {
                "success" => UsageRecordStatus::Success,
                "error" => UsageRecordStatus::Error,
                _ => UsageRecordStatus::All,
            };
            assert_eq!(
                fixture
                    .observations
                    .lock()
                    .unwrap()
                    .records
                    .last()
                    .unwrap()
                    .status,
                status_value
            );
        }
    }
}

#[tokio::test]
async fn dashboard_summary_should_identify_metrics_without_localized_labels() {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use chrono::Duration;
    use gateway_admin::model::{
        accounts::AccountUsage,
        observability::{DashboardAccountUsage, DashboardObservation, TimeRange},
        provider_credentials::{ProviderQuotaWindow, QuotaLocalUsageAttribution},
    };
    use gateway_api::admin::observability;
    use tower::ServiceExt as _;

    use crate::admin::{AdminTestFixture, AdminTestState};

    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let now = Utc::now();
    *fixture.dashboard_observation.lock().unwrap() = Some(DashboardObservation {
        yesterday: Default::default(),
        analytics: Default::default(),
        runtime_slots: None,
        range: TimeRange::new(now - Duration::hours(1), now).unwrap(),
        totals: Default::default(),
        provider_accounts: Default::default(),
        trend: Vec::new(),
        account_usage: [
            (None, Some(2_000), Some(now)),
            (None, Some(0), None),
            (None, None, None),
            (Some(9), Some(2_000), Some(now)),
            (Some(0), Some(2_000), None),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (requests, tokens, last_used_at))| {
            let account_id = format!("acct_metric_{index}");
            DashboardAccountUsage {
                upstream: None,
                account_id: account_id.clone(),
                provider_kind: "openai".to_owned(),
                authentication_kind: "oauth".to_owned(),
                name: account_id.clone(),
                email: None,
                notes: None,
                plan_type: None,
                plan_type_display: None,
                request_count: 10,
                success_count: 10,
                input_tokens: None,
                output_tokens: None,
                cached_input_tokens: None,
                cache_write_input_tokens: None,
                reasoning_output_tokens: None,
                image_input_tokens: None,
                image_output_tokens: None,
                image_request_count: 0,
                image_request_failed_count: 0,
                total_tokens: tokens,
                cost_coverage: Default::default(),
                costs: Vec::new(),
                last_used_at,
                request_buckets: Vec::new(),
                quota_used_percent: None,
                quota_window: requests.map(|request_count| ProviderQuotaWindow {
                    upstream: None,
                    key: "rolling".to_owned(),
                    group: "shortTerm".to_owned(),
                    label: "滚动窗口".to_owned(),
                    limit_id: None,
                    limit_name: None,
                    role: None,
                    local_usage_attribution: QuotaLocalUsageAttribution::AccountWide,
                    window_seconds: Some(86_400),
                    used_percent: None,
                    reset_at: None,
                    limit_reached: false,
                    local_usage: Some(AccountUsage {
                        account_id,
                        request_count,
                        success_count: request_count,
                        input_tokens: None,
                        output_tokens: None,
                        cached_input_tokens: None,
                        cache_write_input_tokens: None,
                        reasoning_output_tokens: None,
                        image_input_tokens: None,
                        image_output_tokens: None,
                        image_request_count: 0,
                        image_request_failed_count: 0,
                        total_tokens: None,
                        cost_coverage: Default::default(),
                        costs: Vec::new(),
                        last_used_at,
                        request_buckets: Vec::new(),
                        models: Vec::new(),
                    }),
                    provider_data: None,
                }),
                models: Vec::new(),
            }
        })
        .collect(),
        recent_requests: Vec::new(),
    });

    for (kind, metrics) in [
        (
            "usage",
            ["inputTokens", "outputTokens", "cachedInputTokens"],
        ),
        (
            "latency",
            ["firstTokenP95Ms", "latencyP95Ms", "outputThroughputP50"],
        ),
        ("errors", ["errors", "successRate", "requests"]),
    ] {
        let response = observability::router::<AdminTestState>()
            .with_state(fixture.state())
            .oneshot(
                Request::builder()
                    .uri(format!("/api/admin/dashboard/summary?kind={kind}"))
                    .header(header::COOKIE, "cpr_session=valid-session")
                    .header(header::ACCEPT_LANGUAGE, "en")
                    .header("x-request-id", format!("req_trend_metrics_{kind}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(status, StatusCode::OK, "{kind}: {value}");
        let accounts = value["data"]["accountUsage"].as_array().unwrap();
        assert_eq!(accounts.len(), 5);
        for (account, (metric, raw, display, last_used_at)) in accounts.iter().zip([
            ("tokens", Some(2_000), "2K", Some(now)),
            ("tokens", Some(0), "0", None),
            ("tokens", None, "—", None),
            ("requests", Some(9), "9", Some(now)),
            ("requests", Some(0), "—", None),
        ]) {
            assert_eq!(account["metric"], metric);
            assert_eq!(account["metricValueRaw"], json!(raw));
            assert_eq!(account["metricValue"], display);
            assert_eq!(account["lastUsedAt"], json!(last_used_at));
        }
        let summary = value["data"]["trend"]["summary"].as_array().unwrap();
        assert_eq!(summary.len(), metrics.len());
        for (item, metric) in summary.iter().zip(metrics) {
            assert_eq!(item["metric"], metric);
            assert!(item.get("label").is_none());
            assert!(item["value"].is_string());
        }
    }
}

#[tokio::test]
async fn dashboard_relative_time_preserves_server_anchor_boundaries_and_deployment_timezone() {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use chrono::{DateTime, Duration};
    use gateway_admin::model::observability::{
        DashboardAccountUsage, DashboardObservation, TimeRange,
    };
    use gateway_api::admin::observability;
    use tower::ServiceExt as _;

    use crate::admin::{AdminTestFixture, AdminTestState};

    let now = "2026-06-15T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
    for (zone, future_display) in [
        ("UTC", "2026-06-15 00:00:01"),
        ("Asia/Shanghai", "2026-06-15 08:00:01"),
        ("Asia/Kathmandu", "2026-06-15 05:45:01"),
        ("America/New_York", "2026-06-14 20:00:01"),
    ] {
        for (elapsed_ms, expected, legacy) in [
            (None, json!(null), "—"),
            (
                Some(-1_000),
                json!({ "kind": "absolute", "value": future_display }),
                future_display,
            ),
            (Some(-1), json!({ "kind": "just_now" }), "刚刚"),
            (Some(0), json!({ "kind": "just_now" }), "刚刚"),
            (Some(59_999), json!({ "kind": "just_now" }), "刚刚"),
            (
                Some(60_000),
                json!({ "kind": "elapsed", "value": 1, "unit": "minute" }),
                "1 分钟前",
            ),
            (
                Some(119_999),
                json!({ "kind": "elapsed", "value": 1, "unit": "minute" }),
                "1 分钟前",
            ),
            (
                Some(3_599_999),
                json!({ "kind": "elapsed", "value": 59, "unit": "minute" }),
                "59 分钟前",
            ),
            (
                Some(3_600_000),
                json!({ "kind": "elapsed", "value": 1, "unit": "hour" }),
                "1 小时前",
            ),
            (
                Some(86_399_999),
                json!({ "kind": "elapsed", "value": 23, "unit": "hour" }),
                "23 小时前",
            ),
            (
                Some(86_400_000),
                json!({ "kind": "elapsed", "value": 1, "unit": "day" }),
                "1 天前",
            ),
            (
                Some(34_560_000_000),
                json!({ "kind": "elapsed", "value": 400, "unit": "day" }),
                "400 天前",
            ),
        ] {
            // 每个边界使用独立缓存，避免相同锚点复用上一轮观测数据
            let fixture = AdminTestFixture::with_timezone(zone.parse().unwrap()).await;
            fixture.auth.insert_session("valid-session");
            let last_used_at = elapsed_ms.map(|value| now - Duration::milliseconds(value));
            *fixture.dashboard_observation.lock().unwrap() = Some(DashboardObservation {
                yesterday: Default::default(),
                analytics: Default::default(),
                runtime_slots: None,
                range: TimeRange::new(now - Duration::hours(1), now).unwrap(),
                totals: Default::default(),
                provider_accounts: Default::default(),
                trend: Vec::new(),
                account_usage: vec![DashboardAccountUsage {
                    upstream: None,
                    account_id: "acct_relative_time".to_owned(),
                    provider_kind: "openai".to_owned(),
                    authentication_kind: "oauth".to_owned(),
                    name: "Relative time fixture".to_owned(),
                    email: None,
                    notes: None,
                    plan_type: None,
                    plan_type_display: None,
                    request_count: 1,
                    success_count: 1,
                    input_tokens: None,
                    output_tokens: None,
                    cached_input_tokens: None,
                    cache_write_input_tokens: None,
                    reasoning_output_tokens: None,
                    image_input_tokens: None,
                    image_output_tokens: None,
                    image_request_count: 0,
                    image_request_failed_count: 0,
                    total_tokens: None,
                    cost_coverage: Default::default(),
                    costs: Vec::new(),
                    last_used_at,
                    request_buckets: Vec::new(),
                    quota_used_percent: None,
                    quota_window: None,
                    models: Vec::new(),
                }],
                recent_requests: Vec::new(),
            });
            let response = observability::router::<AdminTestState>()
                .with_state(fixture.state())
                .oneshot(
                    Request::builder()
                        .uri(format!(
                            "/api/admin/dashboard/summary?asOf={}",
                            now.timestamp_millis()
                        ))
                        .header(header::COOKIE, "cpr_session=valid-session")
                        .header(header::ACCEPT_LANGUAGE, "en")
                        .header("x-request-id", "req_relative_time")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(status, StatusCode::OK, "{zone}: {value}");
            let account = &value["data"]["accountUsage"][0];
            assert_eq!(
                account["lastUsedRelative"], expected,
                "{zone}: {elapsed_ms:?}"
            );
            assert_eq!(account["lastUsed"], legacy, "{zone}: {elapsed_ms:?}");
            assert_eq!(account["lastUsedAt"], json!(last_used_at));
        }
    }
}
