//! 验证观察事件保留已知事实、未知值与独立二进制载荷

use gateway_plugin_sdk::{
    SendState,
    call::observation::{
        Event, RequestCompleted, RequestCost, RequestCostSource, RequestCostStatus, RequestFailure,
        RequestMoney, RequestOutcome, RequestTerminal, RequestTimings, RequestUsage,
        WebSocketResponse,
    },
};
use serde_json::json;

#[test]
fn websocket_response_observation_keeps_payload_out_of_json_and_rejects_unknown_fields() {
    let event = WebSocketResponse {
        event_id: "req_observed:websocket:2:7".into(),
        request_id: "req_observed".into(),
        config_revision: 9,
        operation: "generate".into(),
        protocol: "openai".into(),
        provider: "openai".into(),
        attempt_index: 2,
        sequence: 7,
        payload_included: false,
        requested_model: Some("public-model".into()),
        account_id: None,
        event_type: None,
    };
    let encoded = serde_json::to_value(&event).unwrap();
    assert_eq!(encoded["payload_included"], false);
    assert!(encoded.get("account_id").is_none());
    assert!(encoded.get("event_type").is_none());
    assert!(encoded.get("headers").is_none());
    assert!(encoded.get("body").is_none());
    assert_eq!(
        serde_json::from_value::<WebSocketResponse>(encoded).unwrap(),
        event
    );

    assert!(
        serde_json::from_value::<WebSocketResponse>(json!({
            "event_id":"req_observed:websocket:2:7",
            "request_id":"req_observed",
            "config_revision":9,
            "operation":"generate",
            "protocol":"openai",
            "provider":"openai",
            "attempt_index":2,
            "sequence":7,
            "payload_included":false,
            "content_hint":"must not be accepted"
        }))
        .is_err()
    );
}

fn observation() -> RequestCompleted {
    RequestCompleted {
        event_id: "request-42:terminal".into(),
        request_id: "request-42".into(),
        config_revision: 7,
        operation: "generate".into(),
        requested_model: Some("public-model".into()),
        client_key_id: Some("key_fixture".into()),
        account_id: Some("acct_fixture".into()),
        upstream_model: Some("upstream-model".into()),
        response_model: Some("reported-model".into()),
        service_tier: Some("default".into()),
        provider: None,
        completed_at_ms: 1234,
        terminal: RequestTerminal {
            outcome: RequestOutcome::Succeeded,
            send_state: SendState::Sent,
            attempt_count: 1,
            client_status_code: Some(200),
            error_code: None,
        },
        usage: RequestUsage::default(),
    }
}

#[test]
fn terminal_outcomes_have_stable_wire_names() {
    for (outcome, name) in [
        (RequestOutcome::Succeeded, "succeeded"),
        (RequestOutcome::Failed, "failed"),
        (RequestOutcome::Rejected, "rejected"),
        (RequestOutcome::Cancelled, "cancelled"),
        (RequestOutcome::Incomplete, "incomplete"),
    ] {
        let terminal = RequestTerminal {
            outcome,
            send_state: SendState::NotSent,
            attempt_count: 0,
            client_status_code: None,
            error_code: None,
        };
        let wire = serde_json::to_value(&terminal).unwrap();
        assert_eq!(
            wire,
            json!({"outcome": name, "send_state": "not_sent", "attempt_count": 0})
        );
        assert_eq!(
            serde_json::from_value::<RequestTerminal>(wire).unwrap(),
            terminal
        );
    }
}

#[test]
fn observation_exposes_selected_and_reported_facts_without_inventing_values() {
    let mut request = observation();
    let wire = serde_json::to_value(&request).unwrap();
    assert_eq!(wire["client_key_id"], "key_fixture");
    assert_eq!(wire["account_id"], "acct_fixture");
    assert_eq!(wire["upstream_model"], "upstream-model");
    assert_eq!(wire["response_model"], "reported-model");
    assert_eq!(wire["service_tier"], "default");
    request.account_id = None;
    request.upstream_model = None;
    request.response_model = None;
    request.service_tier = None;
    let wire = serde_json::to_value(request).unwrap();
    for field in [
        "account_id",
        "upstream_model",
        "response_model",
        "service_tier",
    ] {
        assert!(wire.get(field).is_none(), "unknown {field} must be omitted");
    }
}

