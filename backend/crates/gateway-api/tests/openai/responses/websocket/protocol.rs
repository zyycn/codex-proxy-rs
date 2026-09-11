use axum::http::{HeaderMap, HeaderValue};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use gateway_api::openai::responses::{OpenAiRequestHeaders, decode_response_create_with_context};
use gateway_core::operation::Operation;
use serde_json::json;

use super::decode_response_create;

#[test]
fn response_create_should_exclude_cloudflare_headers_on_every_frame() {
    let mut headers = HeaderMap::new();
    for (name, value) in [
        ("CF-Visitor", r#"{"scheme":"https"}"#),
        ("cdn-loop", "cloudflare; loops=1"),
        ("cf-warp-tag-id", "downstream-warp"),
        ("cf-ipcountry", "US"),
        ("cf-worker", "worker.example"),
        ("cf-ew-via", "15"),
        ("cf-future-proxy-field", "downstream-only"),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    headers.append("x-openai-future-mode", HeaderValue::from_static("first"));
    headers.append("x-openai-future-mode", HeaderValue::from_static("second"));
    let request_headers = OpenAiRequestHeaders::from_headers(&headers);

    // 连接级头会用于同一 WebSocket 的每一帧，后续帧也不能恢复下游链路元数据。
    for input in ["hello", "continue"] {
        let decoded = decode_response_create_with_context(
            &json!({"type": "response.create", "model": "smart-code", "input": input}).to_string(),
            &request_headers,
        )
        .expect("decode response.create behind Cloudflare");
        let Operation::Generate(request) = decoded.operation() else {
            panic!("Responses must map to Generate");
        };
        assert_eq!(
            request
                .protocol_payload()
                .context()
                .get("opaque_request_headers"),
            Some(&json!([
                ["x-openai-future-mode", STANDARD.encode(b"first")],
                ["x-openai-future-mode", STANDARD.encode(b"second")],
            ])),
        );
        assert_eq!(request.protocol_payload().body()["input"], input);
    }
}

#[test]
fn latest_official_codex_response_create_fixture_decodes_unchanged() {
    // Audited against openai/codex main 94cbbddafc1776d5e377bca1b05932c697e82238.
    let decoded = decode_response_create(
        &json!({
            "type": "response.create",
            "model": "smart-code",
            "input": "hello",
            "store": false
        })
        .to_string(),
    )
    .expect("latest official Codex response.create fixture must decode");
    let Operation::Generate(request) = decoded.operation() else {
        panic!("Responses must map to Generate");
    };

    assert_eq!(
        request.protocol_payload().body(),
        json!({
            "model": "smart-code",
            "input": "hello",
            "store": false
        })
        .as_object()
        .expect("fixture body")
    );
}
