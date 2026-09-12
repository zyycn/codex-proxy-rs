use std::{
    fmt::Write as _,
    sync::{Arc, Mutex},
};

use gateway_core::diagnostics::{TraceContext, body_fingerprint};
use serde_json::json;
use tracing::{
    Event, Metadata, Subscriber,
    field::{Field, Visit},
    span::{Attributes, Id, Record},
};

#[test]
fn bounded_history_preserves_start_failure_and_final_result() {
    let trace = TraceContext::new("req_trace");
    trace.record("request.started", json!({}));
    for index in 0..400 {
        trace.record(
            "test.event",
            json!({"index": index, "detail": "x".repeat(2000)}),
        );
    }
    trace
        .attempt(1)
        .record("attempt.failed", json!({"code": "websocket_close_1000"}));
    trace
        .attempt(2)
        .record("request.finished", json!({"outcome": "succeeded"}));
    let snapshot = trace.snapshot().unwrap();
    let events = snapshot["events"].as_array().unwrap();
    assert_eq!(events[0]["stage"], "request.started");
    assert_eq!(events[events.len() - 2]["attemptIndex"], 1);
    assert_eq!(events.last().unwrap()["attemptIndex"], 2);
    assert!(snapshot["droppedEvents"].as_u64().unwrap() > 0);
    assert!(events.len() <= 128);
    assert!(snapshot.to_string().len() < 64 * 1024);
    let kept: u64 = events
        .iter()
        .map(|event| event["count"].as_u64().unwrap())
        .sum();
    assert_eq!(
        kept + snapshot["droppedEvents"].as_u64().unwrap(),
        snapshot["totalEvents"]
    );
}

