use gateway_core::operation::Operation;
use serde_json::json;

use super::decode_response_create;

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
