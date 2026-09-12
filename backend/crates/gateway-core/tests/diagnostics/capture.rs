use gateway_core::diagnostics::{body_fingerprint, diagnostic_headers, diagnostic_json};
use serde_json::{Value, json};

#[test]
fn credentials_and_content_never_enter_diagnostics_but_trace_ids_survive() {
    let headers = diagnostic_headers([
        ("Authorization", "Bearer never-persist-this"),
        ("Set-Cookie", "session=never-persist-this"),
        ("X-OAI-Request-ID", "upstream-123"),
        ("x-codex-turn-state", "opaque-never-persist-this"),
        ("cf-ray", "ray-one"),
        ("cf-ray", "ray-two"),
    ]);
    assert_eq!(headers["x-oai-request-id"][0], "upstream-123");
    assert_eq!(headers["cf-ray"].as_array().unwrap().len(), 2);
    assert!(!headers.to_string().contains("never-persist-this"));
    let content = diagnostic_json(&json!({
        "type": "codex.response.metadata", "headers": headers,
        "access_token": "never-persist-this", "refresh_token": "never-persist-this",
        "response": {"output": [{"text": "private model output"}]},
        "input": "private user prompt", "unknown": "private extension",
    }));
    assert_eq!(content["type"], "codex.response.metadata");
    let text = content.to_string();
    for secret in [
        "never-persist-this",
        "private model output",
        "private user prompt",
        "private extension",
    ] {
        assert!(!text.contains(secret), "leaked {secret}");
    }
}

#[test]
fn arbitrary_event_type_text_is_fingerprinted_instead_of_logged() {
    let trace = gateway_core::diagnostics::TraceContext::new("req_type");
    trace.capture(
        "upstream.event",
        br#"{"type":"secret@example.com","message":"private-content"}"#,
    );
    let snapshot = trace.snapshot().unwrap();
    assert_eq!(
        snapshot["events"][0]["data"]["eventType"],
        body_fingerprint(b"secret@example.com")
    );
    assert!(!snapshot.to_string().contains("secret@example.com"));
    assert!(!snapshot.to_string().contains("private-content"));
}

#[test]
fn user_map_keys_and_header_named_values_are_not_diagnostic_facts() {
    let content = diagnostic_json(&json!({
        "type": "response.create",
        "metadata": {
            "PRIVATE_OWNER": "PRIVATE_VALUE",
            "x-request-id": "PRIVATE_NESTED_REQUEST",
            "headers": {"x-oai-request-id": "PRIVATE_NESTED_HEADER"},
            "type": "PRIVATE_TYPE",
            "model": "PRIVATE_MODEL",
            "account": 1234567890123_u64,
        },
        "input": [{
            "role": "user",
            "content": [{"type": "input_text", "text": "PRIVATE_PROMPT"}],
            "PRIVATE_ITEM_KEY": [{"cf-ray": "PRIVATE_ARRAY_HEADER"}],
        }],
    }));
    assert!(
        !content.to_string().contains("PRIVATE_"),
        "user keys and values must be summarized: {content}"
    );
    assert!(!content.to_string().contains("1234567890123"));
    assert_eq!(content["type"], "response.create");
    assert_eq!(content["input"]["length"], 1);
    assert_eq!(content["input"]["sample"][0]["role"], "user");
    assert_eq!(
        content["input"]["sample"][0]["content"]["sample"][0]["type"],
        "input_text"
    );
}

#[test]
fn json_headers_never_gain_trust_from_their_shape_or_event_type() {
    for value in [
        json!({"headers": {"x-request-id": "PRIVATE_ROOT_HEADER"}}),
        json!({
            "type": "codex.response.metadata",
            "headers": {"x-request-id": "PRIVATE_ROOT_HEADER"},
        }),
        json!([{"x-request-id": "PRIVATE_ARRAY_HEADER"}]),
    ] {
        let content = diagnostic_json(&value);
        assert!(!content.to_string().contains("PRIVATE_"), "{content}");
    }
}

