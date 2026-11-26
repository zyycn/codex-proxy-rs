//! 原生 Messages 生成与计数合同

use gateway_protocol::anthropic::{validate_count_request, validate_request};
use serde_json::json;

#[test]
fn counting_does_not_require_generation_budget() {
    let request = json!({"model":"model","messages":[{"role":"user","content":"hello"}]});
    assert!(validate_count_request(request.as_object().unwrap()).is_ok());
    assert!(validate_request(request.as_object().unwrap()).is_err());
}

#[test]
fn native_validation_retains_roles_and_extensions() {
    let request = json!({"model":"model","max_tokens":1024,"messages":[{"role":"user","content":"hello"},{"role":"system","content":[],"output_config":{"effort":"medium"}}]});
    assert!(validate_request(request.as_object().unwrap()).is_ok());
}
