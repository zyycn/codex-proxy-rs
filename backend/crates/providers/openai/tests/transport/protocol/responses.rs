use gateway_protocol::openai::sse::parse_sse_events;
use provider_openai::transport::protocol::responses::{
    CodexResponsesRequest, PreviousResponseScope, ResponsesSseFailure, TransportRequirement,
    response_event_signals, transport_requirement,
};
use serde_json::{Value, json};

use super::super::codex_request;

fn response_body_has_semantic_output(body_bytes: &[u8]) -> bool {
    let body = String::from_utf8_lossy(body_bytes);
    let lf_end = body.rfind("\n\n").map(|index| index + 2);
    let crlf_end = body.rfind("\r\n\r\n").map(|index| index + 4);
    let Some(end) = lf_end.into_iter().chain(crlf_end).max() else {
        return false;
    };
    let Ok(events) = parse_sse_events(&body[..end]) else {
        return false;
    };
    events.iter().any(|event| {
        let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
            return false;
        };
        let event_type = event.event.as_deref().or_else(|| {
            value
                .get("type")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
        });
        response_event_signals(event_type, &value).semantic_output
    })
}

fn response_body_has_output_start(body_bytes: &[u8]) -> bool {
    let body = String::from_utf8_lossy(body_bytes);
    let lf_end = body.rfind("\n\n").map(|index| index + 2);
    let crlf_end = body.rfind("\r\n\r\n").map(|index| index + 4);
    let Some(end) = lf_end.into_iter().chain(crlf_end).max() else {
        return false;
    };
    let Ok(events) = parse_sse_events(&body[..end]) else {
        return false;
    };
    events.iter().any(|event| {
        let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
            return false;
        };
        let event_type = event.event.as_deref().or_else(|| {
            value
                .get("type")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
        });
        response_event_signals(event_type, &value).output_start
    })
}

#[test]
fn semantic_output_should_detect_a_text_delta() {
    let body = b"event: response.output_text.delta\ndata: {\"delta\":\"hello\"}\n\n";

    assert!(response_body_has_semantic_output(body));
}

#[test]
fn semantic_output_should_detect_a_reasoning_delta() {
    let body = b"event: response.reasoning_text.delta\ndata: {\"delta\":\"thinking\"}\n\n";

    assert!(response_body_has_semantic_output(body));
}

#[test]
fn semantic_output_should_detect_function_arguments() {
    let body =
        b"event: response.function_call_arguments.delta\ndata: {\"delta\":\"{\\\"q\\\":1}\"}\n\n";

    assert!(response_body_has_semantic_output(body));
}

#[test]
fn semantic_output_should_use_the_json_type_when_the_event_field_is_absent() {
    let body = b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n";

    assert!(response_body_has_semantic_output(body));
}

#[test]
fn semantic_output_should_ignore_response_created() {
    let body =
        b"event: response.created\ndata: {\"type\":\"response.created\",\"response\":{}}\n\n";

    assert!(!response_body_has_semantic_output(body));
}

#[test]
fn output_start_should_ignore_preamble_and_failure_frames() {
    for body in [
        b"event: response.created\ndata: {\"type\":\"response.created\",\"response\":{}}\n\n".as_slice(),
        b"event: response.in_progress\ndata: {\"type\":\"response.in_progress\",\"response\":{}}\n\n",
        b"event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\"}}\n\n",
        b"data: {\"type\":\"error\"}\n\n",
    ] {
        assert!(
            !response_body_has_output_start(body),
            "frame must not start output: {body:?}"
        );
    }
}

#[test]
fn output_start_should_count_structural_and_semantic_frames() {
    for body in [
        // 结构帧无内容也算。
        b"event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"message\"}}\n\n".as_slice(),
        b"event: response.content_part.added\ndata: {\"type\":\"response.content_part.added\",\"part\":{\"type\":\"output_text\"}}\n\n",
        b"event: response.output_text.delta\ndata: {\"delta\":\"hello\"}\n\n",
        b"event: response.reasoning_text.delta\ndata: {\"delta\":\"thinking\"}\n\n",
    ] {
        assert!(
            response_body_has_output_start(body),
            "frame must start output: {body:?}"
        );
    }
}

