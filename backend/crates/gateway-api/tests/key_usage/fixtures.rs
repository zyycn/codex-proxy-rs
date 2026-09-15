use crate::{admin::AdminTestFixture, support::key_fixture};
use chrono::{Duration, Utc};
use gateway_admin::model::{
    client_keys::ClientKeyRecord,
    observability::{
        AttemptMetrics, CostCoverage, CurrencyCost, DecimalAmount, Granularity, OpsError,
        RequestMetricPoint, RequestMetrics, TimeRange, UsageBilling, UsageListRecord,
        UsageOverview,
    },
};
use gateway_core::{
    engine::budget::{ClientBudgetLimits, ClientBudgetStatus},
    policy::{ClientApiKeyId, RateLimits},
};

pub(super) async fn fixture() -> AdminTestFixture {
    let fixture = key_fixture().await;
    let now = Utc::now();
    *fixture.client_key.lock().unwrap() = Some(ClientKeyRecord {
        id: ClientApiKeyId::new("key-42").unwrap(),
        name: "Development".to_owned(),
        label: Some("private-sentinel".to_owned()),
        prefix: "cpr_live".to_owned(),
        groups: Vec::new(),
        provider_kinds: Vec::new(),
        enabled: true,
        limits: RateLimits::unlimited(),
        budget: ClientBudgetStatus {
            limits: ClientBudgetLimits {
                daily_usd: "1".parse().unwrap(),
                weekly_usd: "5".parse().unwrap(),
            },
            daily_used_usd: "0.640001".parse().unwrap(),
            weekly_used_usd: "2.35".parse().unwrap(),
            daily_resets_at: Some((now + Duration::days(1)).into()),
            weekly_resets_at: Some((now + Duration::days(7)).into()),
        },
        last_used_at: Some(now),
        created_at: now,
        updated_at: now,
    });
    let metrics = RequestMetrics {
        request_count: 2,
        success_count: 1,
        failure_count: 1,
        input_tokens: 1000,
        output_tokens: 100,
        cached_tokens: 900,
        reasoning_tokens: 40,
        total_tokens: 1100,
        ..Default::default()
    };
    let coverage = CostCoverage {
        calculated_count: 1,
        unavailable_count: 1,
        ..Default::default()
    };
    let costs = vec![CurrencyCost {
        currency: "USD".to_owned(),
        amount: "0.123456".parse::<DecimalAmount>().unwrap(),
    }];
    {
        let mut observations = fixture.observations.lock().unwrap();
        observations.summary = Some(UsageOverview {
            range: TimeRange {
                start: now - Duration::hours(1),
                end: now,
            },
            requests: metrics.clone(),
            attempts: AttemptMetrics {
                costs: costs.clone(),
                cost_coverage: coverage.clone(),
                ..Default::default()
            },
            providers: Vec::new(),
        });
        observations.trend = Some(vec![RequestMetricPoint {
            bucket_start: now,
            granularity: Granularity::FifteenMinutes,
            metrics,
            costs,
            cost_coverage: coverage,
        }]);
    }
    fixture.usage_records.lock().unwrap().push(usage_record());
    fixture.ops_errors.lock().unwrap().push(error_record());
    fixture
}

