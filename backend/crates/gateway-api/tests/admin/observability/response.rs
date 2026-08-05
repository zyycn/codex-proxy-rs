//! 响应 DTO 与固定 wire 形状的合同测试。

use chrono::{TimeZone, Utc};
use gateway_admin::model::observability::DesktopReleaseStatus;
use gateway_api::admin::PageMeta;
use gateway_api::admin::observability::{
    BillingView, CostCoverageView, CursorWire, DashboardAccountRequestBucketView,
    DashboardAccountUsageView, DashboardDesktopReleaseStatusView, DashboardWireAttributeView,
    DashboardWireProfileView, DashboardWireTargetView, PageData, TokenDetailsView, TrendData,
    TrendKind, TrendPointView, TrendSummaryView,
};
use serde_json::json;

#[test]
fn usage_page_should_keep_terminal_camel_case_shape() {
    let data = PageData {
        items: vec![json!({"id": "request_1"})],
        page: PageMeta::new(1, 50, 1, 1),
        next_cursor: Some("cursor".to_owned()),
    };
    let value = serde_json::to_value(data).unwrap();
    assert_eq!(value["page"]["pageSize"], 50);
    assert_eq!(value["nextCursor"], "cursor");
}

#[test]
fn dashboard_wire_profiles_should_keep_provider_specific_attributes() {
    let value = serde_json::to_value(DashboardWireProfileView {
        provider: "xai".to_owned(),
        product: "Grok Build".to_owned(),
        version: "0.2.106".to_owned(),
        build: None,
        target: DashboardWireTargetView {
            os_type: "linux".to_owned(),
            os_version: "—".to_owned(),
            arch: "x86_64".to_owned(),
            terminal: "headless".to_owned(),
        },
        user_agent: "grok-shell/0.2.106 (linux; x86_64)".to_owned(),
        attributes: vec![DashboardWireAttributeView {
            label: "客户端标识".to_owned(),
            value: "grok-shell".to_owned(),
        }],
        verified_at: None,
        release: None,
    })
    .expect("dashboard profile");

    assert_eq!(value["provider"], "xai");
    assert_eq!(value["version"], "0.2.106");
    assert_eq!(value["attributes"][0]["label"], "客户端标识");
    assert!(value.get("release").is_none());
    assert!(value.get("verifiedAt").is_none());
}

#[test]
fn dashboard_account_usage_should_keep_daily_request_timeline() {
    let bucket_start = Utc.timestamp_opt(0, 0).single().unwrap();
    let value = serde_json::to_value(DashboardAccountUsageView {
        id: "account_1".to_owned(),
        provider: "xai".to_owned(),
        authentication_kind: "oauth".to_owned(),
        email: "account@example.com".to_owned(),
        plan_type: Some("free".to_owned()),
        tokens: "—".to_owned(),
        request_count: 3,
        request_buckets: vec![DashboardAccountRequestBucketView {
            bucket_start,
            request_count: 3,
        }],
        quota_used_percent: None,
        last_used: "刚刚".to_owned(),
    })
    .expect("dashboard account usage");

    assert_eq!(value["requestCount"], 3);
    assert_eq!(
        value["requestBuckets"][0]["bucketStart"],
        "1970-01-01T00:00:00Z"
    );
    assert_eq!(value["requestBuckets"][0]["requestCount"], 3);
}

#[test]
fn trend_wire_should_serialize_kind_and_values_without_store_types() {
    let now = Utc.timestamp_opt(0, 0).single().unwrap();
    let data = TrendData {
        kind: TrendKind::Usage,
        points: vec![TrendPointView {
            time: "08:00".to_owned(),
            bucket: now,
            label: "01-01 08:00".to_owned(),
            requests: "1".to_owned(),
            requests_value: 1,
            input_tokens: "2".to_owned(),
            input_tokens_value: 2,
            output_tokens: "3".to_owned(),
            output_tokens_value: 3,
            cached_tokens: "0".to_owned(),
            cached_tokens_value: 0,
            cache_hit_rate_value: 0.0,
            tokens_value: 5,
            errors: "0".to_owned(),
            errors_value: 0,
            latency: "1 ms".to_owned(),
            latency_value: Some(1),
            max_latency: "1 ms".to_owned(),
            max_latency_value: Some(1),
            min_latency: "1 ms".to_owned(),
            min_latency_value: Some(1),
            success_rate: "100.0%".to_owned(),
            success_rate_value: Some(100.0),
        }],
        summary: vec![TrendSummaryView {
            label: "输入".to_owned(),
            value: "2".to_owned(),
            ratio: None,
        }],
    };
    let value = serde_json::to_value(data).unwrap();
    assert_eq!(value["kind"], "usage");
    assert_eq!(value["points"][0]["requestsValue"], 1);
}

