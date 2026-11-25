//! 对应生产职责的行为测试

#[test]
fn diagnostic_wire_dump_preserves_binary_bytes_and_numbers_every_fragment() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use gateway_core::diagnostics::TraceContext;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct Writer(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write_all(bytes)?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let output = Arc::new(Mutex::new(Vec::new()));
    let writer = Writer(Arc::clone(&output));
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    let bytes: Vec<u8> = (0..100_000).map(|index| (index % 256) as u8).collect();
    tracing::subscriber::with_default(subscriber, || {
        let trace = TraceContext::new("req_binary_dump");
        trace
            .attempt(3)
            .exchange("websocket")
            .dump("upstream.binary", &bytes);
        trace.headers(
            "client.connection.headers",
            serde_json::json!({}),
            [
                ("authorization", b"Bearer secret".as_slice()),
                ("x-binary", b"\xff\x80".as_slice()),
                ("x-binary", b"\x00".as_slice()),
            ],
        );
        let snapshot = trace.snapshot().unwrap();
        assert_eq!(snapshot["wireFrames"], 2);
        assert!(!snapshot.to_string().contains("Bearer secret"));
    });
    let log = output.lock().unwrap();
    let records: Vec<serde_json::Value> = std::str::from_utf8(&log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .filter(|record: &serde_json::Value| record["target"] == "request_dump")
        .collect();
    assert_eq!(records.len(), 5);
    let mut recovered = Vec::new();
    for (index, record) in records.iter().take(4).enumerate() {
        let fields = &record["fields"];
        assert_eq!(fields["wire_sequence"], 1);
        assert_eq!(fields["attempt_index"], 3);
        assert_eq!(fields["exchange_id"], 1);
        assert_eq!(fields["chunk_index"], index);
        assert_eq!(fields["chunk_count"], 4);
        recovered.extend(
            STANDARD
                .decode(fields["body_base64"].as_str().unwrap())
                .unwrap(),
        );
    }
    assert_eq!(recovered, bytes);
    let header_record = &records[4]["fields"];
    assert_eq!(header_record["wire_sequence"], 2);
    let header_bytes = STANDARD
        .decode(header_record["body_base64"].as_str().unwrap())
        .unwrap();
    let headers: serde_json::Value = serde_json::from_slice(&header_bytes).unwrap();
    for (index, expected) in [b"Bearer secret".as_slice(), b"\xff\x80", b"\x00"]
        .into_iter()
        .enumerate()
    {
        let value = STANDARD
            .decode(headers["headers"][index]["valueBase64"].as_str().unwrap())
            .unwrap();
        assert_eq!(value, expected);
    }
}
