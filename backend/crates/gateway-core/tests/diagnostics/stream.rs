use gateway_core::diagnostics::{StreamCapture, StreamFormat, TraceContext, body_fingerprint};

#[test]
fn sse_capture_reassembles_every_chunk_boundary_and_preserves_unknown_events() {
    let bytes = "event: future.metadata\r\ndata: {\"headers\":{\"x-request-id\":\"up-1\"},\"unknown\":\"中文\"}\r\n\r\ndata: {\"type\":\"response.completed\"}\n\n".as_bytes();
    for boundary in 0..=bytes.len() {
        let trace = TraceContext::new("req_sse");
        let mut capture = StreamCapture::new(trace.clone(), StreamFormat::Sse);
        capture.push(&bytes[..boundary]);
        capture.push(&bytes[boundary..]);
        capture.finish();
        let snapshot = trace.snapshot().unwrap();
        let events = snapshot["events"].as_array().unwrap();
        assert_eq!(events.len(), 3, "boundary {boundary}");
        // 未知 SSE 名称和伪装成头部的 JSON 仍有摘要，但不能获得协议字段的明文权限。
        assert_eq!(
            events[0]["data"]["eventType"],
            body_fingerprint(b"future.metadata")
        );
        assert!(!snapshot.to_string().contains("up-1"));
        assert!(!snapshot.to_string().contains("中文"));
        assert_eq!(events[1]["data"]["eventType"], "response.completed");
        assert_eq!(events[2]["data"]["partialFrame"], false);
    }
}

#[test]
fn oversize_frames_report_a_gap_and_resume_at_the_next_frame() {
    let trace = TraceContext::new("req_sse");
    let mut capture = StreamCapture::new(trace.clone(), StreamFormat::Sse);
    capture.push(b"data: ");
    capture.push(&vec![b'x'; 130 * 1024]);
    capture.push(b"\n\ndata: {\"type\":\"response.completed\"}\n\n");
    capture.finish();
    let snapshot = trace.snapshot().unwrap();
    let events = snapshot["events"].as_array().unwrap();
    assert_eq!(events[0]["stage"], "capture.gap");
    assert_eq!(events[1]["data"]["eventType"], "response.completed");
}

#[test]
fn dropping_a_partial_stream_is_distinct_from_upstream_eof() {
    let trace = TraceContext::new("req_partial");
    {
        let mut capture = StreamCapture::new(trace.clone(), StreamFormat::Sse);
        capture.push(b"data: {\"type\":");
    }
    let snapshot = trace.snapshot().unwrap();
    assert_eq!(snapshot["events"][0]["stage"], "upstream.reader.dropped");
    assert!(
        snapshot["events"][0]["data"]["partialBytes"]
            .as_u64()
            .unwrap()
            > 0
    );
}

#[test]
fn sse_accepts_bom_and_bare_carriage_return_even_when_split_into_single_bytes() {
    let trace = TraceContext::new("req_cr");
    let mut capture = StreamCapture::new(trace.clone(), StreamFormat::Sse);
    for byte in b"\xef\xbb\xbfdata: {\"type\":\"codex.response.metadata\"}\r\r" {
        capture.push(&[*byte]);
    }
    capture.finish();
    let snapshot = trace.snapshot().unwrap();
    assert_eq!(
        snapshot["events"][0]["data"]["eventType"],
        "codex.response.metadata"
    );
    assert_eq!(snapshot["events"][1]["data"]["partialFrame"], false);
}
