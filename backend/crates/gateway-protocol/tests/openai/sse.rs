use gateway_protocol::openai::sse::{
    DONE_SSE_FRAME, MAX_SSE_EVENT_BUFFER_BYTES, SseError, SseEvent, SseEventDecoder,
    encode_sse_event, encode_sse_event_with_metadata, parse_sse_events, response_failed_sse_event,
    response_failed_sse_event_with_id, sse_body_has_done, sse_frame_end, sse_frame_is_done,
};

#[test]
fn parser_should_preserve_multiline_data_id_and_retry() {
    let input = concat!(
        ": keep-alive\n",
        "event: response.output_text.delta\n",
        "id: evt_1\n",
        "retry: 250\n",
        "data: {\"delta\":\"hello\"}\n",
        "data: second-line\n\n",
    );

    assert_eq!(
        parse_sse_events(input).expect("valid SSE"),
        vec![SseEvent {
            event: Some("response.output_text.delta".to_owned()),
            data: "{\"delta\":\"hello\"}\nsecond-line".to_owned(),
            id: Some("evt_1".to_owned()),
            retry: Some(250),
        }]
    );
}

#[test]
fn parser_should_ignore_done_control_frame() {
    assert!(
        parse_sse_events(DONE_SSE_FRAME)
            .expect("valid done frame")
            .is_empty()
    );
}

#[test]
fn parser_should_reject_invalid_retry_value() {
    assert_eq!(
        parse_sse_events("retry: later\ndata: hello\n\n"),
        Err(SseError::InvalidRetry("later".to_owned()))
    );
}