#[test]
fn response_signals_should_not_count_structural_output_item_added() {
    let signals = response_event_signals(
        Some("response.output_item.added"),
        &json!({"item": {"type": "message", "content": []}}),
    );

    assert!(signals.protocol_progress);
    assert!(!signals.semantic_output);
}

#[test]
fn response_signals_should_count_text_inside_a_completed_output_item() {
    let signals = response_event_signals(
        Some("response.output_item.done"),
        &json!({
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "hello"}]
            }
        }),
    );

    assert!(signals.semantic_output);
    assert!(signals.text_output);
    assert!(!signals.reasoning_output);
}

#[test]
fn response_signals_should_classify_reasoning_as_semantic_output() {
    let signals = response_event_signals(
        Some("response.reasoning_text.delta"),
        &json!({"delta": "thinking"}),
    );

    assert!(signals.semantic_output);
    assert!(signals.reasoning_output);
    assert!(!signals.text_output);
}

#[test]
fn response_signals_should_classify_refusal_as_visible_text() {
    let signals = response_event_signals(
        Some("response.refusal.delta"),
        &json!({"delta": "I cannot help with that"}),
    );

    assert!(signals.semantic_output);
    assert!(signals.text_output);
    assert!(!signals.reasoning_output);
}

#[test]
fn response_signals_should_count_a_future_nonempty_delta_without_a_name_table_entry() {
    let signals = response_event_signals(
        Some("response.future_tool_payload.delta"),
        &json!({"delta": {"chunk": 1}}),
    );

    assert!(signals.semantic_output);
    assert!(!signals.text_output);
    assert!(!signals.reasoning_output);
}

#[test]
fn response_signals_should_count_a_completed_hosted_tool_item() {
    let signals = response_event_signals(
        Some("response.output_item.done"),
        &json!({"item": {"type": "mcp_call", "status": "completed"}}),
    );

    assert!(signals.semantic_output);
}

#[test]
fn response_signals_should_count_a_hosted_tool_execution_phase() {
    let signals = response_event_signals(
        Some("response.code_interpreter_call.in_progress"),
        &json!({"type": "response.code_interpreter_call.in_progress"}),
    );

    assert!(signals.semantic_output);
}

#[test]
fn semantic_output_should_ignore_metadata_and_rate_limit_events() {
    let body = concat!(
        "event: response.metadata\n",
        "data: {\"type\":\"response.metadata\"}\n\n",
        "event: codex.rate_limits\n",
        "data: {\"type\":\"codex.rate_limits\"}\n\n",
    );

    assert!(!response_body_has_semantic_output(body.as_bytes()));
}

#[test]
fn semantic_output_should_ignore_an_empty_delta() {
    let body = b"event: response.output_text.delta\ndata: {\"delta\":\"\"}\n\n";

    assert!(!response_body_has_semantic_output(body));
}

#[test]
fn structured_failure_should_preserve_empty_opaque_fields() {
    let failure = ResponsesSseFailure::from_event(
        "response.failed",
        &json!({
            "response": {
                "error": {"message": "", "code": "", "type": ""}
            }
        }),
    );

    assert_eq!(failure.message, "");
    assert_eq!(failure.upstream_code.as_deref(), Some(""));
    assert_eq!(failure.upstream_type.as_deref(), Some(""));
}

#[test]
fn semantic_output_should_ignore_the_done_control_frame() {
    assert!(!response_body_has_semantic_output(b"data: [DONE]\n\n"));
}

#[test]
fn semantic_output_should_ignore_an_incomplete_frame() {
    let body = b"event: response.output_text.delta\ndata: {\"delta\":\"hello\"}";

    assert!(!response_body_has_semantic_output(body));
}

#[test]
fn semantic_output_should_accept_crlf_frame_boundaries() {
    let body = b"event: response.output_text.delta\r\ndata: {\"delta\":\"hello\"}\r\n\r\n";

    assert!(response_body_has_semantic_output(body));
}

#[test]
fn transport_should_prefer_websocket_when_requested_without_history() {
    let mut request = codex_request("gpt-test", "be brief", Vec::new());
    request.use_websocket = true;

    assert_eq!(
        transport_requirement(&request),
        TransportRequirement::NewChain
    );
    assert!(transport_requirement(&request).allows_pre_send_http_fallback());
}