#[test]
fn sensitive_response_views_do_not_require_debug_or_add_secret_fields() {
    let coverage = CostCoverageView {
        known: 1,
        partial: 0,
        unknown: 0,
        not_billable: 0,
    };
    let token_details = TokenDetailsView {
        input_tokens: Some(1),
        output_tokens: Some(2),
        cached_tokens: None,
        cache_write_tokens: None,
        reasoning_tokens: None,
        image_input_tokens: None,
        image_output_tokens: None,
        total_tokens: Some(3),
        input_tokens_display: "1".to_owned(),
        output_tokens_display: "2".to_owned(),
        cached_tokens_display: "-".to_owned(),
        cache_write_tokens_display: "-".to_owned(),
        reasoning_tokens_display: "-".to_owned(),
        image_input_tokens_display: "-".to_owned(),
        image_output_tokens_display: "-".to_owned(),
        total_tokens_display: "3".to_owned(),
    };
    let cursor = CursorWire {
        observed_at: Utc.timestamp_opt(0, 0).single().unwrap(),
        stable_id: "request_1".to_owned(),
    };
    let value = serde_json::to_value((&coverage, &token_details, &cursor)).unwrap();
    assert!(value.to_string().contains("known"));
    assert!(!value.to_string().contains("secret"));
}

#[test]
fn billing_view_should_preserve_the_original_detail_contract() {
    let value = serde_json::to_value(BillingView {
        input_amount_display: "$0.03".to_owned(),
        output_amount_display: "$0.00".to_owned(),
        cache_read_amount_display: "$0.14".to_owned(),
        cache_write_amount_display: "$0.00".to_owned(),
        standard_amount_display: "$0.17".to_owned(),
        total_amount_display: "$0.17".to_owned(),
        input_price_display: "$10.0000 / 1M Token".to_owned(),
        output_price_display: "$60.0000 / 1M Token".to_owned(),
        cache_read_price_display: "$1.0000 / 1M Token".to_owned(),
        cache_write_price_display: "$12.5000 / 1M Token".to_owned(),
        service_tier_display: "Fast".to_owned(),
        multiplier_display: "1.00x".to_owned(),
    })
    .expect("billing view");

    assert_eq!(value["inputAmountDisplay"], "$0.03");
    assert_eq!(value["cacheReadPriceDisplay"], "$1.0000 / 1M Token");
    assert_eq!(value["serviceTierDisplay"], "Fast");
    assert_eq!(value["multiplierDisplay"], "1.00x");
}

