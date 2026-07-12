use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};

pub(crate) fn unsigned_jwt(payload: &Value) -> String {
    let header = json!({"alg": "none", "typ": "JWT"});
    format!("{}.{}.", jwt_part(&header), jwt_part(payload))
}

fn jwt_part(value: &Value) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).expect("test jwt json should encode"))
}