#[test]
fn shared_attempts_and_exchanges_coalesce_delta_without_losing_metadata() {
    let trace = TraceContext::new("req_trace");
    let ws = trace.attempt(1).exchange("websocket");
    ws.capture(
        "upstream.event",
        br#"{"type":"codex.response.metadata","headers":{"x-request-id":"up-1"}}"#,
    );
    trace
        .attempt(1)
        .wire_event("openai", Some("codex.response.metadata"));
    for _ in 0..1000 {
        ws.capture(
            "upstream.event",
            br#"{"type":"response.output_text.delta","delta":"secret"}"#,
        );
        trace
            .attempt(1)
            .wire_event("openai", Some("response.output_text.delta"));
    }
    let http = trace.attempt(1).exchange("http_sse");
    http.capture("upstream.event", br#"{"type":"response.completed"}"#);
    let snapshot = trace.snapshot().unwrap();
    let events = snapshot["events"].as_array().unwrap();
    assert_eq!(snapshot["droppedEvents"], 0);
    assert_eq!(
        events
            .iter()
            .find(|e| e["data"]["eventType"] == "response.output_text.delta")
            .unwrap()["count"],
        1000
    );
    assert_eq!(events.last().unwrap()["exchangeId"], 2);
    assert!(
        events
            .iter()
            .any(|e| e["data"]["metadata"]["headers"]["x-request-id"] == "up-1")
    );
    assert!(!snapshot.to_string().contains("secret"));
    assert!(
        events
            .iter()
            .filter(|event| event["stage"] == "provider.event")
            .all(|event| event["data"].get("metadata").is_none())
    );
}

#[test]
fn header_capture_keeps_duplicates_but_excludes_credentials_from_timeline() {
    let trace = TraceContext::new("req_headers");
    trace.headers(
        "upstream.response.headers",
        json!({"status": 101}),
        [
            ("authorization", b"Bearer SECRET".as_slice()),
            ("x-request-id", b"upstream-one".as_slice()),
            ("x-request-id", b"upstream-two".as_slice()),
            ("set-cookie", b"PRIVATE_COOKIE".as_slice()),
        ],
    );
    let snapshot = trace.snapshot().unwrap();
    assert_eq!(
        snapshot["events"][0]["data"]["headers"]["x-request-id"],
        json!(["upstream-one", "upstream-two"])
    );
    assert!(!snapshot.to_string().contains("SECRET"));
    assert!(!snapshot.to_string().contains("PRIVATE_COOKIE"));
}

#[test]
fn disabled_context_does_not_allocate_a_history() {
    let trace = TraceContext::default().attempt(2).exchange("http");
    trace.capture("upstream.event", b"not json");
    assert!(trace.snapshot().is_none());
}

#[test]
fn a_long_successful_retry_keeps_the_earlier_failure_ahead_of_routine_events() {
    let trace = TraceContext::new("req_retry");
    for _ in 0..12 {
        trace.record("setup", json!({}));
    }
    trace
        .attempt(1)
        .record("attempt.failed", json!({"kind": "unavailable"}));
    trace
        .attempt(1)
        .record("retry.decided", json!({"retryable": true}));
    for index in 0..500 {
        trace
            .attempt(2)
            .record("upstream.event", json!({"index": index}));
    }
    trace.record("request.finished", json!({"outcome": "succeeded"}));
    let snapshot = trace.snapshot().unwrap();
    let events = snapshot["events"].as_array().unwrap();
    assert!(
        events
            .iter()
            .any(|event| event["stage"] == "attempt.failed" && event["attemptIndex"] == 1)
    );
    assert!(events.iter().any(|event| event["stage"] == "retry.decided"));
    assert_eq!(events.last().unwrap()["stage"], "request.finished");
    assert!(events.len() <= 128);
}

#[test]
fn only_top_level_headers_in_controlled_upstream_frames_keep_correlation_values() {
    for event_type in ["response.metadata", "codex.response.metadata", "error"] {
        let trace = TraceContext::new("req_headers");
        let body = serde_json::to_vec(&json!({
            "type": event_type,
            "status": 429,
            "headers": {
                "x-request-id": "upstream-controlled",
                "x-oai-request-id": ["upstream-one", "upstream-two"],
                "authorization": "PRIVATE_CREDENTIAL",
                "PRIVATE_HEADER_KEY": "PRIVATE_HEADER_VALUE",
                "cf-ray": [{"x-request-id": "PRIVATE_MALFORMED_HEADER"}],
                "request-id": [["PRIVATE_NESTED_ARRAY"]],
            },
            "metadata": {
                "x-request-id": "PRIVATE_METADATA_VALUE",
                "headers": {"x-request-id": "PRIVATE_NESTED_VALUE"},
            },
            "response": {
                "type": "codex.response.metadata",
                "headers": {"x-request-id": "PRIVATE_RESPONSE_HEADER"},
            },
        }))
        .unwrap();
        trace.capture("upstream.event", &body);
        let snapshot = trace.snapshot().unwrap();
        let metadata = &snapshot["events"][0]["data"]["metadata"];
        assert_eq!(metadata["headers"]["x-request-id"], "upstream-controlled");
        assert_eq!(
            metadata["headers"]["x-oai-request-id"]["sample"],
            json!(["upstream-one", "upstream-two"])
        );
        assert_eq!(metadata["status"], 429);
        assert!(!snapshot.to_string().contains("PRIVATE_"), "{snapshot}");
    }
}

#[test]
fn body_capture_never_infers_controlled_headers_from_client_supplied_type() {
    for stage in [
        "client.request.body",
        "upstream.request.body",
        "upstream.response.body",
        "upstream.error.body",
    ] {
        let trace = TraceContext::new("req_untrusted");
        trace.capture(
            stage,
            br#"{"type":"codex.response.metadata","headers":{"x-request-id":"PRIVATE_HEADER"}}"#,
        );
        assert!(
            !trace.snapshot().unwrap().to_string().contains("PRIVATE_"),
            "{stage}"
        );
    }
    let trace = TraceContext::new("req_malformed");
    trace.capture(
        "upstream.event",
        br#"{"type":"codex.response.metadata","headers":[{"x-request-id":"PRIVATE_HEADER"}]}"#,
    );
    assert!(!trace.snapshot().unwrap().to_string().contains("PRIVATE_"));
}

#[test]
fn unknown_event_names_are_fingerprinted_at_all_trace_entry_points() {
    let trace = TraceContext::new("req_names");
    trace.capture("upstream.event", br#"{"type":"PRIVATE_EVENT.delta"}"#);
    trace.capture_named_event("upstream.event", b"{}", Some("PRIVATE_SSE_EVENT"));
    trace.wire_event("openai", Some("PRIVATE_WIRE_EVENT"));
    let snapshot = trace.snapshot().unwrap();
    for (event, name) in snapshot["events"].as_array().unwrap().iter().zip([
        "PRIVATE_EVENT.delta",
        "PRIVATE_SSE_EVENT",
        "PRIVATE_WIRE_EVENT",
    ]) {
        assert_eq!(
            event["data"]["eventType"],
            body_fingerprint(name.as_bytes())
        );
    }
    assert!(!snapshot.to_string().contains("PRIVATE_"));
}

#[test]
fn unknown_deltas_coalesce_by_summary_without_merging_distinct_event_types() {
    let trace = TraceContext::new("req_unknown_delta");
    for name in ["PRIVATE_FIRST.delta", "PRIVATE_SECOND.delta"] {
        let bytes = serde_json::to_vec(&json!({"type": name, "delta": "PRIVATE_TEXT"})).unwrap();
        for _ in 0..100 {
            trace.capture("upstream.event", &bytes);
            trace.wire_event("openai", Some(name));
        }
    }
    let snapshot = trace.snapshot().unwrap();
    let events = snapshot["events"].as_array().unwrap();
    assert_eq!(events.len(), 4);
    assert!(events.iter().all(|event| event["count"] == 100));
    assert_eq!(snapshot["totalEvents"], 400);
    assert_eq!(snapshot["droppedEvents"], 0);
    assert_ne!(
        events[0]["data"]["eventType"],
        events[2]["data"]["eventType"]
    );
    assert!(!snapshot.to_string().contains("PRIVATE_"));
}

#[test]
fn size_gaps_and_truncated_events_do_not_reintroduce_user_content() {
    let trace = TraceContext::new("req_limits");
    let private = "PRIVATE_OVERSIZE".repeat(10_000);
    trace.capture("client.request.body", private.as_bytes());
    let fields: serde_json::Map<_, _> = (0..48)
        .map(|i| (format!("PRIVATE_FIELD_{i}"), json!("PRIVATE_VALUE")))
        .collect();
    let bytes = serde_json::to_vec(&fields).unwrap();
    trace.capture_named_event("upstream.event", &bytes, Some("PRIVATE_EVENT"));
    let snapshot = trace.snapshot().unwrap();
    assert_eq!(snapshot["events"][0]["stage"], "capture.gap");
    assert_eq!(
        snapshot["events"][0]["data"]["body"],
        body_fingerprint(private.as_bytes())
    );
    assert_eq!(snapshot["events"][1]["data"]["truncated"], true);
    assert_eq!(
        snapshot["events"][1]["data"]["eventType"],
        body_fingerprint(b"PRIVATE_EVENT")
    );
    assert!(!snapshot.to_string().contains("PRIVATE_"));
}

#[test]
fn ordinary_trace_logs_and_serialized_snapshots_share_the_sanitized_capture_boundary() {
    // tracing 的 callsite 缓存是进程级的；隔离并行测试的注册，不修改生产日志或全局 subscriber。
    const CHILD: &str = "GATEWAY_CORE_DIAGNOSTICS_LOG_TEST";
    if std::env::var_os(CHILD).is_none() {
        let thread = std::thread::current();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", thread.name().unwrap()])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "isolated log regression failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    // 仅捕获本线程的普通 trace 事件，不打开 request_dump，也不读写真实日志文件。
    let logs = Arc::new(Mutex::new(Vec::new()));
    let snapshot = tracing::subscriber::with_default(TraceLog(Arc::clone(&logs)), || {
        let trace = TraceContext::new("req_feedback");
        let exchange = trace.attempt(2).exchange("websocket");
        exchange.capture(
            "client.request.body",
            br#"{"type":"PRIVATE_EVENT","metadata":{"PRIVATE_KEY":"PRIVATE_VALUE","x-request-id":"PRIVATE_HEADER"},"input":[{"role":"user","content":[{"type":"input_text","text":"PRIVATE_PROMPT"}]}]}"#,
        );
        exchange.capture_named_event("upstream.event", b"{}", Some("PRIVATE_SSE_EVENT"));
        exchange.wire_event("openai", Some("PRIVATE_WIRE_EVENT"));
        exchange.headers(
            "upstream.response.headers",
            json!({"status": 429}),
            [
                ("x-request-id", b"upstream-controlled".as_slice()),
                ("authorization", b"PRIVATE_CREDENTIAL".as_slice()),
                ("set-cookie", b"PRIVATE_COOKIE".as_slice()),
            ],
        );
        let snapshot = trace.snapshot().unwrap();
        assert_eq!(snapshot["wireDumpEnabled"], false);
        assert_eq!(snapshot["wireFrames"], 0);
        snapshot
    });
    // API / 反馈导出使用的是这个 Value 的序列化结果，而不只是 Debug 表示。
    let exported = String::from_utf8(serde_json::to_vec(&snapshot).unwrap()).unwrap();
    assert!(!exported.contains("PRIVATE_"), "{exported}");
    let logs = logs.lock().unwrap();
    let output = logs.join("\n");
    assert!(!logs.is_empty(), "必须真实捕获 tracing 事件，不能空集通过");
    assert!(!output.contains("PRIVATE_"), "{output}");
    for fact in [
        "req_feedback",
        "upstream-controlled",
        "upstream.response.headers",
    ] {
        assert!(exported.contains(fact), "{exported}");
        assert!(output.contains(fact), "{output}");
    }
    assert!(output.contains("attempt_index=2"));
    assert!(output.contains("exchange_id=1"));
    assert!(output.contains("request.trace.closed"));
}

struct TraceLog(Arc<Mutex<Vec<String>>>);

impl Subscriber for TraceLog {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.target() == "request_trace"
    }

    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut text = String::new();
        event.record(&mut LogFields(&mut text));
        self.0.lock().unwrap().push(text);
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

struct LogFields<'a>(&'a mut String);

impl Visit for LogFields<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        write!(self.0, "{}={value:?} ", field.name()).unwrap();
    }
}