#[test]
fn desktop_release_status_should_preserve_the_existing_dashboard_wire_values() {
    for (domain, expected) in [
        (DesktopReleaseStatus::Unchecked, "unchecked"),
        (DesktopReleaseStatus::Current, "aligned"),
        (DesktopReleaseStatus::UpdateAvailable, "review_required"),
        (DesktopReleaseStatus::Failed, "check_failed"),
    ] {
        let status = DashboardDesktopReleaseStatusView::from(domain);
        assert_eq!(serde_json::to_value(status).unwrap(), expected);
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
        AccountPoolMetrics, AttemptMetrics, DashboardObservation, RequestMetrics, TimeRange,
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
        range: TimeRange::new(now - Duration::hours(1), now).expect("dashboard range"),
        requests: RequestMetrics::default(),
        attempts: AttemptMetrics::default(),
        provider_accounts: AccountPoolMetrics {
            total: 807,
            enabled: 804,
            unavailable: 792,
            active: 15,
            rate_limited: 0,
            expired: 2,
            invalid: 0,
            quota_exhausted: 785,
            refreshing: None,
            disabled: 3,
            banned: 2,
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
                .header(header::COOKIE, "cpr_admin_session=valid-session")
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

    assert_eq!(
        (
            &value["data"]["cards"]["credentials"]["totalValue"],
            &value["data"]["cards"]["credentials"]["availableValue"],
            &value["data"]["cards"]["credentials"]["unavailableValue"],
        ),
        (
            &serde_json::json!(807),
            &serde_json::json!(15),
            &serde_json::json!(792)
        ),
    );
}

#[tokio::test]
async fn dashboard_summary_should_default_to_current_china_day() {
    use axum::{
        body::Body,
        http::{Request, StatusCode, header},
    };
    use chrono::{Duration, Utc};
    use gateway_admin::model::observability::{
        AccountPoolMetrics, AttemptMetrics, DashboardObservation, RequestMetrics, TimeRange,
        china_day_start,
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
        range: TimeRange::new(now - Duration::hours(1), now).expect("dashboard range"),
        requests: RequestMetrics::default(),
        attempts: AttemptMetrics::default(),
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
                .header(header::COOKIE, "cpr_admin_session=valid-session")
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
    assert_eq!(range.start, china_day_start(range.end));
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
    fixture
        .usage_detail
        .lock()
        .expect("usage detail")
        .replace(UsageDetail {
            request: usage_record_with_account(
                "req_detail",
                "acct_snap_a",
                "Snapshot Alpha",
                "alpha@example.invalid",
                "oauth",
                now,
            ),
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
                cached_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
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
                .header(header::COOKIE, "cpr_admin_session=valid-session")
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
async fn ops_errors_should_keep_account_label_and_authentication_contract() {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use chrono::Utc;
    use gateway_admin::model::observability::OpsError;
    use gateway_api::admin::observability;
    use tower::ServiceExt as _;

    use crate::admin::{AdminTestFixture, AdminTestState};

    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    fixture
        .ops_errors
        .lock()
        .expect("ops errors")
        .push(OpsError {
            source: "model_request".to_owned(),
            event_id: "err_snapshot".to_owned(),
            request_id: Some("req_err".to_owned()),
            attempt_index: Some(1),
            client_api_key_ref: Some("key_err".to_owned()),
            component: "model_request".to_owned(),
            operation: "responses".to_owned(),
            endpoint: Some("/v1/responses".to_owned()),
            provider_kind: Some("openai".to_owned()),
            provider_account_ref: Some("acct_err".to_owned()),
            provider_account_name: None,
            provider_account_email: Some("err@example.invalid".to_owned()),
            provider_account_authentication_kind: Some("api_key".to_owned()),
            upstream_model_id: Some("upstream-err".to_owned()),
            upstream_transport: Some("http_sse".to_owned()),
            failure_kind: "upstream_error".to_owned(),
            client_status_code: Some(502),
            upstream_status_code: Some(502),
            provider_error_code: Some("upstream".to_owned()),
            client_response_id: None,
            upstream_request_id: None,
            latency_ms: Some(90),
            message: "snapshot error".to_owned(),
            occurrence_count: 1,
            occurred_at: Utc::now(),
            stable_sort_id: "model_request:req_err".to_owned(),
        });
    let response = observability::router::<AdminTestState>()
        .with_state(fixture.state())
        .oneshot(
            Request::builder()
                .uri("/api/admin/operations/errors")
                .header(header::COOKIE, "cpr_admin_session=valid-session")
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
    assert_eq!(
        serde_json::json!({
            "provider": value["data"]["items"][0]["provider"],
            "authenticationKind": value["data"]["items"][0]["authenticationKind"],
            "kind": value["data"]["items"][0]["kind"],
            "accountId": value["data"]["items"][0]["accountId"],
            "accountLabel": value["data"]["items"][0]["metadata"]["accountLabel"],
            "clientStatusCode": value["data"]["items"][0]["clientStatusCode"],
            "route": value["data"]["items"][0]["route"],
        }),
        serde_json::json!({
            "provider": "openai",
            "authenticationKind": "api_key",
            "kind": "model_request",
            "accountId": "acct_err",
            "accountLabel": "err@example.invalid",
            "clientStatusCode": 502,
            "route": "/v1/responses",
        })
    );
}

#[tokio::test]
async fn diagnostics_should_keep_stable_key_and_display_name_contract() {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use gateway_admin::model::observability::{CostCoverage, DiagnosticObservation};
    use gateway_api::admin::observability;
    use tower::ServiceExt as _;

    use crate::admin::{AdminTestFixture, AdminTestState};

    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    fixture
        .diagnostics
        .lock()
        .expect("diagnostics")
        .push(DiagnosticObservation {
            key: "acct_diag".to_owned(),
            name: "diag@example.invalid".to_owned(),
            request_count: 2,
            success_count: 2,
            failure_count: 0,
            attempt_count: 2,
            total_tokens: 200,
            average_latency_ms: Some(100),
            latency_p95_ms: Some(3800),
            cost_coverage: CostCoverage::default(),
            costs: Vec::new(),
        });
    let response = observability::router::<AdminTestState>()
        .with_state(fixture.state())
        .oneshot(
            Request::builder()
                .uri("/api/admin/usage/insights/diagnostics?dimension=account")
                .header(header::COOKIE, "cpr_admin_session=valid-session")
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

fn usage_record_with_account(
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
        protocol: "openai".to_owned(),
        operation: "responses".to_owned(),
        endpoint: "/v1/responses".to_owned(),
        client_transport: "http_sse".to_owned(),
        requested_model_id: "coding".to_owned(),
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
        cached_tokens: Some(0),
        cache_write_tokens: Some(0),
        reasoning_tokens: Some(0),
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
        latency_ms: None,
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
async fn usage_route_should_forward_a_bounded_unknown_outcome_filter() {
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
                .uri("/api/admin/usage/records?outcome=provider_future_state")
                .header(header::COOKIE, "cpr_admin_session=valid-session")
                .header("x-request-id", "req_usage_other_outcome")
                .body(Body::empty())
                .expect("usage request"),
        )
        .await
        .expect("usage response");

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn usage_route_should_expose_image_and_websocket_facts() {
    use std::str::FromStr as _;

    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use chrono::Utc;
    use gateway_admin::model::observability::{
        CalculatedBillingBreakdown, CurrencyCost, DecimalAmount, RequestOutcome, UsageBilling,
        UsageRecord,
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
        .push(UsageRecord {
            id: "request_endpoint".to_owned(),
            client_api_key_ref: "key_endpoint".to_owned(),
            config_revision: 1,
            protocol: "openai".to_owned(),
            operation: "generate".to_owned(),
            endpoint: "/v1/responses".to_owned(),
            client_transport: "http_sse".to_owned(),
            requested_model_id: "grok-4.5".to_owned(),
            provider_kind: Some("xai".to_owned()),
            provider_account_ref: Some("acct_snapshot".to_owned()),
            provider_account_name: Some("Snapshot Alpha".to_owned()),
            provider_account_email: Some("alpha@example.invalid".to_owned()),
            provider_account_authentication_kind: Some("oauth".to_owned()),
            upstream_model_id: Some("grok-4.5".to_owned()),
            upstream_transport: Some("http_sse".to_owned()),
            http_version: Some("h2".to_owned()),
            websocket_pool: Some("reuse".to_owned()),
            service_tier: Some("priority".to_owned()),
            provider_metadata_json: Some(
                serde_json::json!({
                    "effectiveModel": "grok-4.5",
                    "requestSummary": {"inputItemsCount": 2},
                    "transport": "must-not-overwrite-core",
                })
                .to_string(),
            ),
            attempt_count: 1,
            upstream_send_state: "sent".to_owned(),
            downstream_committed_at: None,
            outcome: RequestOutcome::Succeeded,
            client_status_code: Some(200),
            upstream_status_code: Some(200),
            client_response_id: Some("resp_usage_metadata".to_owned()),
            upstream_request_id: Some("upstream_usage_metadata".to_owned()),
            upstream_response_id: None,
            error_kind: None,
            provider_error_code: None,
            error_message: None,
            retry_after_ms: None,
            input_tokens: Some(1),
            output_tokens: Some(1),
            cached_tokens: Some(0),
            cache_write_tokens: Some(0),
            reasoning_tokens: Some(0),
            image_input_tokens: Some(31),
            image_output_tokens: Some(9),
            total_tokens: Some(2),
            cost_source: "unavailable".to_owned(),
            cost_amount: None,
            cost_currency: None,
            billing: Some(UsageBilling::Calculated(Box::new(
                CalculatedBillingBreakdown {
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
                    service_tier: Some("priority".to_owned()),
                    multiplier_percent: 100,
                },
            ))),
            transport_decision_wait_ms: Some(7),
            connect_ms: Some(11),
            headers_ms: Some(13),
            first_event_ms: Some(17),
            first_reasoning_ms: Some(19),
            first_text_ms: Some(23),
            first_token_ms: Some(19),
            provider_processing_ms: Some(29),
            latency_ms: Some(31),
            client_ip: Some("127.0.0.1".to_owned()),
            user_agent: Some("usage-metadata-test".to_owned()),
            reasoning_effort: Some("max".to_owned()),
            reasoning_preset: Some("ultra".to_owned()),
            request_kind: Some("review".to_owned()),
            subagent_kind: Some("worker".to_owned()),
            compact: true,
            image_generation_requested: true,
            image_generation_succeeded: Some(true),
            started_at: Utc::now(),
            deadline_at: Utc::now(),
            completed_at: Some(Utc::now()),
        });
    let response = observability::router::<AdminTestState>()
        .with_state(fixture.state())
        .oneshot(
            Request::builder()
                .uri("/api/admin/usage/records")
                .header(header::COOKIE, "cpr_admin_session=valid-session")
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
        serde_json::json!({
            "route": value["data"]["items"][0]["route"],
            "serviceTier": value["data"]["items"][0]["serviceTier"],
            "accountId": value["data"]["items"][0]["accountId"],
            "accountName": value["data"]["items"][0]["accountName"],
            "accountEmail": value["data"]["items"][0]["accountEmail"],
            "authenticationKind": value["data"]["items"][0]["authenticationKind"],
            "imageInputTokens": value["data"]["items"][0]["tokenDetails"]["imageInputTokens"],
            "imageOutputTokens": value["data"]["items"][0]["tokenDetails"]["imageOutputTokens"],
            "websocketPool": value["data"]["items"][0]["websocketPool"],
            "imageGenerationRequested": value["data"]["items"][0]["imageGenerationRequested"],
            "imageGenerationSucceeded": value["data"]["items"][0]["imageGenerationSucceeded"],
            "requestedModel": value["data"]["items"][0]["requestedModel"],
            "upstreamModel": value["data"]["items"][0]["upstreamModel"],
            "clientIp": value["data"]["items"][0]["clientIp"],
            "userAgent": value["data"]["items"][0]["userAgent"],
            "reasoningEffort": value["data"]["items"][0]["reasoningEffort"],
            "reasoningPreset": value["data"]["items"][0]["reasoningPreset"],
            "requestKind": value["data"]["items"][0]["requestKind"],
            "subagentKind": value["data"]["items"][0]["subagentKind"],
            "compact": value["data"]["items"][0]["compact"],
            "transport": value["data"]["items"][0]["transport"],
            "transportDecisionWaitMs": value["data"]["items"][0]["latencyDetails"]["transportDecisionWaitMs"],
            "wsConnectMs": value["data"]["items"][0]["latencyDetails"]["wsConnectMs"],
            "firstReasoningMs": value["data"]["items"][0]["latencyDetails"]["firstReasoningMs"],
            "firstTextMs": value["data"]["items"][0]["latencyDetails"]["firstTextMs"],
            "firstTokenMs": value["data"]["items"][0]["latencyDetails"]["firstTokenMs"],
            "effectiveModel": value["data"]["items"][0]["metadata"]["effectiveModel"],
            "requestSummary": value["data"]["items"][0]["metadata"]["requestSummary"],
        }),
        serde_json::json!({
            "route": "/v1/responses",
            "serviceTier": "priority",
            "accountId": "acct_snapshot",
            "accountName": "Snapshot Alpha",
            "accountEmail": "alpha@example.invalid",
            "authenticationKind": "oauth",
            "imageInputTokens": 31,
            "imageOutputTokens": 9,
            "websocketPool": {"kind": "reuse"},
            "imageGenerationRequested": true,
            "imageGenerationSucceeded": true,
            "requestedModel": "grok-4.5",
            "upstreamModel": "grok-4.5",
            "clientIp": "127.0.0.1",
            "userAgent": "usage-metadata-test",
            "reasoningEffort": "max",
            "reasoningPreset": "ultra",
            "requestKind": "review",
            "subagentKind": "worker",
            "compact": true,
            "transport": "http_sse",
            "transportDecisionWaitMs": 7,
            "wsConnectMs": 11,
            "firstReasoningMs": 19,
            "firstTextMs": 23,
            "firstTokenMs": 19,
            "effectiveModel": "grok-4.5",
            "requestSummary": {"inputItemsCount": 2},
        })
    );
}
