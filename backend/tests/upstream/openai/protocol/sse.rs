use super::*;

#[test]
fn first_token_detection_should_match_text_delta() {
    let body = include_bytes!("../../../fixtures/responses/http_sse/text_delta_hello.sse");

    assert!(response_body_has_semantic_output(body));
}

#[test]
fn first_token_detection_should_match_reasoning_delta() {
    let body = include_bytes!("../../../fixtures/responses/http_sse/reasoning_delta.sse");

    assert!(response_body_has_semantic_output(body));
}

#[test]
fn first_token_detection_should_match_function_call_arguments_delta() {
    let body =
        include_bytes!("../../../fixtures/responses/http_sse/function_call_arguments_delta.sse");

    assert!(response_body_has_semantic_output(body));
}

#[test]
fn first_token_detection_should_match_type_when_event_name_is_missing() {
    let body = include_bytes!("../../../fixtures/responses/http_sse/data_type_text_delta.sse");

    assert!(response_body_has_semantic_output(body));
}

#[test]
fn first_token_detection_should_ignore_response_created() {
    let body = include_bytes!("../../../fixtures/responses/http_sse/response_created.sse");

    assert!(!response_body_has_semantic_output(body));
}

#[test]
fn response_event_signals_should_not_count_structural_output_item_added() {
    let signals = response_event_signals(
        Some("response.output_item.added"),
        &json!({"item": {"type": "message", "content": []}}),
    );

    assert!(signals.protocol_progress);
    assert!(!signals.semantic_output);
}

#[test]
fn response_event_signals_should_count_text_inside_completed_output_item() {
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
fn response_event_signals_should_count_reasoning_as_semantic_output() {
    let signals = response_event_signals(
        Some("response.reasoning_text.delta"),
        &json!({"delta": "thinking"}),
    );

    assert!(signals.semantic_output);
    assert!(signals.reasoning_output);
    assert!(!signals.text_output);
}

#[test]
fn first_token_detection_should_ignore_metadata_and_rate_limit_events() {
    let body = include_bytes!(
        "../../../fixtures/responses/http_sse/metadata_rate_limits_no_first_output.sse"
    );

    assert!(!response_body_has_semantic_output(body));
}

#[test]
fn first_token_detection_should_ignore_empty_delta() {
    let body = include_bytes!("../../../fixtures/responses/http_sse/empty_text_delta.sse");

    assert!(!response_body_has_semantic_output(body));
}

#[test]
fn first_token_detection_should_ignore_done_frame() {
    let body = include_bytes!("../../../fixtures/responses/http_sse/done_only.sse");

    assert!(!response_body_has_semantic_output(body));
}

#[test]
fn first_token_detection_should_ignore_incomplete_frames() {
    let body = include_bytes!("../../../fixtures/responses/http_sse/incomplete_text_delta.sse");

    assert!(!response_body_has_semantic_output(body));
}

#[test]
fn first_token_detection_should_match_crlf_frames() {
    let body = include_bytes!("../../../fixtures/responses/http_sse/text_delta_crlf.sse");

    assert!(response_body_has_semantic_output(body));
}

#[test]
fn incremental_decoder_should_wait_for_complete_frame() {
    let mut decoder = SseEventDecoder::default();

    let events = decoder
        .push(b"event: response.created\ndata: {\"type\":\"response.created\"}")
        .unwrap();

    assert!(events.is_empty());
}

#[test]
fn incremental_decoder_should_decode_events_across_chunk_boundaries() {
    let mut decoder = SseEventDecoder::default();
    decoder
        .push(b"event: response.created\ndata: {\"type\":\"response.")
        .unwrap();

    let events = decoder
        .push(
            b"created\"}\n\nevent: response.completed\ndata: {\"type\":\"response.completed\"}\n\n",
        )
        .unwrap();

    assert_eq!(events.len(), 2);
}

#[test]
fn incremental_decoder_should_ignore_done_frame() {
    let mut decoder = SseEventDecoder::default();

    let events = decoder.push(b"data: [DONE]\n\n").unwrap();

    assert!(events.is_empty());
}

#[test]
fn incremental_decoder_finish_should_decode_unterminated_final_frame() {
    let mut decoder = SseEventDecoder::default();
    decoder
        .push(b"event: response.completed\ndata: {\"type\":\"response.completed\"}")
        .unwrap();

    let events = decoder.finish().unwrap();

    assert_eq!(events.len(), 1);
}