#[test]
fn parser_should_convert_non_sse_json_error_to_a_safe_error_event() {
    let events = parse_sse_events(r#"{"detail":"upstream unavailable","secret":"drop"}"#)
        .expect("non-SSE response is representable");
    let data: serde_json::Value =
        serde_json::from_str(&events[0].data).expect("generated event JSON");

    assert_eq!(
        (
            events[0].event.as_deref(),
            data.pointer("/error/code")
                .and_then(serde_json::Value::as_str),
            data.pointer("/error/message")
                .and_then(serde_json::Value::as_str),
            events[0].data.contains("secret"),
        ),
        (
            Some("error"),
            Some("non_sse_response"),
            Some("upstream unavailable"),
            false,
        )
    );
}

#[test]
fn incremental_decoder_should_wait_for_a_complete_frame() {
    let mut decoder = SseEventDecoder::default();

    assert!(
        decoder
            .push(b"event: response.created\ndata: {\"type\":\"response.created\"}")
            .expect("valid partial frame")
            .is_empty()
    );
}

#[test]
fn incremental_decoder_should_decode_across_arbitrary_chunk_boundaries() {
    let mut decoder = SseEventDecoder::default();
    decoder
        .push(b"event: response.created\ndata: {\"type\":\"response.")
        .expect("first chunk");

    let events = decoder
        .push(
            b"created\"}\n\nevent: response.completed\ndata: {\"type\":\"response.completed\"}\n\n",
        )
        .expect("second chunk");

    assert_eq!(
        events
            .iter()
            .map(|event| event.event.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("response.created"), Some("response.completed")]
    );
}

#[test]
fn incremental_decoder_finish_should_decode_unterminated_final_frame() {
    let mut decoder = SseEventDecoder::default();
    decoder
        .push(b"event: response.completed\ndata: {\"type\":\"response.completed\"}")
        .expect("partial final frame");

    assert_eq!(decoder.finish().expect("finish frame").len(), 1);
}

#[test]
fn incremental_decoder_should_reject_invalid_utf8_frame() {
    let mut decoder = SseEventDecoder::default();

    assert!(matches!(
        decoder.push(&[0xff, b'\n', b'\n']),
        Err(SseError::ParseError(_))
    ));
}

#[test]
fn incremental_decoder_should_accept_exact_limit_and_reject_one_byte_over() {
    let mut decoder = SseEventDecoder::default();
    let exact_limit = vec![b'x'; MAX_SSE_EVENT_BUFFER_BYTES];

    assert!(
        decoder
            .push(&exact_limit)
            .expect("exact pending limit remains valid")
            .is_empty()
    );
    assert_eq!(
        decoder.push(b"x"),
        Err(SseError::BufferExceeded {
            max_bytes: MAX_SSE_EVENT_BUFFER_BYTES,
        })
    );
}

#[test]
fn encoder_should_round_trip_multiline_data() {
    let frame = encode_sse_event("response.output_text.delta", "first\nsecond");

    assert_eq!(
        parse_sse_events(&frame).expect("encoded SSE"),
        vec![SseEvent {
            event: Some("response.output_text.delta".to_owned()),
            data: "first\nsecond".to_owned(),
            id: None,
            retry: None,
        }]
    );
}

#[test]
fn encoder_should_round_trip_sse_id_and_retry() {
    let frame = encode_sse_event_with_metadata(
        "response.output_text.delta",
        "first\nsecond",
        Some("evt_42"),
        Some(750),
    );

    assert_eq!(
        parse_sse_events(&frame).expect("encoded SSE"),
        vec![SseEvent {
            event: Some("response.output_text.delta".to_owned()),
            data: "first\nsecond".to_owned(),
            id: Some("evt_42".to_owned()),
            retry: Some(750),
        }]
    );
}

#[test]
fn response_failed_event_should_match_openai_failure_shape() {
    let frame = response_failed_sse_event("server_error", "stream_disconnected", "closed early");
    let events = parse_sse_events(&frame).expect("generated frame");
    let data: serde_json::Value =
        serde_json::from_str(&events[0].data).expect("generated event JSON");

    assert_eq!(
        (
            events[0].event.as_deref(),
            data["type"].as_str(),
            data["response"]["status"].as_str(),
            data["response"]["error"]["type"].as_str(),
            data["response"]["error"]["code"].as_str(),
            data["response"]["error"]["message"].as_str(),
            data["error"] == data["response"]["error"],
            data["response"]["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("resp_proxy_")),
        ),
        (
            Some("response.failed"),
            Some("response.failed"),
            Some("failed"),
            Some("server_error"),
            Some("stream_disconnected"),
            Some("closed early"),
            true,
            true,
        )
    );
}

#[test]
fn response_failed_event_should_preserve_existing_response_id() {
    let frame = response_failed_sse_event_with_id(
        Some("resp_existing"),
        "server_error",
        "upstream_unavailable",
        "unavailable",
    );
    let event = parse_sse_events(&frame)
        .expect("generated frame")
        .into_iter()
        .next()
        .expect("one event");
    let data: serde_json::Value = serde_json::from_str(&event.data).expect("generated event JSON");

    assert_eq!(data["response"]["id"], "resp_existing");
}

#[test]
fn done_frame_detection_should_require_exact_single_data_field() {
    assert_eq!(
        [
            "data: [DONE]\n\n",
            "data: [DONE]\r\n\r\n",
            "event: done\ndata: [DONE]\n\n",
            "data: [DONE]\ndata: extra\n\n",
            "data: [DONE] later\n\n",
        ]
        .map(sse_frame_is_done),
        [true, true, true, false, false]
    );
}

#[test]
fn body_done_detection_should_only_accept_a_terminal_done_frame() {
    assert_eq!(
        [
            "event: response.completed\ndata: {}\n\ndata: [DONE]\n\n",
            "data: [DONE]\r\n\r\n",
            "data: [DONE]\n\nevent: ping\ndata: {}\n\n",
        ]
        .map(sse_body_has_done),
        [true, true, false]
    );
}

#[test]
fn frame_end_should_support_lf_and_crlf_boundaries() {
    assert_eq!(
        [
            sse_frame_end(b"data: one\n\nremaining"),
            sse_frame_end(b"data: two\r\n\r\nremaining"),
            sse_frame_end(b"data: incomplete"),
        ],
        [Some(11), Some(13), None]
    );
}
