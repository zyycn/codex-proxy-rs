use serde_json::{json, Value};

pub fn openai_error(message: &str, code: &str) -> Value {
    json!({
        "error": {
            "message": message,
            "type": "server_error",
            "param": null,
            "code": code
        }
    })
}
