use gateway_plugin_sdk::{
    Capability, Stage,
    call::middleware::{
        MiddlewareBodyDisposition, MiddlewareBodyFrame, MiddlewareBodyFraming,
        MiddlewareBodyHandle, MiddlewareBodyReadResult, MiddlewareHeader, MiddlewareHeaderMutation,
        MiddlewareMount, MiddlewareNextRequest, MiddlewareNextResponse, MiddlewareRequestBody,
        MiddlewareRequestHead, MiddlewareResponseBody, MiddlewareResponseHead, MiddlewareTransport,
    },
};
use serde_json::json;

#[test]
fn middleware_capability_and_mounts_have_one_stable_wire_vocabulary() {
    assert_eq!(
        serde_json::to_value(Capability::Middleware).unwrap(),
        "middleware"
    );
    assert_eq!(serde_json::to_value(Stage::Request).unwrap(), "request");
    assert_eq!(serde_json::to_value(Stage::Attempt).unwrap(), "attempt");
    assert_eq!(
        serde_json::to_value(MiddlewareMount::Request).unwrap(),
        "request"
    );
    assert_eq!(
        serde_json::to_value(MiddlewareMount::Attempt).unwrap(),
        "attempt"
    );
}

#[test]
fn middleware_request_keeps_body_out_of_json_and_rejects_unknown_fields() {
    let request = MiddlewareRequestHead {
        request_id: "request-1".into(),
        mount: MiddlewareMount::Attempt,
        attempt_index: Some(2),
        operation: "generate".into(),
        protocol: "openai".into(),
        endpoint: "responses".into(),
        transport: MiddlewareTransport::HttpSse,
        provider: Some("openai".into()),
        model: Some("gpt-5".into()),
        account_id: Some("account-1".into()),
        headers: vec![MiddlewareHeader {
            name: "x-feature".into(),
            value: b"enabled".to_vec(),
        }],
        body_visible: true,
    };
    let wire = serde_json::to_value(&request).unwrap();
    assert_eq!(wire["mount"], "attempt");
    assert_eq!(wire["transport"], "http_sse");
    assert_eq!(
        wire["headers"][0]["value"],
        json!([101, 110, 97, 98, 108, 101, 100])
    );
    assert!(wire.get("body").is_none());
    assert!(serde_json::from_value::<MiddlewareRequestHead>(wire.clone()).is_ok());

    let mut unknown = wire;
    unknown["credential_revision"] = json!(7);
    assert!(serde_json::from_value::<MiddlewareRequestHead>(unknown).is_err());
}

#[test]
fn next_distinguishes_preserve_from_replace_empty_and_uses_incremental_headers() {
    let preserve = MiddlewareNextRequest {
        protocol: None,
        header_mutations: Vec::new(),
        body: MiddlewareRequestBody::Preserve,
        capabilities: None,
    };
    assert_eq!(
        serde_json::to_value(preserve).unwrap(),
        json!({"header_mutations":[],"body":"preserve"})
    );

    let replace = MiddlewareNextRequest {
        protocol: Some("xai".into()),
        header_mutations: vec![
            MiddlewareHeaderMutation::Remove {
                name: "x-old".into(),
            },
            MiddlewareHeaderMutation::Append {
                name: "x-new".into(),
                value: b"new".to_vec(),
            },
        ],
        body: MiddlewareRequestBody::Replace,
        capabilities: None,
    };
    assert_eq!(
        serde_json::to_value(replace).unwrap(),
        json!({
            "protocol":"xai",
            "header_mutations":[
                {"operation":"remove","name":"x-old"},
                {"operation":"append","name":"x-new","value":[110,101,119]}
            ],
            "body":"replace"
        })
    );
}

#[test]
fn response_body_modes_distinguish_transfer_empty_and_plugin_stream() {
    let handle = MiddlewareBodyHandle {
        handle: "opaque-response-body".into(),
        framing: MiddlewareBodyFraming::SseEvent,
    };
    let next = MiddlewareNextResponse {
        response: "opaque-response".into(),
        protocol: "openai".into(),
        status: 200,
        headers: Vec::new(),
        body: Some(handle.clone()),
    };
    let next_wire = serde_json::to_value(next).unwrap();
    assert_eq!(next_wire["body"]["framing"], "sse_event");
    assert!(next_wire.get("usage").is_none());

    let pass_through = MiddlewareResponseHead {
        response: Some("opaque-response".into()),
        protocol: None,
        status: None,
        header_mutations: Vec::new(),
        body: MiddlewareResponseBody::PassThrough { body: handle },
    };
    assert_eq!(
        serde_json::to_value(pass_through).unwrap()["body"]["kind"],
        "pass_through"
    );
    assert_eq!(
        serde_json::to_value(MiddlewareResponseBody::Empty).unwrap(),
        json!({"kind":"empty"})
    );
    assert_eq!(
        serde_json::to_value(MiddlewareResponseBody::Stream {
            framing: MiddlewareBodyFraming::JsonDocument,
        })
        .unwrap(),
        json!({"kind":"stream","framing":"json_document"})
    );
}

#[test]
fn body_read_metadata_and_binary_frame_keep_raw_bytes_out_of_json() {
    let metadata = MiddlewareBodyReadResult {
        framing: MiddlewareBodyFraming::RawBytes,
        source_id: 9,
        eof: false,
        terminal: true,
    };
    assert_eq!(
        serde_json::to_value(metadata).unwrap(),
        json!({"framing":"raw_bytes","source_id":9,"eof":false,"terminal":true})
    );

    let encoded = MiddlewareBodyFrame::new(vec![0, 255, 10], true).encode();
    assert_eq!(
        &encoded[..14],
        b"GMB1\x00\x00\x00\x00\x00\x00\x00\x00\x00\x01"
    );
    let decoded = MiddlewareBodyFrame::decode(&encoded).unwrap();
    assert_eq!(decoded.payload, vec![0, 255, 10]);
    assert!(decoded.terminal);
    assert_eq!(decoded.source_id(), 0);
    assert_eq!(decoded.disposition(), MiddlewareBodyDisposition::Standalone);
    assert!(MiddlewareBodyFrame::decode(b"GMB1\x06bad").is_err());
    assert!(MiddlewareBodyFrame::decode(b"GMB").is_err());
}