#[test]
fn unspecified_transport_without_history_should_still_be_a_new_chain() {
    let request = codex_request("gpt-test", "be brief", Vec::new());

    assert_eq!(
        transport_requirement(&request),
        TransportRequirement::NewChain
    );
}

#[test]
fn new_chain_should_allow_pre_delivery_http_fallback() {
    let mut request = codex_request("gpt-test", "be brief", Vec::new());
    request.use_websocket = true;

    assert!(
        transport_requirement(&request).allows_pre_delivery_http_fallback(),
        "new chains may follow the TS same-account fallback behavior"
    );
}

#[test]
fn sticky_http_fallback_should_only_apply_to_new_chains() {
    assert!(TransportRequirement::NewChain.allows_sticky_http_fallback());

    for requirement in [
        TransportRequirement::HttpRequired,
        TransportRequirement::ExplicitWebSocketWarmup,
        TransportRequirement::ExactWebSocketContinuation,
        TransportRequirement::PersistedContinuation,
        TransportRequirement::ExternalUnknown,
    ] {
        assert!(
            !requirement.allows_sticky_http_fallback(),
            "sticky HTTP must not override {}",
            requirement.as_str()
        );
    }
}

#[test]
fn transport_should_mark_unknown_previous_response_as_external() {
    let mut request = codex_request("gpt-test", "be brief", Vec::new());
    request.set_previous_response_id(Some("resp_previous".to_owned()));

    assert_eq!(
        transport_requirement(&request),
        TransportRequirement::ExternalUnknown
    );
    assert!(transport_requirement(&request).allows_pre_send_http_fallback());
}

#[test]
fn previous_response_should_forbid_pre_delivery_http_fallback() {
    let mut request = codex_request("gpt-test", "be brief", Vec::new());
    request.set_previous_response_id(Some("resp_previous".to_owned()));

    assert!(
        !transport_requirement(&request).allows_pre_delivery_http_fallback(),
        "a sent continuation must not be replayed on another transport"
    );
}

#[test]
fn transport_should_allow_forced_http_sse() {
    let mut request = codex_request("gpt-test", "be brief", Vec::new());
    request.set_previous_response_id(Some("resp_previous".to_owned()));
    request.use_websocket = true;
    request.force_http_sse = true;

    assert_eq!(
        transport_requirement(&request),
        TransportRequirement::HttpRequired
    );
}

#[test]
fn transport_should_require_exact_websocket_for_connection_local_continuation() {
    let mut request = codex_request("gpt-test", "be brief", Vec::new());
    request.set_previous_response_id(Some("resp_previous".to_owned()));
    request.previous_response_scope = Some(PreviousResponseScope::ConnectionLocal);

    assert_eq!(
        transport_requirement(&request),
        TransportRequirement::ExactWebSocketContinuation
    );
    assert!(transport_requirement(&request).requires_websocket());
}

#[test]
fn exact_continuation_should_remain_stricter_than_forced_websocket() {
    let mut preferred = codex_request("gpt-test", "be brief", Vec::new());
    preferred.use_websocket = true;
    assert_eq!(
        transport_requirement(&preferred),
        TransportRequirement::NewChain
    );
    assert!(!transport_requirement(&preferred).requires_websocket());

    preferred.set_previous_response_id(Some("resp_previous".to_owned()));
    preferred.previous_response_scope = Some(PreviousResponseScope::ConnectionLocal);
    assert_eq!(
        transport_requirement(&preferred),
        TransportRequirement::ExactWebSocketContinuation
    );
    assert!(transport_requirement(&preferred).requires_websocket());
}

#[test]
fn transport_should_require_websocket_for_store_false_warmup() {
    let mut body = serde_json::Map::new();
    body.insert("generate".to_owned(), json!(false));
    body.insert("store".to_owned(), json!(false));
    let request = CodexResponsesRequest::from_body(body);

    assert_eq!(
        transport_requirement(&request),
        TransportRequirement::ExplicitWebSocketWarmup
    );
    assert!(transport_requirement(&request).requires_websocket());
}
