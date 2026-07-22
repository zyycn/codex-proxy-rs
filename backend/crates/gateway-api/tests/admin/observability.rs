mod query {
    use gateway_api::admin::observability::{
        DashboardQuery, DiagnosticDimension, DiagnosticsQuery, OpsQuery, TrendKind, UsageQuery,
        parse_attempt_index, parse_datetime, parse_status,
    };
    use serde_json::json;

    #[test]
    fn dashboard_query_should_parse_terminal_trend_kinds() {
        let query: DashboardQuery = serde_json::from_value(json!({"kind": "errors"})).unwrap();
        assert_eq!(query.trend_kind().unwrap(), TrendKind::Errors);
    }

    #[test]
    fn dashboard_query_should_reject_unknown_trend_kind() {
        let query: DashboardQuery = serde_json::from_value(json!({"kind": "secret"})).unwrap();
        assert_eq!(query.trend_kind().unwrap_err().field(), "kind");
    }

    #[test]
    fn usage_query_should_bound_page_size_and_cursor() {
        let query: UsageQuery = serde_json::from_value(json!({
            "page": 2,
            "pageSize": 100,
            "cursor": "opaque"
        }))
        .unwrap();
        assert_eq!(query.validate_page().unwrap(), (2, 100));
        assert!(query.validate_cursor().is_ok());
    }

    #[test]
    fn usage_query_should_reject_zero_page() {
        let query: UsageQuery = serde_json::from_value(json!({"page": 0})).unwrap();
        assert_eq!(query.validate_page().unwrap_err().field(), "page");
    }

    #[test]
    fn ops_query_should_reject_page_size_above_terminal_limit() {
        let query: OpsQuery = serde_json::from_value(json!({"pageSize": 101})).unwrap();
        assert_eq!(query.validate_page().unwrap_err().field(), "pageSize");
    }

    #[test]
    fn diagnostics_query_should_keep_wire_dimension_name() {
        let query: DiagnosticsQuery =
            serde_json::from_value(json!({"dimension": "failure_class"})).unwrap();
        assert_eq!(query.dimension().unwrap(), DiagnosticDimension::Failure);
        assert_eq!(DiagnosticDimension::Failure.display_name(), "failureClass");
    }

    #[test]
    fn scalar_query_parsers_should_reject_out_of_range_values_without_echoing_input() {
        assert_eq!(parse_status(Some(99)).unwrap_err().field(), "statusCode");
        assert_eq!(
            parse_attempt_index(Some(0)).unwrap_err().field(),
            "attemptIndex"
        );
        assert_eq!(
            parse_datetime(Some("not-a-time")).unwrap_err().field(),
            "timeRange"
        );
    }
}

mod response {
    use chrono::{TimeZone, Utc};
    use gateway_admin::model::observability::DesktopReleaseStatus;
    use gateway_api::admin::observability::{
        BillingView, CostCoverageView, CursorWire, DashboardDesktopReleaseStatusView,
        DashboardWireAttributeView, DashboardWireProfileView, DashboardWireTargetView, PageData,
        PageMeta, TokenDetailsView, TrendData, TrendKind, TrendPointView, TrendSummaryView,
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
                first_token_latency: "1 ms".to_owned(),
                first_token_latency_value: Some(1),
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
}

#[tokio::test]
async fn usage_route_should_forward_a_bounded_unknown_outcome_filter() {
    use axum::{
        body::Body,
        http::{Request, StatusCode, header},
    };
    use gateway_api::admin::observability;
    use tower::ServiceExt as _;

    use super::{AdminTestFixture, AdminTestState};

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
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use chrono::Utc;
    use gateway_admin::model::observability::{RequestOutcome, UsageRecord};
    use gateway_api::admin::observability;
    use tower::ServiceExt as _;

    use super::{AdminTestFixture, AdminTestState};

    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
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
            input_token_estimate: 1,
            provider_kind: Some("xai".to_owned()),
            provider_account_ref: None,
            provider_account_name: None,
            provider_account_email: None,
            upstream_model_id: Some("grok-4.5".to_owned()),
            upstream_transport: Some("http_sse".to_owned()),
            http_version: Some("h2".to_owned()),
            websocket_pool: Some("reuse".to_owned()),
            attempt_count: 1,
            upstream_send_state: "sent".to_owned(),
            downstream_committed_at: None,
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
            image_input_tokens: Some(31),
            image_output_tokens: Some(9),
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
        serde_json::json!({
            "route": value["data"]["items"][0]["route"],
            "imageInputTokens": value["data"]["items"][0]["tokenDetails"]["imageInputTokens"],
            "imageOutputTokens": value["data"]["items"][0]["tokenDetails"]["imageOutputTokens"],
            "websocketPool": value["data"]["items"][0]["metadata"]["websocketPool"],
            "imageGenerationRequested": value["data"]["items"][0]["metadata"]["imageGenerationRequested"],
            "imageGenerationSucceeded": value["data"]["items"][0]["metadata"]["imageGenerationSucceeded"],
        }),
        serde_json::json!({
            "route": "/v1/responses",
            "imageInputTokens": 31,
            "imageOutputTokens": 9,
            "websocketPool": "reuse",
            "imageGenerationRequested": true,
            "imageGenerationSucceeded": true,
        })
    );
}