#[test]
fn protocol_shaped_strings_are_not_a_safety_guarantee() {
    let content = diagnostic_json(&json!({
        "type": "PRIVATE_TYPE",
        "code": "PRIVATE_CODE",
        "model": "PRIVATE_MODEL",
        "role": "PRIVATE_ROLE",
        "status": "PRIVATE_STATUS",
        "object": "PRIVATE_OBJECT",
        "service_tier": "PRIVATE_TIER",
    }));
    assert!(!content.to_string().contains("PRIVATE_"), "{content}");
    assert_eq!(content["type"], body_fingerprint(b"PRIVATE_TYPE"));
}

#[test]
fn opaque_maps_do_not_inherit_even_known_protocol_keys_or_values() {
    let content = diagnostic_json(&json!({
        "status": 429,
        "metadata": {
            "type": "response.completed",
            "role": "user",
            "status": 429,
            "Authorization_PRIVATE_OWNER": "PRIVATE_CREDENTIAL",
            "私人标记@example.test": "PRIVATE_VALUE",
        },
    }));
    assert_eq!(content["status"], 429);
    let metadata = content["metadata"].as_object().unwrap();
    for value in ["response.completed", "user", "429"] {
        assert!(
            metadata
                .values()
                .any(|v| *v == body_fingerprint(value.as_bytes()))
        );
    }
    let text = content.to_string();
    for original in ["PRIVATE_", "私人标记", "response.completed"] {
        assert!(!text.contains(original), "{text}");
    }
}

#[test]
fn diagnostic_capture_keeps_bounded_samples_of_unknown_keys_without_collisions() {
    let prefix = "PRIVATE_".repeat(16);
    let value = json!({
        format!("{prefix}one"): "PRIVATE_ONE",
        format!("{prefix}two"): "PRIVATE_TWO",
    });
    let content = diagnostic_json(&value);
    let entries = content.as_object().unwrap();
    assert_eq!(entries.len(), 2);
    assert!(
        entries
            .values()
            .any(|v| *v == body_fingerprint(b"PRIVATE_ONE"))
    );
    assert!(
        entries
            .values()
            .any(|v| *v == body_fingerprint(b"PRIVATE_TWO"))
    );
    assert!(!content.to_string().contains("PRIVATE_"));
}

#[test]
fn diagnostic_capture_bounds_fields_arrays_depth_and_total_nodes() {
    let fields: serde_json::Map<_, _> = (0..100)
        .map(|i| (format!("PRIVATE_FIELD_{i}"), json!("PRIVATE_VALUE")))
        .collect();
    let content = diagnostic_json(&Value::Object(fields));
    assert_eq!(content.as_object().unwrap().len(), 49);
    assert_eq!(content["_omittedFields"], 52);

    let array = diagnostic_json(&json!(vec!["PRIVATE_VALUE"; 8]));
    assert_eq!(array["length"], 8);
    assert_eq!(array["sample"].as_array().unwrap().len(), 4);

    let mut deep = json!("PRIVATE_DEEP");
    for _ in 0..12 {
        deep = json!({"PRIVATE_DEPTH": deep});
    }
    let content = diagnostic_json(&deep);
    assert!(content.to_string().contains("\"omitted\":true"));
    assert!(!content.to_string().contains("PRIVATE_"));

    let wide: serde_json::Map<_, _> = (0..48)
        .map(|i| (format!("PRIVATE_NODE_{i}"), json!(vec!["PRIVATE_VALUE"; 4])))
        .collect();
    let content = diagnostic_json(&Value::Object(wide));
    assert!(content["_omittedFields"].as_u64().unwrap() > 0);
    assert!(content.to_string().len() < 32 * 1024);
    assert!(!content.to_string().contains("PRIVATE_"));
}

#[test]
fn credentials_remain_redacted_at_nested_json_and_real_header_boundaries() {
    for name in [
        "Authorization",
        "Cookie",
        "Set-Cookie",
        "api_key",
        "api-key",
        "access_token",
        "refresh_token",
        "password",
        "client_secret",
        "attestation",
        "x-oai-is",
        "x-oai-is-update",
    ] {
        let content = diagnostic_json(&json!({"metadata": {name: "PRIVATE_CREDENTIAL"}}));
        assert!(!content.to_string().contains("PRIVATE_"));
        assert!(content.to_string().contains("<redacted>"));
        let headers = diagnostic_headers([(name, "PRIVATE_CREDENTIAL")]);
        assert!(!headers.to_string().contains("PRIVATE_"));
        assert_eq!(headers[name.to_ascii_lowercase()][0], "<redacted>");
    }
}