#[test]
fn completed_event_requires_terminal_and_usage_without_inventing_unknown_values() {
    let request = observation();
    let event = Event::RequestCompleted(Box::new(request.clone()));
    let encoded = serde_json::to_value(&event).unwrap();
    assert_eq!(encoded["event"], "request_completed");
    assert_eq!(encoded["data"]["terminal"]["outcome"], "succeeded");
    assert_eq!(encoded["data"]["usage"], json!({}));
    assert_eq!(serde_json::from_value::<Event>(encoded).unwrap(), event);
    for field in ["terminal", "usage"] {
        let mut incomplete = serde_json::to_value(&request).unwrap();
        incomplete.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<RequestCompleted>(incomplete).is_err());
    }
}

#[test]
fn missing_usage_is_not_invented_as_zero_or_derived_totals() {
    let usage: RequestUsage = serde_json::from_value(json!({"input_tokens": 0})).unwrap();
    assert_eq!(usage.input_tokens, Some(0));
    assert_eq!(usage.output_tokens, None);
    assert_eq!(usage.total_tokens, None);
    assert_eq!(
        serde_json::to_value(usage).unwrap(),
        json!({"input_tokens": 0})
    );

    assert!(serde_json::from_value::<RequestUsage>(json!({"output_tokens": -1})).is_err());
}

#[test]
fn observation_contract_rejects_unrecognized_fields_and_outcomes() {
    let mut request = serde_json::to_value(observation()).unwrap();
    request["request_body"] = json!({});
    assert!(serde_json::from_value::<RequestCompleted>(request).is_err());
    assert!(serde_json::from_value::<RequestUsage>(json!({"custom_tokens": 1})).is_err());
    assert!(
        serde_json::from_value::<RequestTerminal>(json!({
            "outcome": "succeeded", "send_state": "sent", "attempt_count": 1,
            "internal_error": "not a public observation field"
        }))
        .is_err()
    );
    assert!(serde_json::from_value::<RequestOutcome>(json!("running")).is_err());
}

#[test]
fn cost_timings_and_failure_keep_unknown_facts_explicit_and_safe() {
    let unknown = RequestCost {
        status: RequestCostStatus::Unknown,
        source: RequestCostSource::Unavailable,
        total: None,
    };
    assert_eq!(
        serde_json::to_value(&unknown).unwrap(),
        json!({"status":"unknown","source":"unavailable"})
    );
    let known = RequestCost {
        status: RequestCostStatus::Known,
        source: RequestCostSource::ProviderReported,
        total: Some(RequestMoney {
            amount: "0.0123".into(),
            currency: "USD".into(),
        }),
    };
    assert_eq!(
        serde_json::from_value::<RequestCost>(serde_json::to_value(&known).unwrap()).unwrap(),
        known
    );

    let timings = RequestTimings {
        first_text_ms: Some(12),
        latency_ms: Some(34),
        ..RequestTimings::default()
    };
    assert_eq!(
        serde_json::to_value(&timings).unwrap(),
        json!({"first_text_ms":12,"latency_ms":34})
    );

    let failure = RequestFailure {
        outcome: RequestOutcome::Failed,
        send_state: SendState::Sent,
        attempt_count: 2,
        client_status_code: Some(502),
        upstream_status_code: Some(503),
        error_code: Some("upstream_unavailable".into()),
        retry_after_ms: Some(1_000),
    };
    let wire = serde_json::to_value(&failure).unwrap();
    assert!(wire.get("message").is_none());
    assert!(wire.get("raw_upstream_error").is_none());
    let mut unsafe_wire = wire.clone();
    unsafe_wire["provider_error_code"] = json!("opaque-upstream-value");
    assert!(serde_json::from_value::<RequestFailure>(unsafe_wire).is_err());
    assert_eq!(
        serde_json::from_value::<RequestFailure>(wire).unwrap(),
        failure
    );

    let usage = RequestUsage {
        cost: Some(known),
        timings: Some(timings),
        failure: Some(failure),
        ..RequestUsage::default()
    };
    let wire = serde_json::to_value(&usage).unwrap();
    assert!(wire.get("input_tokens").is_none());
    assert_eq!(wire["cost"]["total"]["amount"], "0.0123");
    assert_eq!(wire["timings"]["latency_ms"], 34);
    assert!(wire["failure"].get("provider_error_code").is_none());
    assert_eq!(serde_json::from_value::<RequestUsage>(wire).unwrap(), usage);
}