fn usage_record() -> UsageListRecord {
    UsageListRecord {
        id: "req-visible".to_owned(),
        endpoint: "/v1/responses".to_owned(),
        client_transport: "http_sse".to_owned(),
        requested_model_id: Some("coding".to_owned()),
        provider_kind: Some("private-sentinel".to_owned()),
        provider_account_ref: Some("private-sentinel".to_owned()),
        provider_account_name: Some("private-sentinel".to_owned()),
        provider_account_email: Some("private-sentinel".to_owned()),
        provider_account_authentication_kind: Some("private-sentinel".to_owned()),
        upstream_model_id: Some("private-sentinel".to_owned()),
        upstream_transport: Some("websocket".to_owned()),
        service_tier: Some("private-sentinel".to_owned()),
        input_tokens: Some(1000),
        output_tokens: Some(100),
        cached_tokens: Some(900),
        cache_write_tokens: Some(0),
        reasoning_tokens: Some(40),
        image_input_tokens: None,
        image_output_tokens: None,
        total_tokens: Some(1100),
        cost_source: "calculated".to_owned(),
        cost_amount: Some("0.123456".parse::<DecimalAmount>().unwrap()),
        cost_currency: Some("USD".to_owned()),
        billing: Some(UsageBilling::Total {
            source: "provider_reported".to_owned(),
            total: CurrencyCost {
                currency: "USD".to_owned(),
                amount: "0.123456".parse().unwrap(),
            },
        }),
        transport_decision_wait_ms: None,
        connect_ms: None,
        headers_ms: None,
        first_event_ms: Some(100),
        first_reasoning_ms: Some(210),
        first_text_ms: Some(600),
        first_token_ms: Some(210),
        provider_processing_ms: None,
        latency_ms: Some(1250),
        admission_decision_ms: None,
        account_selection_wait_ms: None,
        capacity_used_slots: Some(17),
        capacity_total_slots: Some(50),
        client_ip: Some("192.0.2.42".to_owned()),
        user_agent: Some("key-usage-test/1.0".to_owned()),
        reasoning_effort: Some("xhigh".to_owned()),
        reasoning_preset: Some("private-sentinel".to_owned()),
        subagent_kind: Some("private-sentinel".to_owned()),
        compact: false,
        started_at: Utc::now(),
    }
}

fn error_record() -> OpsError {
    OpsError {
        source: "private-sentinel".to_owned(),
        event_id: "error-visible".to_owned(),
        request_id: Some("private-sentinel".to_owned()),
        attempt_index: None,
        client_api_key_ref: Some("private-sentinel".to_owned()),
        component: "private-sentinel".to_owned(),
        operation: "private-sentinel".to_owned(),
        protocol: Some("private-sentinel".to_owned()),
        client_transport: Some("http_sse".to_owned()),
        requested_model_id: Some("coding".to_owned()),
        service_tier: Some("private-sentinel".to_owned()),
        endpoint: Some("/v1/responses".to_owned()),
        provider_kind: Some("private-sentinel".to_owned()),
        provider_account_ref: Some("private-sentinel".to_owned()),
        provider_account_name: Some("private-sentinel".to_owned()),
        provider_account_email: Some("private-sentinel".to_owned()),
        provider_account_authentication_kind: Some("private-sentinel".to_owned()),
        upstream_model_id: Some("private-sentinel".to_owned()),
        upstream_transport: Some("websocket".to_owned()),
        failure_kind: "private-sentinel".to_owned(),
        upstream_send_state: Some("private-sentinel".to_owned()),
        client_status_code: Some(502),
        upstream_status_code: Some(503),
        provider_error_code: Some("private-sentinel".to_owned()),
        client_response_id: Some("private-sentinel".to_owned()),
        upstream_request_id: Some("private-sentinel".to_owned()),
        latency_ms: Some(420),
        message: "private-sentinel".to_owned(),
        raw_upstream_error: Some("private-sentinel".to_owned()),
        client_ip: Some("192.0.2.42".to_owned()),
        user_agent: Some("key-usage-test/1.0".to_owned()),
        reasoning_effort: Some("xhigh".to_owned()),
        reasoning_preset: Some("private-sentinel".to_owned()),
        request_kind: Some("private-sentinel".to_owned()),
        subagent_kind: Some("private-sentinel".to_owned()),
        compact: None,
        continuation_affinity_hash: Some("private-sentinel".to_owned()),
        continuation_previous_response_id_hash: Some("private-sentinel".to_owned()),
        continuation_unavailable_reason: Some("private-sentinel".to_owned()),
        upstream_connection_id: Some("private-sentinel".to_owned()),
        upstream_connection_exit_reason: Some("private-sentinel".to_owned()),
        upstream_connection_age_ms: None,
        upstream_connection_idle_ms: None,
        recovery_request_id: Some("private-sentinel".to_owned()),
        recovered_at: None,
        recovery_attempt_count: 0,
        recovery_retry_delay_ms: None,
        recovery_total_latency_ms: None,
        occurred_at: Utc::now(),
        stable_sort_id: "private-sentinel".to_owned(),
    }
}
